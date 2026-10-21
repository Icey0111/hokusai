/* libmypaint C reference runner for the hokusai differential gate.
 *
 * Inputs:
 *   brush.txt   one setting/mapping per line, see cref/gen_brush_txt.py
 *   events.txt  "x y pressure xtilt ytilt dtime viewzoom viewrotation barrel [linear]"
 *   out.bin     uint32 width, uint32 height, then RGBA u16 fix15 row-major
 *
 * The sequence mirrors the Rust harness:
 *   new_with_buckets(256) -> from_defaults -> apply settings
 *   begin_atomic -> dt=10 warm-up at first event -> events -> end_atomic
 */

#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "helpers.h"
#include "mypaint-brush.h"
#include "mypaint.h"
#include "mypaint-surface.h"

#include "ref_surface.h"

#ifdef _MSC_VER
#define strtok_r strtok_s
#endif

static int
starts_with(const char *text, const char *prefix) {
    return strncmp(text, prefix, strlen(prefix)) == 0;
}

static int
load_brush(MyPaintBrush *brush, const char *path) {
    FILE *file = fopen(path, "r");
    if (!file) {
        fprintf(stderr, "cannot open brush file: %s\n", path);
        return 0;
    }
    char line[8192];
    int setting_count = 0;
    int mapping_count = 0;
    while (fgets(line, sizeof(line), file)) {
        if (starts_with(line, "#") || starts_with(line, "\n")) {
            continue;
        }
        char *save = NULL;
        char *kind = strtok_r(line, " \t\r\n", &save);
        if (!kind) {
            continue;
        }
        if (strcmp(kind, "B") == 0) {
            char *name = strtok_r(NULL, " \t\r\n", &save);
            char *value_text = strtok_r(NULL, " \t\r\n", &save);
            if (!name || !value_text) {
                fprintf(stderr, "bad B line\n");
                fclose(file);
                return 0;
            }
            const MyPaintBrushSetting setting =
                mypaint_brush_setting_from_cname(name);
            if ((int)setting < 0 || (int)setting >= MYPAINT_BRUSH_SETTINGS_COUNT) {
                continue;
            }
            /* MyPaint's BrushInfo syncs each setting in the file with
             * set_base_value + set_mapping_n for *every* input, so a
             * setting that lists no inputs drops libmypaint's built-in
             * mappings (notably the opaque_multiply pressure curve). */
            for (int input = 0; input < MYPAINT_BRUSH_INPUTS_COUNT; input++) {
                mypaint_brush_set_mapping_n(brush, setting,
                                            (MyPaintBrushInput)input, 0);
            }
            mypaint_brush_set_base_value(brush, setting,
                                         strtof(value_text, NULL));
            setting_count++;
        } else if (strcmp(kind, "M") == 0) {
            char *setting_name = strtok_r(NULL, " \t\r\n", &save);
            char *input_name = strtok_r(NULL, " \t\r\n", &save);
            char *count_text = strtok_r(NULL, " \t\r\n", &save);
            if (!setting_name || !input_name || !count_text) {
                fprintf(stderr, "bad M line\n");
                fclose(file);
                return 0;
            }
            const MyPaintBrushSetting setting =
                mypaint_brush_setting_from_cname(setting_name);
            const MyPaintBrushInput input =
                mypaint_brush_input_from_cname(input_name);
            if ((int)input < 0 || (int)input >= MYPAINT_BRUSH_INPUTS_COUNT) {
                continue;
            }
            const int n = atoi(count_text);
            mypaint_brush_set_mapping_n(brush, setting, input, n);
            for (int i = 0; i < n; i++) {
                char *x_text = strtok_r(NULL, " \t\r\n", &save);
                char *y_text = strtok_r(NULL, " \t\r\n", &save);
                if (!x_text || !y_text) {
                    fprintf(stderr, "bad M point\n");
                    fclose(file);
                    return 0;
                }
                mypaint_brush_set_mapping_point(
                    brush, setting, input, i, strtof(x_text, NULL),
                    strtof(y_text, NULL));
            }
            mapping_count++;
        } else {
            fprintf(stderr, "unknown line kind: %s\n", kind);
            fclose(file);
            return 0;
        }
    }
    fclose(file);
    fprintf(stderr, "loaded settings=%d mappings=%d\n", setting_count,
            mapping_count);
    return 1;
}

/* Python colorsys.hsv_to_rgb, double precision. MyPaint's wrapper runs this
 * in Python (double) before storing the result as C float. */
static void
py_hsv_to_rgb(double h, double s, double v, double *r, double *g, double *b) {
    if (s == 0.0) {
        *r = *g = *b = v;
        return;
    }
    h = h - floor(h);
    h *= 6.0;
    int i = (int)floor(h);
    double f = h - floor(h);
    double p = v * (1.0 - s);
    double q = v * (1.0 - s * f);
    double t = v * (1.0 - s * (1.0 - f));
    switch (i % 6) {
    case 0: *r = v; *g = t; *b = p; break;
    case 1: *r = q; *g = v; *b = p; break;
    case 2: *r = p; *g = v; *b = t; break;
    case 3: *r = p; *g = q; *b = v; break;
    case 4: *r = t; *g = p; *b = v; break;
    default: *r = v; *g = p; *b = q; break;
    }
}

/* Python colorsys.rgb_to_hsv, double precision. */
static void
py_rgb_to_hsv(double r, double g, double b, double *h, double *s, double *v) {
    double maxc = fmax(r, fmax(g, b));
    double minc = fmin(r, fmin(g, b));
    *v = maxc;
    double delta = maxc - minc;
    if (delta == 0.0) {
        *h = 0.0;
        *s = 0.0;
        return;
    }
    *s = delta / maxc;
    if (r == maxc) {
        *h = (g - b) / delta;
    } else if (g == maxc) {
        *h = 2.0 + (b - r) / delta;
    } else {
        *h = 4.0 + (r - g) / delta;
    }
    *h /= 6.0;
    if (*h < 0.0) {
        *h += 1.0;
    }
}

/* MyPaint's Python wrapper converts color_h/s/v to linear sRGB before the
 * engine sees them (lib/brush.py:651-682, helpers.transform_hsv). The C
 * library itself only delinearizes/linearizes around HSV/HSL dynamics. */
static void
apply_mypaint_color_transform(MyPaintBrush *brush) {
    double h = mypaint_brush_get_base_value(brush, MYPAINT_BRUSH_SETTING_COLOR_H);
    double s = mypaint_brush_get_base_value(brush, MYPAINT_BRUSH_SETTING_COLOR_S);
    double v = mypaint_brush_get_base_value(brush, MYPAINT_BRUSH_SETTING_COLOR_V);
    double r, g, b;
    py_hsv_to_rgb(h, s, v, &r, &g, &b);
    r = pow(r, 2.2);
    g = pow(g, 2.2);
    b = pow(b, 2.2);
    py_rgb_to_hsv(r, g, b, &h, &s, &v);
    mypaint_brush_set_base_value(brush, MYPAINT_BRUSH_SETTING_COLOR_H,
                                 (float)h);
    mypaint_brush_set_base_value(brush, MYPAINT_BRUSH_SETTING_COLOR_S,
                                 (float)s);
    mypaint_brush_set_base_value(brush, MYPAINT_BRUSH_SETTING_COLOR_V,
                                 (float)v);
}

static int
dump_fix15(const char *path, RefSurface *surface, int width, int height) {
    FILE *file = fopen(path, "wb");
    if (!file) {
        fprintf(stderr, "cannot open output: %s\n", path);
        return 0;
    }
    const uint32_t header[2] = {(uint32_t)width, (uint32_t)height};
    if (fwrite(header, sizeof(header), 1, file) != 1) {
        fclose(file);
        return 0;
    }
    uint16_t *pixels = (uint16_t *)malloc(
        (size_t)width * (size_t)height * 4 * sizeof(uint16_t));
    if (!pixels) {
        fclose(file);
        return 0;
    }
    ref_surface_dump(surface, pixels);
    const size_t count = (size_t)width * (size_t)height * 4;
    const int ok = fwrite(pixels, sizeof(uint16_t), count, file) == count;
    free(pixels);
    fclose(file);
    return ok;
}

static void
trace_state(const char *label, MyPaintBrush *brush) {
    if (!getenv("CREF_TRACE_STATE")) {
        return;
    }
    printf("%s", label);
    for (int i = 0; i < MYPAINT_BRUSH_STATES_COUNT; i++) {
        union {
            float f;
            uint32_t u;
        } v;
        v.f = mypaint_brush_get_state(brush, i);
        printf(" %08x", v.u);
    }
    printf("\n");
}

int
main(int argc, char **argv) {
    if (argc != 7) {
        fprintf(stderr,
                "usage: %s brush.txt events.txt out.bin width height linear\n",
                argv[0]);
        return 2;
    }
    const int width = atoi(argv[4]);
    const int height = atoi(argv[5]);
    const int linear = atoi(argv[6]) != 0;
    if (width <= 0 || height <= 0) {
        fprintf(stderr, "bad size\n");
        return 2;
    }

    mypaint_init();
    MyPaintBrush *brush = mypaint_brush_new_with_buckets(256);
    if (!brush) {
        fprintf(stderr, "brush alloc failed\n");
        return 1;
    }
    mypaint_brush_from_defaults(brush);
    if (!load_brush(brush, argv[1])) {
        mypaint_brush_unref(brush);
        return 1;
    }
    if (linear) {
        apply_mypaint_color_transform(brush);
    }

    FILE *events = fopen(argv[2], "r");
    if (!events) {
        fprintf(stderr, "cannot open events: %s\n", argv[2]);
        mypaint_brush_unref(brush);
        return 1;
    }

    RefSurface *surface = ref_surface_new(width, height);
    if (!surface) {
        fprintf(stderr, "surface alloc failed\n");
        fclose(events);
        mypaint_brush_unref(brush);
        return 1;
    }
    MyPaintSurface *iface = ref_surface_interface(surface);

    char line[4096];
    float first_x = 0.0f;
    float first_y = 0.0f;
    int have_first = 0;
    int event_count = 0;
    mypaint_surface_begin_atomic(iface);
    while (fgets(line, sizeof(line), events)) {
        if (starts_with(line, "#") || starts_with(line, "\n")) {
            continue;
        }
        float x, y, pressure, xtilt, ytilt, viewzoom, viewrotation, barrel;
        double dtime;
        int event_linear = linear;
        const int n = sscanf(line,
                             "%f %f %f %f %f %lf %f %f %f %d",
                             &x, &y, &pressure, &xtilt, &ytilt, &dtime,
                             &viewzoom, &viewrotation, &barrel,
                             &event_linear);
        if (n < 9) {
            fprintf(stderr, "bad event line (parsed %d): %s\n", n, line);
            fclose(events);
            ref_surface_free(surface);
            mypaint_brush_unref(brush);
            return 1;
        }
        if (!have_first) {
            first_x = x;
            first_y = y;
            have_first = 1;
            mypaint_brush_stroke_to(brush, iface, first_x, first_y, 0.0f,
                                    0.0f, 0.0f, 10.0, 1.0f, 0.0f, 0.0f,
                                    TRUE);
            trace_state("W", brush);
        }
        mypaint_brush_stroke_to(brush, iface, x, y, pressure, xtilt, ytilt,
                                dtime, viewzoom, viewrotation, barrel,
                                event_linear ? TRUE : FALSE);
        char label[16];
        snprintf(label, sizeof(label), "E%d", event_count);
        trace_state(label, brush);
        event_count++;
    }
    fclose(events);
    mypaint_surface_end_atomic(iface, NULL);

    fprintf(stderr, "rendered %d events\n", event_count);
    const int ok = dump_fix15(argv[3], surface, width, height);
    ref_surface_free(surface);
    mypaint_brush_unref(brush);
    return ok ? 0 : 1;
}

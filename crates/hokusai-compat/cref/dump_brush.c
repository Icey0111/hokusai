#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "mypaint-brush.h"
#include "mypaint-brush-settings.h"

static int
starts_with(const char *text, const char *prefix) {
    return strncmp(text, prefix, strlen(prefix)) == 0;
}

static int
load_brush(MyPaintBrush *brush, const char *path) {
    FILE *file = fopen(path, "r");
    if (!file) {
        return 0;
    }
    char line[8192];
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
            char *value = strtok_r(NULL, " \t\r\n", &save);
            mypaint_brush_set_base_value(
                brush, mypaint_brush_setting_from_cname(name),
                strtof(value, NULL));
        } else if (strcmp(kind, "M") == 0) {
            char *setting_name = strtok_r(NULL, " \t\r\n", &save);
            char *input_name = strtok_r(NULL, " \t\r\n", &save);
            char *count_text = strtok_r(NULL, " \t\r\n", &save);
            const MyPaintBrushSetting setting =
                mypaint_brush_setting_from_cname(setting_name);
            const MyPaintBrushInput input =
                mypaint_brush_input_from_cname(input_name);
            const int n = atoi(count_text);
            mypaint_brush_set_mapping_n(brush, setting, input, n);
            for (int i = 0; i < n; i++) {
                char *x = strtok_r(NULL, " \t\r\n", &save);
                char *y = strtok_r(NULL, " \t\r\n", &save);
                mypaint_brush_set_mapping_point(brush, setting, input, i,
                                                strtof(x, NULL),
                                                strtof(y, NULL));
            }
        }
    }
    fclose(file);
    return 1;
}

int
main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: %s brush.txt\n", argv[0]);
        return 2;
    }
    MyPaintBrush *brush = mypaint_brush_new_with_buckets(256);
    mypaint_brush_from_defaults(brush);
    if (!load_brush(brush, argv[1])) {
        return 1;
    }
    for (int s = 0; s < MYPAINT_BRUSH_SETTINGS_COUNT; s++) {
        const MyPaintBrushSettingInfo *info = mypaint_brush_setting_info(s);
        printf("B %s %.9g\n", info->cname,
               mypaint_brush_get_base_value(brush, s));
        for (int i = 0; i < MYPAINT_BRUSH_INPUTS_COUNT; i++) {
            const int n = mypaint_brush_get_mapping_n(brush, s, i);
            if (n <= 0) {
                continue;
            }
            printf("M %s %s %d", info->cname,
                   mypaint_brush_input_info(i)->cname, n);
            for (int p = 0; p < n; p++) {
                float x, y;
                mypaint_brush_get_mapping_point(brush, s, i, p, &x, &y);
                printf(" %.9g %.9g", x, y);
            }
            printf("\n");
        }
    }
    mypaint_brush_unref(brush);
    return 0;
}

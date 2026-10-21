/* libmypaint - The MyPaint Brush Library
 * Copyright (C) 2008 Martin Renold <martinxyz@gmx.ch>
 * Copyright (C) 2012 Jon Nordby <jononor@gmail.com>
 *
 * Permission to use, copy, modify, and/or distribute this software for any
 * purpose with or without fee is hereby granted, provided that the above
 * copyright notice and this permission notice appear in all copies.
 *
 * THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
 * WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
 * MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
 * ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
 * WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
 * ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
 * OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
 */

#include "config.h"

#include <assert.h>

#include "mypaint-surface.h"
#include <stdio.h>
#include <stdlib.h>

#include "helpers.h"

int
mypaint_surface_draw_dab(MyPaintSurface *self,
                       float x, float y,
                       float radius,
                       float color_r, float color_g, float color_b,
                       float opaque, float hardness, float softness,
                       float alpha_eraser,
                       float aspect_ratio, float angle,
                       float lock_alpha,
                       float colorize,
                       float posterize,
                       float posterize_num,
                       float paint
                       )
{
    assert(self->draw_dab);
    if (getenv("CREF_TRACE_DABARGS")) {
      static long cref_dab_seq = 0;
      cref_dab_seq++;
      union { float f; unsigned u; } ax, ay, ar, acr, acg, acb, aop, aha, aso, aae, aas, aan, ala, aci, apo, apn, apa;
      ax.f=x; ay.f=y; ar.f=radius; acr.f=color_r; acg.f=color_g; acb.f=color_b;
      aop.f=opaque; aha.f=hardness; aso.f=softness; aae.f=alpha_eraser;
      aas.f=aspect_ratio; aan.f=angle; ala.f=lock_alpha; aci.f=colorize;
      apo.f=posterize; apn.f=posterize_num; apa.f=paint;
      fprintf(stderr, "DAB n=%ld x=%08x y=%08x r=%08x cr=%08x cg=%08x cb=%08x op=%08x ha=%08x so=%08x ae=%08x as=%08x an=%08x la=%08x co=%08x po=%08x pn=%08x pa=%08x\n",
        cref_dab_seq, ax.u, ay.u, ar.u, acr.u, acg.u, acb.u, aop.u, aha.u, aso.u, aae.u, aas.u, aan.u, ala.u, aci.u, apo.u, apn.u, apa.u);
    }
    return self->draw_dab(self, x, y, radius, color_r, color_g, color_b,
                   opaque, hardness, softness, alpha_eraser, aspect_ratio, angle,
                   lock_alpha, colorize, posterize, posterize_num, paint);
}


void
mypaint_surface_get_color(MyPaintSurface *self,
                        float x, float y,
                        float radius,
                        float * color_r, float * color_g, float * color_b, float * color_a,
                        float paint
                        )
{
    assert(self->get_color);
    self->get_color(self, x, y, radius, color_r, color_g, color_b, color_a, paint);
    if (getenv("CREF_TRACE_DABARGS")) {
      union { float f; unsigned u; } ax, ay, ar, apa, oa, orr, og, ob;
      ax.f=x; ay.f=y; ar.f=radius; apa.f=paint; oa.f=*color_a; orr.f=*color_r; og.f=*color_g; ob.f=*color_b;
      fprintf(stderr, "GET x=%08x y=%08x r=%08x pa=%08x ro=%08x go=%08x bo=%08x ao=%08x\n",
        ax.u, ay.u, ar.u, apa.u, orr.u, og.u, ob.u, oa.u);
    }
}


/**
 * mypaint_surface_init: (skip)
 *
 * Initialize the surface. The reference count will be set to 1.
 * Note: Only intended to be called from subclasses of #MyPaintSurface
 **/
void
mypaint_surface_init(MyPaintSurface *self)
{
    self->refcount = 1;
}

/**
 * mypaint_surface_ref: (skip)
 *
 * Increase the reference count.
 **/
void
mypaint_surface_ref(MyPaintSurface *self)
{
    self->refcount++;
}

/**
 * mypaint_surface_unref: (skip)
 *
 * Decrease the reference count.
 **/
void
mypaint_surface_unref(MyPaintSurface *self)
{
    self->refcount--;
    if (self->refcount == 0) {
        assert(self->destroy);
        self->destroy(self);
    }
}

float mypaint_surface_get_alpha (MyPaintSurface *self, float x, float y, float radius)
{
    float color_r, color_g, color_b, color_a;
    mypaint_surface_get_color (self, x, y, radius, &color_r, &color_g, &color_b, &color_a, 1.0);
    return color_a;
}

void
mypaint_surface_save_png(MyPaintSurface *self, const char *path, int x, int y, int width, int height)
{
    if (self->save_png) {
        self->save_png(self, path, x, y, width, height);
    }
}

void
mypaint_surface_begin_atomic(MyPaintSurface *self)
{
    if (self->begin_atomic)
        self->begin_atomic(self);
}

/**
 * mypaint_surface_end_atomic:
 * @roi: (out) (allow-none) (transfer none): Invalidated rectangles will be stored here.
 * The value of roi->num_rectangles must be at least 1, and roi->rectangles must point to
 * sufficient accessible memory to contain n = roi->num_rectangles of MyPaintRectangle structs.
 *
 * Returns: s
 */
void
mypaint_surface_end_atomic(MyPaintSurface *self, MyPaintRectangles *roi)
{
    assert(self->end_atomic);
    self->end_atomic(self, roi);
}

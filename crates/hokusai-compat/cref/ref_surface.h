#ifndef MYPAINT_CREF_SURFACE_H
#define MYPAINT_CREF_SURFACE_H

#include <stdint.h>

#include "mypaint-tiled-surface.h"

typedef struct RefSurface RefSurface;

RefSurface *ref_surface_new(int width, int height);
void ref_surface_free(RefSurface *self);
MyPaintSurface *ref_surface_interface(RefSurface *self);

/* Copy the visible 0..width/0..height area into out as RGBA u16 fix15,
 * row-major, 4 components per pixel. */
void ref_surface_dump(RefSurface *self, uint16_t *out);

#endif

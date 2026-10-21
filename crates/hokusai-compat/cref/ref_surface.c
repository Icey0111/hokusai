#include "ref_surface.h"

#include <stdlib.h>
#include <string.h>

#include "mypaint-surface.h"

#define TILE_PIXELS (MYPAINT_TILE_SIZE * MYPAINT_TILE_SIZE)
#define TILE_VALUES (TILE_PIXELS * 4)

struct RefSurface {
    MyPaintTiledSurface parent;
    int width;
    int height;
    int tiles_x;
    int tiles_y;
    uint16_t *tiles;
    uint16_t *null_tile;
};

static void
tile_request_start(MyPaintTiledSurface *surface, MyPaintTileRequest *request) {
    RefSurface *self = (RefSurface *)surface;
    if (request->tx < 0 || request->ty < 0 ||
        request->tx >= self->tiles_x || request->ty >= self->tiles_y) {
        request->buffer = self->null_tile;
        return;
    }
    const size_t index =
        ((size_t)request->ty * (size_t)self->tiles_x + (size_t)request->tx) *
        TILE_VALUES;
    request->buffer = self->tiles + index;
}

static void
tile_request_end(MyPaintTiledSurface *surface, MyPaintTileRequest *request) {
    (void)surface;
    (void)request;
}

RefSurface *
ref_surface_new(int width, int height) {
    RefSurface *self = (RefSurface *)calloc(1, sizeof(RefSurface));
    if (!self) {
        return NULL;
    }
    self->width = width;
    self->height = height;
    self->tiles_x = (width + MYPAINT_TILE_SIZE - 1) / MYPAINT_TILE_SIZE;
    self->tiles_y = (height + MYPAINT_TILE_SIZE - 1) / MYPAINT_TILE_SIZE;
    self->tiles = (uint16_t *)calloc(
        (size_t)self->tiles_x * (size_t)self->tiles_y * TILE_VALUES,
        sizeof(uint16_t));
    self->null_tile = (uint16_t *)calloc(TILE_VALUES, sizeof(uint16_t));
    if (!self->tiles || !self->null_tile) {
        free(self->tiles);
        free(self->null_tile);
        free(self);
        return NULL;
    }
    mypaint_tiled_surface_init(&self->parent, tile_request_start,
                               tile_request_end);
    return self;
}

void
ref_surface_free(RefSurface *self) {
    if (!self) {
        return;
    }
    mypaint_tiled_surface_destroy(&self->parent);
    free(self->tiles);
    free(self->null_tile);
    free(self);
}

MyPaintSurface *
ref_surface_interface(RefSurface *self) {
    return &self->parent.parent;
}

void
ref_surface_dump(RefSurface *self, uint16_t *out) {
    const size_t row_values = (size_t)self->width * 4;
    memset(out, 0, row_values * (size_t)self->height * sizeof(uint16_t));
    for (int ty = 0; ty < self->tiles_y; ty++) {
        for (int tx = 0; tx < self->tiles_x; tx++) {
            const size_t tile_index =
                ((size_t)ty * (size_t)self->tiles_x + (size_t)tx) * TILE_VALUES;
            const uint16_t *tile = self->tiles + tile_index;
            const int x0 = tx * MYPAINT_TILE_SIZE;
            const int y0 = ty * MYPAINT_TILE_SIZE;
            const int w = (x0 + MYPAINT_TILE_SIZE <= self->width)
                              ? MYPAINT_TILE_SIZE
                              : (self->width - x0);
            const int h = (y0 + MYPAINT_TILE_SIZE <= self->height)
                              ? MYPAINT_TILE_SIZE
                              : (self->height - y0);
            for (int y = 0; y < h; y++) {
                const uint16_t *src =
                    tile + (size_t)y * MYPAINT_TILE_SIZE * 4;
                uint16_t *dst =
                    out + ((size_t)(y0 + y) * (size_t)self->width + x0) * 4;
                memcpy(dst, src, (size_t)w * 4 * sizeof(uint16_t));
            }
        }
    }
}

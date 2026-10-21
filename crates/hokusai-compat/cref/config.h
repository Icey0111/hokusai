#ifndef MYPAINT_CREF_CONFIG_H
#define MYPAINT_CREF_CONFIG_H

/* Minimal config for the C reference harness.
 * GLib is not required; JSON is replaced by cref/json.h stubs because the
 * runner sets settings through the public C API instead of parsing .myb. */
#define MYPAINT_CONFIG_USE_GLIB 0
#define MYPAINT_TILE_SIZE 64
#define MYPAINT_MAX_THREADS 1
#define MYPAINT_MAX_MIPMAP_LEVEL 4

#endif

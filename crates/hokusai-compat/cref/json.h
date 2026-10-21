#ifndef MYPAINT_CREF_JSON_H
#define MYPAINT_CREF_JSON_H

/* libmypaint only needs json-c for brush (de)serialization. The reference
 * runner builds brushes through mypaint_brush_set_* instead, so these stubs
 * just keep the unused serialization code linkable. */

typedef struct json_object {
    int unused;
} json_object;

typedef enum {
    json_type_null,
    json_type_boolean,
    json_type_double,
    json_type_int,
    json_type_object,
    json_type_array,
    json_type_string
} json_type;

#define JSON_C_MINOR_VERSION 10

static inline json_object *json_object_new_object(void) {
    static json_object object;
    return &object;
}

static inline void json_object_put(json_object *obj) { (void)obj; }

static inline int
json_object_is_type(const json_object *obj, json_type type) {
    (void)obj;
    (void)type;
    return 0;
}

static inline json_object *
json_object_object_get(const json_object *obj, const char *key) {
    (void)obj;
    (void)key;
    return 0;
}

static inline int
json_object_object_get_ex(const json_object *obj, const char *key,
                          json_object **out) {
    (void)obj;
    (void)key;
    if (out) {
        *out = 0;
    }
    return 0;
}

static inline double json_object_get_double(const json_object *obj) {
    (void)obj;
    return 0.0;
}

static inline int json_object_get_int(const json_object *obj) {
    (void)obj;
    return 0;
}

static inline int json_object_array_length(const json_object *obj) {
    (void)obj;
    return 0;
}

static inline json_object *
json_object_array_get_idx(const json_object *obj, int index) {
    (void)obj;
    (void)index;
    return 0;
}

static inline json_object *json_tokener_parse(const char *text) {
    (void)text;
    return 0;
}

static inline const char *
json_cref_foreach_key(const json_object *obj) {
    (void)obj;
    return 0;
}

#define json_object_object_foreach(obj, key, val)                       \
    for (char *key = (char *)json_cref_foreach_key(obj); key != 0;      \
         key = 0)                                                       \
        for (json_object *val = 0; val != 0; val = 0)

#endif

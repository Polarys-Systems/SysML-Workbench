/* Compiles stb_truetype (v1.26, public domain) plus a tiny shim so Rust never
 * needs to know the layout of stbtt_fontinfo. Built by build.rs via `cc`. */
#include <stdlib.h>

#define STB_TRUETYPE_IMPLEMENTATION
#include "stb_truetype.h"

/* Returns a heap-allocated font info for face `index` of `data`, or NULL.
 * `data` must stay alive and unchanged for the lifetime of the result.
 * stb_truetype does not bounds-check reads: only pass trusted font files. */
stbtt_fontinfo *ev_stbtt_create(const unsigned char *data, int len, int index) {
    if (!data || len < 12 || index < 0) return NULL;
    int offset = stbtt_GetFontOffsetForIndex(data, index);
    if (offset < 0 || offset >= len) return NULL;
    stbtt_fontinfo *info = (stbtt_fontinfo *)malloc(sizeof(stbtt_fontinfo));
    if (!info) return NULL;
    if (!stbtt_InitFont(info, data, offset)) {
        free(info);
        return NULL;
    }
    return info;
}

void ev_stbtt_destroy(stbtt_fontinfo *info) { free(info); }

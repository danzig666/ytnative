#ifndef PLX_ASS_H
#define PLX_ASS_H
/* ytnative: the UI text half of plx-native's private libass facade (include/ass.h at 31e6f24),
 * built alone over the television's own FreeType, HarfBuzz and FriBidi (see README.md here). */
#include <stddef.h>
#include <stdint.h>

/* The facade ABI the application's `rtl` module checks for (`NATIVE_ABI`). */
#define PLX_ASS_ABI_VERSION 5
uint32_t plx_ass_abi_version(void);

/* ---- UI text (ABI 4) ------------------------------------------------------------------------
 * The application draws its own text with the television's SDL2_ttf 2.0.x: one face per call, a
 * left-to-right advance loop keyed on codepoints, no shaper. Arabic needs contextual joining and
 * Hebrew and Arabic both need right-to-left order, so the runs of those scripts are shaped here,
 * with the same pinned FriBidi, HarfBuzz and FreeType that already shape the subtitles. Every
 * other run stays on SDL2_ttf. One thread (the application's main thread) owns every handle. */
typedef struct PlxText PlxText;

typedef struct {
    int width;            /* box width in px: the advance, grown by any ink overhanging either end */
    int ascent;           /* rows from the box's top to the baseline: the ink's height above it */
    int height;           /* rows in the box: exactly the ink's, so 0 for a run of spaces */
    const uint8_t *alpha; /* width*height coverage, row-major, tightly packed. NULL from
                           * plx_ass_text_measure; from plx_ass_text_render, valid until the next
                           * call on the same handle or its close */
} PlxTextRun;

/* A face at `px` pixels per em (SDL_ttf's point size, which is pixels at its fixed 72 dpi).
 * NULL if the file will not open or `px` is outside 1..512. */
PlxText *plx_ass_text_open(const char *path, int px);
void plx_ass_text_close(PlxText *text);
/* Shape `len` bytes of UTF-8, one directional run in LOGICAL order (rtl: 1 right-to-left, 0
 * left-to-right), and report its box. measure and render lay the run out through the same code,
 * so a render's width is exactly its measure's. -1 if refused (bad arguments, a run over 4096
 * bytes, a box over the pixel cap), 0 on success. */
int plx_ass_text_measure(PlxText *text, const char *utf8, size_t len, int rtl, PlxTextRun *out);
int plx_ass_text_render(PlxText *text, const char *utf8, size_t len, int rtl, PlxTextRun *out);
/* The Unicode Bidirectional Algorithm (FriBidi) over ONE line of `n` codepoints, its paragraph
 * direction taken from the first strong character (left-to-right when there is none). Fills
 * `levels` with each character's resolved level after rule L1, `visual` with the logical index
 * drawn at each visual position (rule L2, left to right, combining marks NOT moved after their
 * base), and `mirrored` with each character's Bidi_Mirroring_Glyph where its level is odd and it
 * has one, else the character itself. Returns 1 if any level is odd, 0 if none, -1 if refused
 * (n outside 1..16384, a NULL array, or FriBidi failing). */
int plx_ass_text_bidi(const uint32_t *chars, int n, uint8_t *levels, int32_t *visual,
                      uint32_t *mirrored);

#endif

/* lv_conf.h: how LVGL 9.5 is configured for luyaz-ui, the Luya-Z touch UI.
 * Consumers: rs/lvgl-sys/build.rs compiles LVGL with it and rs/lvgl-sys/regen.sh hands it to
 * bindgen. Settings left out here take LVGL's defaults from lv_conf_internal.h. After editing,
 * rerun rs/lvgl-sys/regen.sh to refresh the committed bindings (`--check` tells whether they
 * match). Do not reorder the defines: bindgen writes them out in this order.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 */

#if !defined(LV_CONF_H)
#define LV_CONF_H /* include guard, empty on purpose */
#include <stdint.h> /* LVGL expects the fixed-width ints here */

/* == Display, threading, helpers, logging, renderer == */
#define LV_COLOR_DEPTH 16 /* the 320 x 480 DRM buffer is RGB565 */
#define LV_USE_OS LV_OS_NONE /* single thread, we run the main loop */

/* libc supplies malloc and the string/printf helpers. The UI is a normal Linux process with RAM
 * to spare, and LVGL's own fixed heap ran dry in real use; hence no LV_MEM_SIZE either. */
#define LV_USE_STDLIB_MALLOC  LV_STDLIB_CLIB /* malloc/free */
#define LV_USE_STDLIB_STRING  LV_STDLIB_CLIB /* memcpy, strlen, ... */
#define LV_USE_STDLIB_SPRINTF LV_STDLIB_CLIB /* lv_snprintf */
#define LV_USE_LOG 1 /* warnings and errors ... */
#define LV_LOG_PRINTF 1 /* ... printed to stderr, i.e. the UI log */
#define LV_LOG_LEVEL LV_LOG_LEVEL_WARN /* nothing below warnings */
#define LV_USE_DRAW_SW 1 /* software rendering, no GPU */
#define LV_DEF_REFR_PERIOD 20 /* ms; shorter than the default so motion looks smoother */
#define LV_USE_COLOR_FILTER 1 /* kit::tappable's immediate light/dark pressed feedback */

/* == Fonts: the Montserrat set is compiled in, used when no TTF loads == */
#define LV_FONT_MONTSERRAT_12 1 /* fallback 12 px */
#define LV_FONT_MONTSERRAT_14 1 /* fallback 14 px */
#define LV_FONT_MONTSERRAT_16 1 /* fallback 16 px, also the default */
#define LV_FONT_MONTSERRAT_20 1 /* fallback 20 px */
#define LV_FONT_MONTSERRAT_28 1 /* fallback 28 px */
#define LV_FONT_DEFAULT &lv_font_montserrat_16 /* default font */

/* == Extras == */
#define LV_USE_QRCODE 1 /* Wi-Fi sharing QR code */

/* TTF fonts loaded at run time (Chinese text, Nunito digits) go through FreeType; the files are
 * read with plain stdio rather than LVGL's file-system layer. */
#define LV_USE_FREETYPE 1 /* TTF rendering */
#define LV_FREETYPE_USE_LVGL_PORT 0 /* stdio, not lv_fs */
#define LV_FREETYPE_CACHE_FT_GLYPH_CNT 256 /* glyphs cached */

/* Operator logos are PNGs in /data/luyaz/ui/operator-logos/, opened as "A:<absolute path>" via the
 * POSIX driver and decoded with LodePNG; the image cache holds decoded logos so redraws are cheap. */
#define LV_USE_FS_POSIX 1 /* POSIX file driver */
#define LV_FS_POSIX_LETTER 'A' /* drive letter A: */
#define LV_FS_POSIX_PATH "" /* paths are absolute */
#define LV_USE_LODEPNG 1 /* PNG decoder */
#define LV_CACHE_DEF_SIZE (64 * 1024) /* bytes of decoded images kept */

/* == Left out of the build == */
#define LV_USE_DEMO_WIDGETS 0 /* no demo */
#define LV_BUILD_EXAMPLES 0 /* no examples */

#endif /* !defined(LV_CONF_H) */

/*
 * LVGL 9.5 settings for the Luya-Z touch UI (luyaz-ui).
 *
 * Read by rs/lvgl-sys (build.rs compiles LVGL against it; regen.sh feeds it
 * to bindgen). Anything not set here keeps LVGL's default from
 * lv_conf_internal.h. If you change a setting, rerun rs/lvgl-sys/regen.sh so
 * the committed bindings stay in step (--check verifies). Keep the order of
 * the defines: bindgen emits them in that order.
 *
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */
#ifndef LV_CONF_H
#define LV_CONF_H

#include <stdint.h>

/* ---- display and runtime ---------------------------------------------- */

/* The panel's DRM buffer is RGB565 (320 x 480), so draw in 16-bit. */
#define LV_COLOR_DEPTH 16

/* One thread, our own main loop. */
#define LV_USE_OS LV_OS_NONE

/* Memory and string helpers come from libc. The UI is an ordinary Linux
 * process with plenty of RAM; LVGL's built-in fixed-size heap once ran out
 * under real use, so it is not used (and LV_MEM_SIZE is therefore unset). */
#define LV_USE_STDLIB_MALLOC  LV_STDLIB_CLIB
#define LV_USE_STDLIB_STRING  LV_STDLIB_CLIB
#define LV_USE_STDLIB_SPRINTF LV_STDLIB_CLIB

/* Warnings and errors go to stderr (the UI log). */
#define LV_USE_LOG   1
#define LV_LOG_PRINTF 1
#define LV_LOG_LEVEL LV_LOG_LEVEL_WARN

/* CPU rendering only. */
#define LV_USE_DRAW_SW 1

/* 20 ms refresh period instead of LVGL's default, for smoother motion. */
#define LV_DEF_REFR_PERIOD 20

/* ---- fonts ------------------------------------------------------------ */

/* Built-in Montserrat sizes: the last fallback when no TTF can be loaded. */
#define LV_FONT_MONTSERRAT_12 1
#define LV_FONT_MONTSERRAT_14 1
#define LV_FONT_MONTSERRAT_16 1
#define LV_FONT_MONTSERRAT_20 1
#define LV_FONT_MONTSERRAT_28 1
#define LV_FONT_DEFAULT &lv_font_montserrat_16

/* ---- widgets and libraries -------------------------------------------- */

/* QR codes (Wi-Fi sharing). */
#define LV_USE_QRCODE 1

/* FreeType renders the TTF fonts loaded at run time (Chinese text, Nunito
 * numerals); the files are opened with plain stdio, not LVGL's fs layer. */
#define LV_USE_FREETYPE 1
#define LV_FREETYPE_USE_LVGL_PORT 0
#define LV_FREETYPE_CACHE_FT_GLYPH_CNT 256

/* Operator logos: PNG files under /data/luyaz/ui/operator-logos/, opened as
 * "A:<absolute path>" through the POSIX driver and decoded by LodePNG. The
 * image cache keeps decoded logos so a redraw does not decode again. */
#define LV_USE_FS_POSIX 1
#define LV_FS_POSIX_LETTER 'A'
#define LV_FS_POSIX_PATH ""
#define LV_USE_LODEPNG 1
#define LV_CACHE_DEF_SIZE (64 * 1024)

/* ---- not built -------------------------------------------------------- */

#define LV_USE_DEMO_WIDGETS 0
#define LV_BUILD_EXAMPLES   0

#endif /* LV_CONF_H */

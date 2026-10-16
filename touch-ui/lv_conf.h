/*
 * lv_conf.h - LVGL v9 configuration for u60pro-devui.
 *
 * Only options that differ from LVGL's built-in defaults are set here; every
 * other option falls back to lv_conf_internal.h. Targets a 320x480 RGB565
 * command-mode panel with no RTOS.
 *
 * SPDX-License-Identifier: MIT
 */
#ifndef LV_CONF_H
#define LV_CONF_H

#include <stdint.h>

/* RGB565 matches the panel's dumb buffer exactly. */
#define LV_COLOR_DEPTH 16

/* Bare-metal main loop, no OS. */
#define LV_USE_OS LV_OS_NONE

/* Real system allocator, not LVGL's own small fixed-size arena. This project
 * originally used LV_STDLIB_BUILTIN ("self-contained heap so we don't depend
 * on libc malloc") — that reasoning is for bare-metal/RTOS targets with a
 * genuinely fixed SRAM budget. This binary is a normal Linux process with
 * 690MB+ free system RAM; htmlmain.c (the other renderer in this repo)
 * already uses plain malloc() successfully. Confining every LVGL widget,
 * glyph bitmap, and draw buffer into a small carved-out pool (was 512KB,
 * then 2MB) was the actual root cause of a recurring heap-exhaustion crash
 * under real-device use (see [[lvgl-heap-instability]]) — no amount of
 * reducing churn inside that pool fixes the fact that it's artificially
 * small against a system that isn't. LV_MEM_SIZE only applies to
 * LV_STDLIB_BUILTIN, so it's removed, not just left unused. */
#define LV_USE_STDLIB_MALLOC  LV_STDLIB_CLIB
#define LV_USE_STDLIB_STRING  LV_STDLIB_CLIB
#define LV_USE_STDLIB_SPRINTF LV_STDLIB_CLIB

/* Logging to stderr during development; flip to 0 for production. */
#define LV_USE_LOG   1
#define LV_LOG_PRINTF 1
#define LV_LOG_LEVEL LV_LOG_LEVEL_WARN

/* Software renderer (no GPU). */
#define LV_USE_DRAW_SW 1

/* Refresh/input cadence (ms) — snappier animations on this slow panel. */
#define LV_DEF_REFR_PERIOD 20

/* Fonts we use in the demo UI. */
#define LV_FONT_MONTSERRAT_12 1
#define LV_FONT_MONTSERRAT_14 1
#define LV_FONT_MONTSERRAT_16 1
#define LV_FONT_MONTSERRAT_20 1
#define LV_FONT_MONTSERRAT_28 1
#define LV_FONT_DEFAULT &lv_font_montserrat_16

/* QR code widget for the WiFi share page. */
#define LV_USE_QRCODE 1

/* FreeType for CJK (Chinese) text — loads a TTF from the device at runtime. */
#define LV_USE_FREETYPE 1
#define LV_FREETYPE_USE_LVGL_PORT 0
#define LV_FREETYPE_CACHE_FT_GLYPH_CNT 256

/* Operator logo on the home card (2026-09-26): transparent PNGs read from
 * /data/plugins/u60pro-devui/operator-logos/ through the POSIX fs driver
 * ("A:/abs/path"). The cache keeps decoded logos (~3.5 KB each) so a
 * redraw does not re-decode; nothing else in the UI uses images. */
#define LV_USE_FS_POSIX 1
#define LV_FS_POSIX_LETTER 'A'
#define LV_FS_POSIX_PATH ""
#define LV_USE_LODEPNG 1
#define LV_CACHE_DEF_SIZE (64 * 1024)

/* No bundled demos/examples in the binary. */
#define LV_USE_DEMO_WIDGETS 0
#define LV_BUILD_EXAMPLES   0

#endif /* LV_CONF_H */

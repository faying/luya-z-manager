//! LVGL 9.5 (+ patches/lvgl-v9.5.0-freetype-bold.patch, the touch UI's lv_conf.h, FreeType
//! 2.13.3) as raw bindings. `bindings.rs` and `csrc/static_fns.c` are bindgen's output,
//! committed (the build image has no libclang); `regen.sh` makes them again and `regen.sh
//! --check` fails when they no longer match lv_conf.h and the LVGL sources.
#![no_std]
#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case, dead_code, unnecessary_transmutes, clippy::all)]

include!("bindings.rs");

//! LVGL 9.5 with our patch and lv_conf.h, the static-inline wrappers, and a minimal FreeType
//! 2.13.3 (the module list of scripts/_build_freetype.sh), all through `cc`, with the touch
//! UI's Makefile flags. LVGL_DIR: patched LVGL v9.5.0 tree; FT_SRC: FreeType source tree;
//! LV_CONF_DIR: the directory holding lv_conf.h (default: the repo root).
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use std::path::{Path, PathBuf};

fn c_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            c_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "c") {
            out.push(p);
        }
    }
}

fn main() {
    let env = |k: &str| PathBuf::from(std::env::var(k).unwrap_or_else(|_| panic!("set {k}")));
    let (lvgl, ft) = (env("LVGL_DIR"), env("FT_SRC"));
    let conf = std::env::var("LV_CONF_DIR").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("../.."));
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    for k in ["LVGL_DIR", "FT_SRC", "LV_CONF_DIR"] {
        println!("cargo:rerun-if-env-changed={k}");
    }
    println!("cargo:rerun-if-changed=csrc/static_fns.c");
    println!("cargo:rerun-if-changed={}", conf.join("lv_conf.h").display());

    let mut files = Vec::new();
    c_files(&lvgl.join("src"), &mut files);
    cc::Build::new()
        .std("c11")
        .flag("-Os")
        .flag("-ffunction-sections")
        .flag("-fdata-sections")
        .define("_GNU_SOURCE", None)
        .define("LV_CONF_INCLUDE_SIMPLE", None)
        .include(&conf)
        .include(&lvgl)
        .include(ft.join("include"))
        .warnings(false)
        .files(&files)
        .file("csrc/static_fns.c")
        .compile("lvgl");

    // FreeType's module list (ftmodule.h), cut down to what the UI's TTF fonts need; order kept.
    const FT_MODULES: [(&str, &str); 8] = [
        ("FT_Module_Class", "autofit_module_class"),
        ("FT_Driver_ClassRec", "tt_driver_class"),
        ("FT_Driver_ClassRec", "cff_driver_class"),
        ("FT_Module_Class", "psaux_module_class"),
        ("FT_Module_Class", "psnames_module_class"),
        ("FT_Module_Class", "pshinter_module_class"),
        ("FT_Module_Class", "sfnt_module_class"),
        ("FT_Renderer_Class", "ft_smooth_renderer_class"),
    ];
    let module_list: String = FT_MODULES
        .iter()
        .map(|(class, name)| format!("FT_USE_MODULE( {class}, {name} )\n"))
        .collect();
    std::fs::write(out.join("ftmodule_min.h"), module_list).unwrap();
    // after lvgl: the linker resolves lvgl's FreeType calls from what comes later
    let mut b = cc::Build::new();
    b.include(ft.join("include"))
        .include(&out)
        .define("FT2_BUILD_LIBRARY", None)
        .define("FT_CONFIG_MODULES_H", "\"ftmodule_min.h\"")
        .opt_level(2)
        .warnings(false);
    for s in [
        "base/ftbase", "base/ftsystem", "base/ftinit", "base/ftdebug", "base/ftbbox", "base/ftbitmap", "base/ftglyph", "base/ftmm",
        "base/ftstroke", "base/ftsynth", "cache/ftcache", "autofit/autofit", "truetype/truetype", "cff/cff", "psaux/psaux",
        "psnames/psnames", "pshinter/pshinter", "sfnt/sfnt", "smooth/smooth", "gzip/ftgzip",
    ] {
        b.file(ft.join("src").join(format!("{s}.c")));
    }
    b.compile("freetype");
}

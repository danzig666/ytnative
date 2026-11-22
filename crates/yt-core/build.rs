//! Compiles QuickJS (vendor/quickjs) and its one-call wrapper (csrc/jsrun.c): the JavaScript
//! engine that runs yt-dlp's challenge solver on YouTube's player script (`crate::jsc`).
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let qjs = manifest.join("../../vendor/quickjs");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=csrc/jsrun.c");
    println!("cargo:rerun-if-changed={}", qjs.display());

    let msvc = std::env::var("TARGET").unwrap().ends_with("-msvc");
    let mut b = cc::Build::new();
    for f in ["quickjs.c", "libregexp.c", "libunicode.c", "dtoa.c"] {
        b.file(qjs.join(f));
    }
    b.file(manifest.join("csrc/jsrun.c"))
        .include(&qjs)
        .define("QUICKJS_NG_BUILD", None)
        .define("_GNU_SOURCE", None)
        .opt_level(2)
        .warnings(false);
    if msvc {
        b.define("WIN32_LEAN_AND_MEAN", None).define("_WIN32_WINNT", "0x0601").flag("/experimental:c11atomics").flag("/std:c11");
    } else {
        b.flag("-funsigned-char");
    }
    b.compile("ytjs");
    if !msvc {
        println!("cargo:rustc-link-lib=m");
    }
}

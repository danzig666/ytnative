//! Compiles the C pieces (nanosvg rasterizer) and links the TV's own shared libraries by soname
//! from `sysroot/lib` (pulled from the TV with `cargo xtask pull-sysroot`).
use std::path::PathBuf;

fn main() {
    let target = std::env::var("TARGET").unwrap();
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../vendor/plx/src/svg.c");

    cc::Build::new()
        .file(root.join("vendor/plx/src/svg.c"))
        .include(root.join("vendor/nanosvg"))
        .define("_GNU_SOURCE", None)
        .opt_level(2)
        .warnings(false)
        .compile("ytsvg");

    if target == "armv7-unknown-linux-gnueabi" {
        let lib = root.join("sysroot/lib");
        if !lib.join("libSDL2-2.0.so.0").exists() {
            panic!("sysroot/lib is missing: run `cargo xtask pull-sysroot` once (needs the TV)");
        }
        println!("cargo:rustc-link-search=native={}", lib.display());
        for so in [
            "libSDL2-2.0.so.0",
            "libSDL2_ttf-2.0.so.0",
            "libGLESv2.so.2",
            // TODO(portability): link a generated GLESv2 stub instead of the Mali driver.
            "libmali.so.0",
            "libwayland-client.so.0",
            "libluna-service2.so.3",
            "libglib-2.0.so.0",
        ] {
            println!("cargo:rustc-link-arg=-l:{so}");
        }
        // The TV libraries' own dependencies are resolved on the TV, not here.
        println!("cargo:rustc-link-arg=-Wl,--allow-shlib-undefined");
    }
}

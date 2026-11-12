//! Compiles the C pieces (nanosvg rasterizer) and, for the TV, links the TV's shared libraries by
//! soname through generated link-only stubs (see `link_tv_stubs`).
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

    // ---- TV link set (portability) --------------------------------------------------------
    if target == "armv7-unknown-linux-gnueabi" {
        link_tv_stubs(&root);
    }
    // ---- end TV link set ----------------------------------------------------------------------
}

// ---- TV link set (portability) ------------------------------------------------------------
/// Links the TV libraries by soname against link-only stubs generated from `ci/symbols/*.txt`
/// (tools/gen-stubs.py), never against LG's binaries: the build needs nothing from a TV (CI links
/// the same way), and DT_NEEDED names `libGLESv2.so.2` rather than one GPU vendor's driver. On the
/// TV the real libGLESv2 pulls in its own driver (libmali on today's sets). Whether every import
/// exists on real firmware is checked by `cargo xtask symcheck` / `fwcompat` (docs/debugging.md).
fn link_tv_stubs(root: &std::path::Path) {
    println!("cargo:rerun-if-changed=../../ci/symbols");
    println!("cargo:rerun-if-changed=../../tools/gen-stubs.py");
    println!("cargo:rerun-if-env-changed=YTNATIVE_STUBS_DIR");
    let stubs = std::env::var("YTNATIVE_STUBS_DIR").map(PathBuf::from).unwrap_or_else(|_| root.join("target/stubs"));
    let python = std::env::var("PYTHON").unwrap_or_else(|_| "python".into());
    let st = std::process::Command::new(python)
        .arg(root.join("tools/gen-stubs.py"))
        .arg("--out")
        .arg(&stubs)
        .status()
        .expect("running python tools/gen-stubs.py");
    assert!(st.success(), "tools/gen-stubs.py failed");
    println!("cargo:rustc-link-search=native={}", stubs.display());
    for so in [
        "libSDL2-2.0.so.0",
        "libSDL2_ttf-2.0.so.0",
        "libGLESv2.so.2",
        "libwayland-client.so.0",
        "libluna-service2.so.3",
        "libglib-2.0.so.0",
    ] {
        println!("cargo:rustc-link-arg=-l:{so}");
    }
    // The stubs have no dependencies of their own; the real ones are resolved on the TV.
    println!("cargo:rustc-link-arg=-Wl,--allow-shlib-undefined");
}
// ---- end TV link set ----------------------------------------------------------------------

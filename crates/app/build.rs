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
        // Engine B (Feed): the StarfishMediaAPIs shim (csrc/starfish.c). Its methods are
        // dlsym'd at runtime, but libplayerAPIs.so.1 is also on the link line: that is what puts
        // the callback-gate interposer (a symbol the library defines too) into the executable's
        // dynamic symbol table, so it preempts the library's own callbackFunctionHook. zig's
        // linker driver rejects --export-dynamic-symbol / --dynamic-list and ignores
        // --as-needed, so this makes libplayerAPIs.so.1 a DT_NEEDED of the app (present on every
        // webOS release plx-native inventoried, 1.2 to 11.2).
        println!("cargo:rustc-link-arg=-l:libplayerAPIs.so.1"); // stub from ci/symbols, real one on the TV
        println!("cargo:rerun-if-changed=csrc/starfish.c");
        println!("cargo:rerun-if-changed=csrc/starfish.h");
        println!("cargo:rerun-if-changed=csrc/video_plane.c");
        println!("cargo:rerun-if-changed=csrc/video_plane.h");
        cc::Build::new()
            .file("csrc/starfish.c")
            // The webOS 4 video plane (ACB): libAcbAPI.so.1 is dlopen'd at runtime, never linked
            // (webOS 5+ deleted it, and a DT_NEEDED would kill the process there before main).
            .file("csrc/video_plane.c")
            .define("_GNU_SOURCE", None)
            .opt_level(2)
            .warnings(false)
            .compile("ytstarfish");
    }
    // ---- end TV link set ----------------------------------------------------------------------

    if target.ends_with("windows-msvc") {
        windows_sim(&root);
    }
}

/// The Windows desktop simulator: link SDL2 / SDL2_ttf from `third_party/win` (fetched by
/// `tools/fetch-sim-deps.ps1`), generate the GL forwarding trampolines, and stage the DLLs and
/// fonts next to the built exe (the app loads fonts from its own directory).
fn windows_sim(root: &std::path::Path) {
    let tp = root.join("third_party/win");
    let find = |prefix: &str| -> PathBuf {
        std::fs::read_dir(&tp)
            .ok()
            .and_then(|rd| {
                rd.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(prefix)))
                    .max()
            })
            .unwrap_or_else(|| panic!("third_party/win/{prefix}* is missing: run tools/fetch-sim-deps.ps1 once"))
            .join("lib/x64")
    };
    let (sdl, ttf) = (find("SDL2-"), find("SDL2_ttf-"));
    println!("cargo:rerun-if-changed={}", tp.display());
    println!("cargo:rustc-link-search=native={}", sdl.display());
    println!("cargo:rustc-link-search=native={}", ttf.display());
    println!("cargo:rustc-link-lib=dylib=SDL2");
    println!("cargo:rustc-link-lib=dylib=SDL2_ttf");

    // GL trampolines: `jmp [table + 8*i]` per name, the table filled after context creation.
    let names_file = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("src/sim/gl_names.txt");
    println!("cargo:rerun-if-changed={}", names_file.display());
    let names: Vec<String> = std::fs::read_to_string(&names_file)
        .unwrap()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect();
    let mut asm = String::from(".text\n");
    for (i, n) in names.iter().enumerate() {
        asm.push_str(&format!(".globl {n}\n.p2align 4\n{n}:\n    jmp qword ptr [rip + {{t}} + {}]\n", i * 8));
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let code = format!(
        "pub const GL_NAMES: [&str; {n}] = {names:?};\n\
         #[no_mangle]\n\
         pub static mut YTSIM_GL_TABLE: [usize; {n}] = [0; {n}];\n\
         std::arch::global_asm!({asm:?}, t = sym YTSIM_GL_TABLE);\n",
        n = names.len(),
    );
    std::fs::write(out.join("gl_shim.rs"), code).unwrap();

    // Stage runtime files next to the exe: OUT_DIR is target/<profile>/build/<pkg>-<hash>/out.
    let exe_dir = out.ancestors().nth(3).unwrap().to_path_buf();
    let copy = |from: PathBuf| {
        if let Some(name) = from.file_name() {
            let _ = std::fs::copy(&from, exe_dir.join(name));
        }
    };
    copy(sdl.join("SDL2.dll"));
    copy(ttf.join("SDL2_ttf.dll"));
    let pkg = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("pkg");
    println!("cargo:rerun-if-changed={}", pkg.display());
    for e in std::fs::read_dir(&pkg).unwrap().flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) == Some("ttf") || p.file_name().and_then(|n| n.to_str()) == Some("appinfo.json") {
            copy(p);
        }
    }
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

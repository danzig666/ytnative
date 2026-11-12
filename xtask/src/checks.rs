//! Link-surface checks: DT_NEEDED against `ci/expected-dt-needed.txt`, every import against the
//! TV's real libraries (`symcheck`), the same against several firmware snapshots (`fwcompat`), and
//! the symbol lists the link stubs are generated from (`stub-symbols`).

use crate::elf::{self, DynSym, Elf, Exports};
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

pub const EXPECTED_NEEDED: &str = "ci/expected-dt-needed.txt";
pub const SYMBOL_LISTS: &str = "ci/symbols";

/// The loader itself: never part of a sysroot snapshot, provides only `_dl_*`/TLS internals.
const LOADER: &str = "ld-linux.so.3";

/// Libraries the app links through generated stubs: (soname, sysroot libs the symbol list is
/// taken from, name filter). The GL/EGL stubs take their lists from the Mali driver too, because
/// LG's own libGLESv2.so.2/libEGL.so.1 are thin front libraries over it.
pub const STUBS: &[(&str, &[&str], Option<&[&str]>)] = &[
    ("libSDL2-2.0.so.0", &["libSDL2-2.0.so.0"], None),
    ("libSDL2_ttf-2.0.so.0", &["libSDL2_ttf-2.0.so.0"], None),
    ("libGLESv2.so.2", &["libGLESv2.so.2", "libmali.so.0"], Some(&["gl"])),
    ("libEGL.so.1", &["libEGL.so.1", "libmali.so.0"], Some(&["egl"])),
    ("libwayland-client.so.0", &["libwayland-client.so.0"], None),
    ("libluna-service2.so.3", &["libluna-service2.so.3"], None),
    ("libglib-2.0.so.0", &["libglib-2.0.so.0"], None),
    ("libplayerAPIs.so.1", &["libplayerAPIs.so.1"], None),
];

// ---------------------------------------------------------------- DT_NEEDED

pub fn needed(bin: &Path, check: bool) -> Result<()> {
    let data = std::fs::read(bin).with_context(|| format!("reading {} (build first)", bin.display()))?;
    let mut got = Elf::parse(&data)?.needed();
    got.sort();
    if !check {
        for n in &got {
            println!("{n}");
        }
        return Ok(());
    }
    let raw = std::fs::read_to_string(EXPECTED_NEEDED).with_context(|| format!("reading {EXPECTED_NEEDED}"))?;
    let mut want: Vec<String> =
        raw.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_string).collect();
    want.sort();
    if got == want {
        println!("DT_NEEDED OK ({} libraries, matches {EXPECTED_NEEDED})", got.len());
        return Ok(());
    }
    for n in got.iter().filter(|n| !want.contains(n)) {
        println!("  + {n}   (new dependency: is it on every target firmware?)");
    }
    for n in want.iter().filter(|n| !got.contains(n)) {
        println!("  - {n}   (no longer needed)");
    }
    bail!("DT_NEEDED differs from {EXPECTED_NEEDED}; update that file if the change is intended")
}

// ---------------------------------------------------------------- grading

/// One sysroot snapshot: every `.so*` in a directory, keyed by DT_SONAME (and file name).
struct Snapshot {
    libs: HashMap<String, PathBuf>,
}

impl Snapshot {
    fn open(dir: &Path) -> Result<Self> {
        let mut libs = HashMap::new();
        for e in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let p = e?.path();
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            if !p.is_file() || !name.contains(".so") {
                continue;
            }
            if let Ok(data) = std::fs::read(&p) {
                if let Ok(Some(so)) = Elf::parse(&data).map(|e| e.soname()) {
                    libs.entry(so).or_insert_with(|| p.clone());
                }
            }
            libs.entry(name).or_insert(p);
        }
        Ok(Snapshot { libs })
    }
}

pub struct Grade {
    pub imports: usize,
    /// Direct DT_NEEDED entries the snapshot lacks: the loader would refuse to start the binary.
    pub missing_libs: Vec<String>,
    /// Libraries only needed by other libraries, absent from the snapshot. Not an error by
    /// themselves (a snapshot only holds what we pulled), but their exports were not searched.
    pub unsearched: Vec<String>,
    pub missing_syms: Vec<String>,
    /// soname -> number of imports it resolves (first in load order wins, like ld.so).
    pub providers: BTreeMap<String, usize>,
}

/// Grades `bin` against one snapshot the way ld.so would resolve it: breadth-first over DT_NEEDED,
/// following each library's own DT_NEEDED (so GL symbols that only the Mali driver defines count
/// as provided when libGLESv2.so.2 pulls it in), symbol versions honoured.
fn grade(bin: &[u8], snap: &Snapshot) -> Result<Grade> {
    let imports = elf::imports(bin)?;
    let mut order: Vec<String> = Vec::new();
    let direct = Elf::parse(bin)?.needed();
    let mut queue: std::collections::VecDeque<String> = direct.clone().into();
    let (mut missing_libs, mut unsearched) = (Vec::new(), Vec::new());
    let mut exports: Vec<(String, Exports)> = Vec::new();
    while let Some(so) = queue.pop_front() {
        if order.contains(&so) || so == LOADER {
            continue;
        }
        order.push(so.clone());
        let Some(path) = snap.libs.get(&so) else {
            if direct.contains(&so) { missing_libs.push(so) } else { unsearched.push(so) }
            continue;
        };
        let data = std::fs::read(path)?;
        let e = Elf::parse(&data)?;
        queue.extend(e.needed());
        let mut ex = Exports::default();
        ex.add_lib(&data)?;
        exports.push((so, ex));
    }
    let mut providers = BTreeMap::new();
    let mut missing_syms = Vec::new();
    for imp in &imports {
        match exports.iter().find(|(_, ex)| ex.provides(imp)) {
            Some((so, _)) => *providers.entry(so.clone()).or_insert(0) += 1,
            None if imp.version_file.as_deref() == Some(LOADER) => *providers.entry(LOADER.to_string()).or_insert(0) += 1,
            None => missing_syms.push(describe(imp)),
        }
    }
    Ok(Grade { imports: imports.len(), missing_libs, unsearched, missing_syms, providers })
}

fn describe(s: &DynSym) -> String {
    match &s.version_file {
        Some(f) => format!("{}  (from {f})", s.display()),
        None => s.display(),
    }
}

pub fn symcheck(bin: &Path) -> Result<()> {
    let data = std::fs::read(bin).context("build first")?;
    let dir = Path::new("sysroot/lib");
    if !dir.is_dir() {
        bail!("sysroot/lib is missing: run `cargo xtask pull-sysroot` first (needs the TV)");
    }
    let g = grade(&data, &Snapshot::open(dir)?)?;
    let provided = g.imports - g.missing_syms.len();
    println!("{} imported symbols, {provided} provided by sysroot/lib:", g.imports);
    for (so, n) in &g.providers {
        println!("  {n:5}  {so}");
    }
    for l in &g.missing_libs {
        println!("  MISSING library: {l}");
    }
    if !g.unsearched.is_empty() {
        println!("  (indirect dependencies not in sysroot/lib, not searched: {})", g.unsearched.join(" "));
    }
    for m in &g.missing_syms {
        println!("  MISSING on TV: {m}");
    }
    if g.missing_syms.is_empty() && g.missing_libs.is_empty() {
        println!("symcheck OK");
        Ok(())
    } else {
        bail!("{} symbols / {} libraries are not provided by the TV's libraries", g.missing_syms.len(), g.missing_libs.len())
    }
}

/// `fwcompat <dir>`: each subdirectory of `dir` is one TV's snapshot (its `lib/` if it has one,
/// else the directory itself; an optional FIRMWARE.txt names it). `dir` may itself be a snapshot.
pub fn fwcompat(bin: &Path, dir: &Path, verbose: bool) -> Result<()> {
    let data = std::fs::read(bin).context("build first")?;
    let mut snaps: Vec<PathBuf> = Vec::new();
    if dir.join("lib").is_dir() {
        snaps.push(dir.to_path_buf());
    } else {
        for e in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let p = e?.path();
            if p.is_dir() {
                snaps.push(p);
            }
        }
    }
    snaps.sort();
    if snaps.is_empty() {
        bail!("no snapshots under {} (expected <dir>/<name>/lib/*.so*)", dir.display());
    }
    let imports = elf::imports(&data)?;
    println!("{}: {} imports, versions required:", bin.display(), imports.len());
    let mut vers: BTreeMap<String, usize> = BTreeMap::new();
    for i in &imports {
        if let Some(v) = &i.version {
            *vers.entry(v.clone()).or_insert(0) += 1;
        }
    }
    println!("  {}", vers.iter().map(|(v, n)| format!("{v} x{n}")).collect::<Vec<_>>().join(", "));
    println!();
    println!("{:<28} {:<34} {:>8} {:>8}  verdict", "snapshot", "firmware", "missing", "libs");
    let mut failed = 0;
    for s in &snaps {
        let libdir = if s.join("lib").is_dir() { s.join("lib") } else { s.clone() };
        let fw = std::fs::read_to_string(s.join("FIRMWARE.txt"))
            .map(|t| t.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" "))
            .unwrap_or_else(|_| "?".into());
        let g = grade(&data, &Snapshot::open(&libdir)?)?;
        let ok = g.missing_syms.is_empty() && g.missing_libs.is_empty();
        if !ok {
            failed += 1;
        }
        let name = s.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        println!(
            "{:<28} {:<34} {:>8} {:>8}  {}",
            name,
            fw.chars().take(34).collect::<String>(),
            g.missing_syms.len(),
            g.missing_libs.len(),
            if ok { "OK (loads: every import resolves)" } else { "FAIL" }
        );
        if !ok {
            let limit = if verbose { usize::MAX } else { 12 };
            for l in &g.missing_libs {
                println!("      missing library {l}");
            }
            for m in g.missing_syms.iter().take(limit) {
                println!("      missing {m}");
            }
            if g.missing_syms.len() > limit {
                println!("      ... {} more (pass --verbose)", g.missing_syms.len() - limit);
            }
        }
    }
    if failed > 0 {
        bail!("{failed} of {} snapshots cannot load this binary", snaps.len());
    }
    Ok(())
}

// ---------------------------------------------------------------- stub symbol lists

/// Writes `ci/symbols/<soname>.txt` from the real libraries in sysroot/lib: every exported
/// function (`name`) and data object (`name object <size>`, so copy relocations get the right size).
/// These lists are what `tools/gen-stubs.py` turns into link-only stub libraries, so the build
/// (and CI) never needs LG's binaries.
pub fn stub_symbols() -> Result<()> {
    let dir = Path::new("sysroot/lib");
    if !dir.is_dir() {
        bail!("sysroot/lib is missing: run `cargo xtask pull-sysroot` first (needs the TV)");
    }
    std::fs::create_dir_all(SYMBOL_LISTS)?;
    for (soname, sources, filter) in STUBS {
        let mut syms: BTreeMap<String, String> = BTreeMap::new();
        for src in *sources {
            let p = dir.join(src);
            let Ok(data) = std::fs::read(&p) else {
                println!("  (no {src} in sysroot/lib)");
                continue;
            };
            for s in Elf::parse(&data)?.dynsyms() {
                if !s.defined || s.hidden || s.bind == 0 {
                    continue;
                }
                if let Some(f) = filter {
                    if !f.iter().any(|p| s.name.starts_with(p)) {
                        continue;
                    }
                }
                let line = match s.kind {
                    elf::STT_FUNC | elf::STT_GNU_IFUNC => s.name.clone(),
                    elf::STT_OBJECT => format!("{} object {}", s.name, s.size),
                    elf::STT_TLS => format!("{} tls {}", s.name, s.size),
                    _ => continue,
                };
                syms.entry(s.name.clone()).or_insert(line);
            }
        }
        let mut out = format!(
            "# Link-only symbol list for {soname} (generated by `cargo xtask stub-symbols` from a TV's own\n\
             # libraries; names only, no LG code). Format: `name` = function, `name object <size>` = data.\n"
        );
        for line in syms.values() {
            out.push_str(line);
            out.push('\n');
        }
        let path = Path::new(SYMBOL_LISTS).join(format!("{soname}.txt"));
        std::fs::write(&path, out)?;
        println!("  {:5} symbols -> {}", syms.len(), path.display());
    }
    Ok(())
}

/// `needed`/`dump` helper: DT_NEEDED, soname and build id of any ELF.
pub fn describe_file(p: &Path) -> Result<()> {
    let data = std::fs::read(p)?;
    let e = Elf::parse(&data)?;
    println!("{}: type {} soname {:?} build-id {:?}", p.display(), e.e_type, e.soname(), e.build_id());
    for n in e.needed() {
        println!("  NEEDED {n}");
    }
    Ok(())
}

//! `cargo xtask crash`: fetch the crash tracer's log (crates/app/src/crashtrace.rs) from the TV and
//! symbolize it against the kept unstripped binary, `target/<T>/release/dist/ytnative.debug`.
//!
//!   cargo xtask crash               fetch /tmp/ytnative/crash.log, symbolize the last record
//!   cargo xtask crash <file> [--all] symbolize a local copy (every record with --all)
//!   cargo xtask crash --selftest    build a synthetic record from known functions and check that
//!                                   symbolization lands on them (no TV needed)
//!
//! Address arithmetic: a `map:` line gives (start, file offset) for each executable mapping, so an
//! address becomes a file offset (`addr - start + offset`) and then, through the binary's PT_LOAD
//! headers, a link-time address -- correct for PIE (ET_DYN, today's build) and ET_EXEC alike.

use crate::elf::Elf;
use anyhow::{bail, Context, Result};
use std::path::Path;

struct MapLine {
    start: u32,
    end: u32,
    offset: u32,
    path: String,
}

fn parse_map(line: &str) -> Option<MapLine> {
    let f: Vec<&str> = line.split_whitespace().collect();
    let (range, offset) = (f.first()?, f.get(2)?);
    let (s, e) = range.split_once('-')?;
    Some(MapLine {
        start: u32::from_str_radix(s, 16).ok()?,
        end: u32::from_str_radix(e, 16).ok()?,
        offset: u32::from_str_radix(offset, 16).ok()?,
        path: f.get(5..).map(|p| p.join(" ")).unwrap_or_default(),
    })
}

fn hex(s: &str) -> Option<u32> {
    u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()
}

pub fn run(arg: Option<&str>) -> Result<()> {
    let args: Vec<String> = std::env::args().skip(2).collect();
    let all = args.iter().any(|a| a == "--all");
    let text = match arg {
        Some("--selftest") => return selftest(),
        Some(f) if !f.starts_with("--") => std::fs::read_to_string(f).with_context(|| format!("reading {f}"))?,
        _ => fetch()?,
    };
    let records: Vec<&str> = text.split("*** SIGNAL").skip(1).collect();
    if records.is_empty() {
        println!("no crash records (the log is empty: no traced crash since it was last cleared)");
        return Ok(());
    }
    let dbg_path = crate::debug_binary();
    let dbg = std::fs::read(&dbg_path).with_context(|| format!("reading {} (run `cargo xtask build`)", dbg_path.display()))?;
    let sym = Symbolizer::new(&dbg_path, &dbg)?;
    let pick: Vec<&str> = if all { records } else { vec![records[records.len() - 1]] };
    for r in pick {
        println!("{}", sym.symbolize(&format!("*** SIGNAL{r}"))?);
    }
    Ok(())
}

fn fetch() -> Result<String> {
    let log = crate::tv_ssh(&format!("cat {}/crash.log 2>/dev/null", crate::TV_TMP))?;
    std::fs::create_dir_all("target/crash")?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let local = format!("target/crash/crash-{stamp}.log");
    std::fs::write(&local, &log)?;
    println!("fetched {}/crash.log -> {local}", crate::TV_TMP);
    Ok(log)
}

struct Symbolizer<'a> {
    elf: Elf<'a>,
    build_id: Option<String>,
    loader: addr2line::Loader,
    exe_name: String,
}

impl<'a> Symbolizer<'a> {
    fn new(path: &Path, data: &'a [u8]) -> Result<Self> {
        let elf = Elf::parse(data)?;
        let build_id = elf.build_id();
        let loader = addr2line::Loader::new(path).map_err(|e| anyhow::anyhow!("loading {}: {e}", path.display()))?;
        let exe_name = crate::app_field("main").unwrap_or_else(|_| "ytnative".into());
        Ok(Symbolizer { elf, build_id, loader, exe_name })
    }

    /// "func at file:line" for a link-time address, innermost inlined frame first.
    fn lookup(&self, vaddr: u32) -> String {
        let mut parts = Vec::new();
        if let Ok(mut frames) = self.loader.find_frames(vaddr as u64) {
            while let Ok(Some(f)) = frames.next() {
                // line-tables-only DWARF leaves some subprograms unnamed: fall back to .symtab.
                let name = f
                    .function
                    .as_ref()
                    .and_then(|n| n.demangle().ok())
                    .map(|n| n.into_owned())
                    .or_else(|| self.loader.find_symbol(vaddr as u64).map(|s| format!("{:#}", rustc_demangle::demangle(s))))
                    .unwrap_or_else(|| "??".into());
                let loc = f
                    .location
                    .map(|l| format!("{}:{}", l.file.unwrap_or("??").rsplit(['/', '\\']).take(3).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("/"), l.line.unwrap_or(0)))
                    .unwrap_or_else(|| "??:?".into());
                parts.push(format!("{name} at {loc}"));
            }
        }
        if parts.is_empty() {
            if let Some(s) = self.loader.find_symbol(vaddr as u64) {
                parts.push(format!("{:#}", rustc_demangle::demangle(s)));
            }
        }
        if parts.is_empty() {
            "??".into()
        } else {
            parts.join("\n                                   inlined into ")
        }
    }

    fn symbolize(&self, rec: &str) -> Result<String> {
        let mut out = String::new();
        let maps: Vec<MapLine> = rec.lines().filter_map(|l| l.strip_prefix("map: ")).filter_map(parse_map).collect();
        let field = |p: &str| rec.lines().find_map(|l| l.strip_prefix(p)).map(str::trim);
        let addrs = |p: &str| -> Vec<u32> { field(p).map(|v| v.split_whitespace().filter_map(hex).collect()).unwrap_or_default() };
        out += rec.lines().next().unwrap_or("");
        out += "\n";
        for p in ["time: ", "reg: "] {
            if let Some(v) = field(p) {
                out += &format!("{p}{v}\n");
            }
        }
        match (field("build-id: "), &self.build_id) {
            (Some(a), Some(b)) if a == b => out += &format!("build-id {a} matches ytnative.debug\n"),
            (Some(a), Some(b)) => out += &format!(
                "*** build-id {a} != ytnative.debug {b}: the crashed binary is a different build;\n    \
                 the frames below are NOT trustworthy (redeploy, reproduce, or rebuild that commit)\n"
            ),
            _ => out += "(no build-id to compare)\n",
        }
        let frame = |i: usize, a: u32, is_return: bool| -> String {
            let Some(m) = maps.iter().find(|m| a >= m.start && a < m.end) else {
                return format!("  #{i:<2} {a:#010x}  (outside every executable mapping)\n");
            };
            let file = m.path.rsplit('/').next().unwrap_or("");
            let off = a - m.start + m.offset;
            if file != self.exe_name {
                return format!("  #{i:<2} {a:#010x}  {file}+{off:#x}  (TV library: no symbols)\n");
            }
            // Return addresses point after the call: look up the call instruction itself.
            let probe = (off & !1).wrapping_sub(if is_return { 1 } else { 0 });
            match self.elf.offset_to_vaddr(probe) {
                Some(v) => format!("  #{i:<2} {a:#010x}  ytnative+{v:#x}  {}\n", self.lookup(v)),
                None => format!("  #{i:<2} {a:#010x}  ytnative file offset {off:#x} outside PT_LOAD\n"),
            }
        };
        let bt = addrs("bt: ");
        out += "backtrace (pc, lr, frame-pointer chain):\n";
        for (i, a) in bt.iter().enumerate() {
            out += &frame(i, *a, i > 0);
        }
        let scan = addrs("scan: ");
        if !scan.is_empty() {
            out += "stack scan (words pointing into ytnative; likely but not certain return addresses):\n";
            for (i, a) in scan.iter().enumerate() {
                out += &frame(i, *a, true);
            }
        }
        if let Some(sig) = rec.split_whitespace().nth(2) {
            out += match sig {
                "(SIGILL)" => "hint: SIGILL means codegen (an instruction this SoC lacks), not logic.\n",
                "(SIGABRT)" => "hint: SIGABRT is usually a Rust panic or abort(): read the app log (cargo xtask logs).\n",
                "(SIGSEGV)" | "(SIGBUS)" => "hint: bad pointer or wrong struct offset at the first ytnative frame.\n",
                _ => "",
            };
        }
        Ok(out)
    }
}

/// Synthetic record: known functions of ytnative.debug placed at a PIE-like base, as the tracer
/// would log them. Checks that each address symbolizes back to its function.
fn selftest() -> Result<()> {
    let path = crate::debug_binary();
    let data = std::fs::read(&path).with_context(|| format!("reading {} (run `cargo xtask build`)", path.display()))?;
    let elf = Elf::parse(&data)?;
    let mut funcs = elf.functions();
    funcs.retain(|(n, _, s)| *s >= 16 && (n.contains("crashtrace") || n.contains("8ytnative4main") || n.contains("now_us")));
    funcs.sort_by(|a, b| a.0.cmp(&b.0));
    funcs.dedup_by(|a, b| a.1 == b.1);
    if funcs.len() < 2 {
        bail!("selftest: could not find known functions in {}", path.display());
    }
    let base = 0x0047_0000u32;
    let exe = format!("/media/developer/apps/usr/palm/applications/{}/ytnative", crate::app_field("id")?);
    let mut maps = String::new();
    for l in elf.loads.iter().filter(|l| l.flags & 1 != 0) {
        let (s, e) = (base + (l.vaddr & !0xfff), base + (l.vaddr + l.memsz).div_ceil(0x1000) * 0x1000);
        maps += &format!("map: {s:08x}-{e:08x} r-xp {:08x} b3:12 4242       {exe}\n", l.offset & !0xfff);
    }
    maps += "map: b6c00000-b6d20000 r-xp 00000000 b3:0c 77         /usr/lib/libmali.so.0\n";
    // pc inside the first function, then return addresses (call site + 4) inside the others.
    let pc = base + funcs[0].1 + 8;
    let rets: Vec<u32> = funcs[1..].iter().map(|f| base + f.1 + f.2 / 2).collect();
    let bt: Vec<String> =
        std::iter::once(pc).chain(rets.iter().copied()).chain([0xb6c01234]).map(|a| format!("{a:#010x}")).collect();
    let id = elf.build_id().unwrap_or_default();
    let rec = format!(
        "*** SIGNAL 11 (SIGSEGV) code=1 addr=0x00000000 pc={pc:#010x} lr={:#010x}\nbuild-id: {id}\n\
         time: 1700000000 pid=1234 tid=1234 thread=ytnative\nbt: {}\nscan:\n{maps}*** END\n",
        rets[0],
        bt.join(" ")
    );
    println!("synthetic record:\n{rec}");
    let sym = Symbolizer::new(&path, &data)?;
    let out = sym.symbolize(&rec)?;
    println!("{out}");
    let mut ok = 0;
    for (i, f) in funcs.iter().enumerate() {
        let short = format!("{:#}", rustc_demangle::demangle(&f.0));
        let leaf = short.rsplit("::").next().unwrap_or(&short).to_string();
        let line = out.lines().find(|l| l.trim_start().starts_with(&format!("#{i} "))).unwrap_or("");
        // Inlining may name an inner function first; the frame must mention ours somewhere.
        let block: String = out
            .split(&format!("#{i} "))
            .nth(1)
            .map(|s| s.split("  #").next().unwrap_or(s).to_string())
            .unwrap_or_default();
        let hit = block.contains(&leaf);
        println!("  {} frame #{i}: expected {short}", if hit { "ok  " } else { "MISS" });
        if hit {
            ok += 1;
        } else {
            println!("        got: {line}");
        }
    }
    if !out.contains("libmali.so.0+0x1234") {
        bail!("selftest: TV library frame was not attributed to libmali.so.0+0x1234");
    }
    if ok != funcs.len() {
        bail!("selftest: {}/{} frames symbolized to the expected function", ok, funcs.len());
    }
    println!("crash selftest OK ({ok} frames + 1 library frame)");
    Ok(())
}

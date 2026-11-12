//! Just enough 32-bit little-endian ELF parsing for the TV binary and the TV's libraries:
//! dynamic symbols with their GNU symbol versions, DT_NEEDED / DT_SONAME, the GNU build id and
//! the PT_LOAD segments (for turning a crash address into a link-time address).

use anyhow::{bail, Result};
use std::collections::HashMap;

const SHT_DYNSYM: u32 = 11;
const SHT_DYNAMIC: u32 = 6;
const SHT_NOTE: u32 = 7;
const SHT_GNU_VERDEF: u32 = 0x6fff_fffd;
const SHT_GNU_VERNEED: u32 = 0x6fff_fffe;
const SHT_GNU_VERSYM: u32 = 0x6fff_ffff;
const PT_LOAD: u32 = 1;
const DT_NEEDED: u32 = 1;
const DT_SONAME: u32 = 14;

pub const STB_WEAK: u8 = 2;
pub const STT_OBJECT: u8 = 1;
pub const STT_FUNC: u8 = 2;
pub const STT_TLS: u8 = 6;
pub const STT_GNU_IFUNC: u8 = 10;
const SHN_ABS: u16 = 0xfff1;

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn cstr(tab: &[u8], off: usize) -> String {
    if off >= tab.len() {
        return String::new();
    }
    let end = tab[off..].iter().position(|&c| c == 0).unwrap_or(tab.len() - off);
    String::from_utf8_lossy(&tab[off..off + end]).into_owned()
}

#[derive(Clone, Copy)]
struct Shdr {
    ty: u32,
    off: usize,
    size: usize,
    link: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Load {
    pub offset: u32,
    pub vaddr: u32,
    pub filesz: u32,
    pub memsz: u32,
    pub flags: u32,
}

#[derive(Clone, Debug)]
pub struct DynSym {
    pub name: String,
    /// GNU symbol version (`GLIBC_2.4`), None when unversioned (or the base version).
    pub version: Option<String>,
    /// For an import: the library the version requirement names (verneed `vn_file`).
    pub version_file: Option<String>,
    /// `foo@VER` (hidden, non-default) as opposed to `foo@@VER`.
    pub hidden: bool,
    pub defined: bool,
    pub bind: u8,
    pub kind: u8,
    pub size: u32,
}

impl DynSym {
    pub fn display(&self) -> String {
        match &self.version {
            Some(v) => format!("{}@{}", self.name, v),
            None => self.name.clone(),
        }
    }
}

pub struct Elf<'a> {
    b: &'a [u8],
    sh: Vec<Shdr>,
    pub e_type: u16,
    pub loads: Vec<Load>,
}

impl<'a> Elf<'a> {
    pub fn parse(b: &'a [u8]) -> Result<Self> {
        if b.len() < 52 || &b[..4] != b"\x7fELF" || b[4] != 1 || b[5] != 1 {
            bail!("not a 32-bit little-endian ELF");
        }
        let e_type = u16le(b, 16);
        let phoff = u32le(b, 28) as usize;
        let shoff = u32le(b, 32) as usize;
        let phentsize = u16le(b, 42) as usize;
        let phnum = u16le(b, 44) as usize;
        let shentsize = u16le(b, 46) as usize;
        let shnum = u16le(b, 48) as usize;
        let mut sh = Vec::with_capacity(shnum);
        for i in 0..shnum {
            let o = shoff + i * shentsize;
            if o + 40 > b.len() {
                bail!("truncated section header table");
            }
            sh.push(Shdr { ty: u32le(b, o + 4), off: u32le(b, o + 16) as usize, size: u32le(b, o + 20) as usize, link: u32le(b, o + 24) });
        }
        let mut loads = Vec::new();
        for i in 0..phnum {
            let o = phoff + i * phentsize;
            if o + 32 > b.len() {
                bail!("truncated program header table");
            }
            if u32le(b, o) == PT_LOAD {
                loads.push(Load {
                    offset: u32le(b, o + 4),
                    vaddr: u32le(b, o + 8),
                    filesz: u32le(b, o + 16),
                    memsz: u32le(b, o + 20),
                    flags: u32le(b, o + 24),
                });
            }
        }
        Ok(Elf { b, sh, e_type, loads })
    }

    fn data(&self, s: &Shdr) -> &'a [u8] {
        let end = (s.off + s.size).min(self.b.len());
        &self.b[s.off.min(end)..end]
    }

    fn find(&self, ty: u32) -> Option<Shdr> {
        self.sh.iter().copied().find(|s| s.ty == ty)
    }

    fn strtab_of(&self, s: &Shdr) -> &'a [u8] {
        self.sh.get(s.link as usize).map(|t| self.data(t)).unwrap_or(&[])
    }

    /// verneed: version index -> (file, version name)
    fn verneed(&self) -> HashMap<u16, (String, String)> {
        let mut m = HashMap::new();
        let Some(s) = self.find(SHT_GNU_VERNEED) else { return m };
        let (d, st) = (self.data(&s), self.strtab_of(&s));
        let mut o = 0usize;
        while o + 16 <= d.len() {
            let cnt = u16le(d, o + 2) as usize;
            let file = cstr(st, u32le(d, o + 4) as usize);
            let mut a = o + u32le(d, o + 8) as usize;
            for _ in 0..cnt {
                if a + 16 > d.len() {
                    break;
                }
                let other = u16le(d, a + 6);
                m.insert(other, (file.clone(), cstr(st, u32le(d, a + 8) as usize)));
                let next = u32le(d, a + 12) as usize;
                if next == 0 {
                    break;
                }
                a += next;
            }
            let next = u32le(d, o + 12) as usize;
            if next == 0 {
                break;
            }
            o += next;
        }
        m
    }

    /// verdef: version index -> version name (the base/soname entry is left out).
    fn verdef(&self) -> HashMap<u16, String> {
        let mut m = HashMap::new();
        let Some(s) = self.find(SHT_GNU_VERDEF) else { return m };
        let (d, st) = (self.data(&s), self.strtab_of(&s));
        let mut o = 0usize;
        while o + 20 <= d.len() {
            let flags = u16le(d, o + 2);
            let ndx = u16le(d, o + 4);
            let aux = o + u32le(d, o + 12) as usize;
            if flags & 1 == 0 && aux + 8 <= d.len() {
                m.insert(ndx, cstr(st, u32le(d, aux) as usize));
            }
            let next = u32le(d, o + 16) as usize;
            if next == 0 {
                break;
            }
            o += next;
        }
        m
    }

    pub fn dynsyms(&self) -> Vec<DynSym> {
        let Some(s) = self.find(SHT_DYNSYM) else { return Vec::new() };
        let (d, st) = (self.data(&s), self.strtab_of(&s));
        let versym = self.find(SHT_GNU_VERSYM).map(|v| self.data(&v)).unwrap_or(&[]);
        let (need, def) = (self.verneed(), self.verdef());
        let mut out = Vec::new();
        for k in 1..d.len() / 16 {
            let o = k * 16;
            let name = cstr(st, u32le(d, o) as usize);
            if name.is_empty() {
                continue;
            }
            let info = d[o + 12];
            let shndx = u16le(d, o + 14);
            let defined = shndx != 0;
            let vs = if versym.len() >= (k + 1) * 2 { u16le(versym, k * 2) } else { 1 };
            let (idx, hidden) = (vs & 0x7fff, vs & 0x8000 != 0);
            let (version, version_file) = if idx < 2 {
                (None, None)
            } else if defined {
                (def.get(&idx).cloned(), None)
            } else {
                match need.get(&idx) {
                    Some((f, v)) => (Some(v.clone()), Some(f.clone())),
                    None => (None, None),
                }
            };
            out.push(DynSym {
                name,
                version,
                version_file,
                hidden,
                defined: defined && shndx != SHN_ABS,
                bind: info >> 4,
                kind: info & 0xf,
                size: u32le(d, o + 8),
            });
        }
        out
    }

    /// Defined functions from the full `.symtab` (only in unstripped binaries): (name, addr, size).
    pub fn functions(&self) -> Vec<(String, u32, u32)> {
        let mut out = Vec::new();
        for s in self.sh.iter().filter(|s| s.ty == 2) {
            let (d, st) = (self.data(s), self.strtab_of(s));
            for o in (1..d.len() / 16).map(|k| k * 16) {
                if d[o + 12] & 0xf == STT_FUNC && u16le(d, o + 14) != 0 {
                    out.push((cstr(st, u32le(d, o) as usize), u32le(d, o + 4) & !1, u32le(d, o + 8)));
                }
            }
        }
        out
    }

    fn dynamic(&self) -> Vec<(u32, String)> {
        let Some(s) = self.find(SHT_DYNAMIC) else { return Vec::new() };
        let (d, st) = (self.data(&s), self.strtab_of(&s));
        let mut out = Vec::new();
        for o in (0..d.len() / 8).map(|i| i * 8) {
            let tag = u32le(d, o);
            if tag == 0 {
                break;
            }
            if tag == DT_NEEDED || tag == DT_SONAME {
                out.push((tag, cstr(st, u32le(d, o + 4) as usize)));
            }
        }
        out
    }

    pub fn needed(&self) -> Vec<String> {
        self.dynamic().into_iter().filter(|(t, _)| *t == DT_NEEDED).map(|(_, s)| s).collect()
    }

    pub fn soname(&self) -> Option<String> {
        self.dynamic().into_iter().find(|(t, _)| *t == DT_SONAME).map(|(_, s)| s)
    }

    /// NT_GNU_BUILD_ID as lowercase hex.
    pub fn build_id(&self) -> Option<String> {
        for s in self.sh.iter().filter(|s| s.ty == SHT_NOTE) {
            let d = self.data(s);
            let mut o = 0;
            while o + 12 <= d.len() {
                let (nsz, dsz, ty) = (u32le(d, o) as usize, u32le(d, o + 4) as usize, u32le(d, o + 8));
                let name_end = o + 12 + nsz.div_ceil(4) * 4;
                let desc_end = name_end + dsz;
                if desc_end > d.len() {
                    break;
                }
                if ty == 3 && &d[o + 12..o + 12 + nsz.min(3)] == b"GNU" {
                    return Some(d[name_end..desc_end].iter().map(|b| format!("{b:02x}")).collect());
                }
                o = name_end + dsz.div_ceil(4) * 4;
            }
        }
        None
    }

    /// File offset -> link-time virtual address, through the PT_LOAD that maps it.
    pub fn offset_to_vaddr(&self, off: u32) -> Option<u32> {
        self.loads.iter().find(|l| off >= l.offset && off < l.offset + l.filesz.max(1)).map(|l| off - l.offset + l.vaddr)
    }
}

/// Imports of a binary: undefined, non-weak dynamic symbols (weak ones may legitimately be
/// absent: Rust's std probes `statx`, `getrandom`, ... at runtime), deduplicated.
pub fn imports(b: &[u8]) -> Result<Vec<DynSym>> {
    let mut v: Vec<DynSym> = Elf::parse(b)?.dynsyms().into_iter().filter(|s| !s.defined && s.bind != STB_WEAK).collect();
    v.sort_by(|a, b| a.display().cmp(&b.display()));
    v.dedup_by(|a, b| a.display() == b.display());
    Ok(v)
}

/// Exports of a library: (name -> list of (version, hidden)).
#[derive(Default)]
pub struct Exports(pub HashMap<String, Vec<(Option<String>, bool)>>);

impl Exports {
    pub fn add_lib(&mut self, b: &[u8]) -> Result<()> {
        for s in Elf::parse(b)?.dynsyms().into_iter().filter(|s| s.defined) {
            self.0.entry(s.name).or_default().push((s.version, s.hidden));
        }
        Ok(())
    }

    /// Would the dynamic loader bind `imp` here? A versioned import needs that exact version
    /// (hidden `foo@VER` definitions still satisfy an explicit `@VER` request); an unversioned one
    /// takes any default (non-hidden) definition.
    pub fn provides(&self, imp: &DynSym) -> bool {
        let Some(defs) = self.0.get(&imp.name) else { return false };
        match &imp.version {
            Some(v) => defs.iter().any(|(dv, _)| dv.as_deref() == Some(v.as_str()) || dv.is_none()),
            None => defs.iter().any(|(_, hidden)| !hidden),
        }
    }
}

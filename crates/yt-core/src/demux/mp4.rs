//! Fragmented MP4 (ISO/IEC 14496-12) as YouTube serves its DASH formats: `ftyp`, `moov` (one
//! `trak`, `mvex/trex`), `sidx`, then `moof`+`mdat` pairs. Incremental: top-level boxes are parsed
//! once complete; a `moof` waits for its `mdat`.

use super::{scale, AccessUnit, Codec, Color, Demuxer, Kind, Rd, SegmentRef, TrackInfo};
use anyhow::{bail, Result};
use std::collections::VecDeque;

#[derive(Clone, Debug, Default)]
struct Trex {
    _sdi: u32,
    duration: u32,
    size: u32,
    flags: u32,
}

#[derive(Clone, Debug)]
struct Trak {
    info: TrackInfo,
    timescale: u32,
    /// `elst` media_time of the first non-empty edit, in track timescale (subtracted from PTS).
    edit_offset: i64,
    trex: Trex,
    /// H.264: NAL length size from avcC.
    nal_len: Option<usize>,
}

/// Samples of one parsed `moof`, waiting for the `mdat` that holds their bytes.
#[derive(Debug)]
struct PendingSample {
    track: usize,
    offset: u64,
    size: u32,
    dts: i64,
    cts: i64,
    duration: u32,
    key: bool,
}

pub struct Mp4Demuxer {
    buf: Vec<u8>,
    /// Absolute file offset of `buf[0]`.
    pos: u64,
    traks: Vec<Trak>,
    infos: Vec<TrackInfo>,
    index: Option<Vec<SegmentRef>>,
    pending: Vec<PendingSample>,
    out: VecDeque<AccessUnit>,
    duration_ns: Option<i64>,
    /// Bytes of an uninteresting box still to discard (lets huge boxes pass without buffering).
    skip: u64,
}

impl Default for Mp4Demuxer {
    fn default() -> Self {
        Self::new()
    }
}

impl Mp4Demuxer {
    pub fn new() -> Self {
        Mp4Demuxer {
            buf: Vec::new(),
            pos: 0,
            traks: Vec::new(),
            infos: Vec::new(),
            index: None,
            pending: Vec::new(),
            out: VecDeque::new(),
            duration_ns: None,
            skip: 0,
        }
    }

    fn parse(&mut self) -> Result<()> {
        loop {
            if self.skip > 0 {
                let n = (self.skip as usize).min(self.buf.len());
                self.buf.drain(..n);
                self.pos += n as u64;
                self.skip -= n as u64;
                if self.skip > 0 {
                    return Ok(());
                }
            }
            let Some((ty, hdr, size)) = box_header(&self.buf) else { return Ok(()) };
            let size = match size {
                Some(s) => s,
                // size 0 = "to the end of the file": only sensible for a trailing mdat.
                None => {
                    if &ty == b"mdat" && !self.pending.is_empty() {
                        let len = self.buf.len() as u64;
                        self.extract(len)?;
                    }
                    return Ok(());
                }
            };
            if size < hdr as u64 {
                bail!("mp4: box {} has bad size {size}", String::from_utf8_lossy(&ty));
            }
            let interesting = matches!(&ty, b"moov" | b"moof" | b"sidx" | b"mdat");
            if !interesting {
                self.skip = size;
                continue;
            }
            if (self.buf.len() as u64) < size {
                return Ok(());
            }
            let size = size as usize;
            match &ty {
                b"moov" => self.parse_moov(&self.buf[hdr..size].to_vec())?,
                b"sidx" => {
                    let end = self.pos + size as u64;
                    self.index = Some(parse_sidx(&self.buf[hdr..size], end)?);
                }
                b"moof" => {
                    let start = self.pos;
                    let body = self.buf[hdr..size].to_vec();
                    self.parse_moof(&body, start)?;
                }
                b"mdat" => self.extract(size as u64)?,
                _ => {}
            }
            self.buf.drain(..size);
            self.pos += size as u64;
        }
    }

    /// Moves every pending sample whose bytes lie in `buf[..avail]` to the output.
    fn extract(&mut self, avail: u64) -> Result<()> {
        let end = self.pos + avail;
        let pending = std::mem::take(&mut self.pending);
        for s in pending {
            if s.offset < self.pos || s.offset + s.size as u64 > end {
                // A sample outside this mdat: data offsets we cannot honour. Drop it rather than
                // feed garbage.
                continue;
            }
            let t = &self.traks[s.track];
            let at = (s.offset - self.pos) as usize;
            let data = self.buf[at..at + s.size as usize].to_vec();
            // YouTube's fragments flag mid-fragment IDRs as non-sync samples (seen in SABR
            // segments): for H.264 the NAL types decide.
            let key = match t.nal_len {
                Some(n) => super::framing::h264_has_idr(&data, n),
                None => s.key,
            };
            let ts = t.timescale as i64;
            let pts = s.dts + s.cts - t.edit_offset;
            self.out.push_back(AccessUnit {
                track: t.info.id,
                kind: t.info.kind,
                pts_ns: scale(pts, 1_000_000_000, ts),
                dts_ns: scale(s.dts - t.edit_offset, 1_000_000_000, ts),
                duration_ns: scale(s.duration as i64, 1_000_000_000, ts),
                keyframe: key,
                data,
            });
        }
        Ok(())
    }

    fn parse_moov(&mut self, body: &[u8]) -> Result<()> {
        let mut traks = Vec::new();
        let mut trexs = Vec::new();
        let mut mvhd_dur = None;
        for (ty, b) in boxes(body) {
            match &ty {
                b"trak" => {
                    if let Some(t) = parse_trak(b) {
                        traks.push(t);
                    }
                }
                b"mvex" => {
                    for (ty, b) in boxes(b) {
                        if &ty == b"trex" {
                            let mut r = Rd::new(b);
                            r.skip(4);
                            if let (Some(id), Some(sdi), Some(d), Some(s), Some(f)) = (r.u32(), r.u32(), r.u32(), r.u32(), r.u32()) {
                                trexs.push((id, Trex { _sdi: sdi, duration: d, size: s, flags: f }));
                            }
                        }
                    }
                }
                b"mvhd" => {
                    let mut r = Rd::new(b);
                    let v = r.u8().unwrap_or(0);
                    r.skip(3);
                    let (ts, d) = if v == 1 {
                        r.skip(16);
                        (r.u32(), r.u64())
                    } else {
                        r.skip(8);
                        (r.u32(), r.u32().map(u64::from))
                    };
                    if let (Some(ts), Some(d)) = (ts, d) {
                        if d != 0 && d != u32::MAX as u64 && d != u64::MAX {
                            mvhd_dur = Some(scale(d as i64, 1_000_000_000, ts as i64));
                        }
                    }
                }
                _ => {}
            }
        }
        for t in &mut traks {
            if let Some((_, x)) = trexs.iter().find(|(id, _)| *id as u64 == t.info.id) {
                t.trex = x.clone();
            }
        }
        if traks.is_empty() {
            bail!("mp4: moov has no usable track");
        }
        self.infos = traks.iter().map(|t| t.info.clone()).collect();
        self.traks = traks;
        if mvhd_dur.is_some() {
            self.duration_ns = mvhd_dur;
        }
        Ok(())
    }

    fn parse_moof(&mut self, body: &[u8], moof_start: u64) -> Result<()> {
        for (ty, traf) in boxes(body) {
            if &ty != b"traf" {
                continue;
            }
            let mut tfhd = None;
            let mut tfdt = None;
            let mut truns = Vec::new();
            for (ty, b) in boxes(traf) {
                match &ty {
                    b"tfhd" => tfhd = Some(b),
                    b"tfdt" => {
                        let mut r = Rd::new(b);
                        let v = r.u8().unwrap_or(0);
                        r.skip(3);
                        tfdt = if v == 1 { r.u64() } else { r.u32().map(u64::from) };
                    }
                    b"trun" => truns.push(b),
                    _ => {}
                }
            }
            let Some(tfhd) = tfhd else { continue };
            let mut r = Rd::new(tfhd);
            let flags = r.u32().unwrap_or(0) & 0xff_ffff;
            let id = r.u32().unwrap_or(0) as u64;
            let Some(ti) = self.traks.iter().position(|t| t.info.id == id) else { continue };
            let trex = self.traks[ti].trex.clone();
            let base = if flags & 0x1 != 0 { r.u64().unwrap_or(moof_start) } else { moof_start };
            if flags & 0x2 != 0 {
                r.u32();
            }
            let d_dur = if flags & 0x8 != 0 { r.u32().unwrap_or(0) } else { trex.duration };
            let d_size = if flags & 0x10 != 0 { r.u32().unwrap_or(0) } else { trex.size };
            let d_flags = if flags & 0x20 != 0 { r.u32().unwrap_or(0) } else { trex.flags };
            let audio = self.traks[ti].info.kind == Kind::Audio;
            let mut dts = tfdt.unwrap_or(0) as i64;
            let mut cursor = base;
            for trun in truns {
                let mut r = Rd::new(trun);
                let vf = r.u32().unwrap_or(0);
                let (ver, tf) = (vf >> 24, vf & 0xff_ffff);
                let count = r.u32().unwrap_or(0);
                if tf & 0x1 != 0 {
                    let off = r.u32().unwrap_or(0) as i32 as i64;
                    cursor = (base as i64 + off) as u64;
                }
                let first_flags = if tf & 0x4 != 0 { r.u32() } else { None };
                for i in 0..count {
                    let dur = if tf & 0x100 != 0 { r.u32().unwrap_or(d_dur) } else { d_dur };
                    let size = if tf & 0x200 != 0 { r.u32().unwrap_or(d_size) } else { d_size };
                    let mut sflags = if tf & 0x400 != 0 { r.u32().unwrap_or(d_flags) } else { d_flags };
                    if i == 0 {
                        if let Some(f) = first_flags {
                            sflags = f;
                        }
                    }
                    let cts = if tf & 0x800 != 0 {
                        let c = r.u32().unwrap_or(0);
                        if ver == 0 { c as i64 } else { c as i32 as i64 }
                    } else {
                        0
                    };
                    let non_sync = sflags & 0x1_0000 != 0;
                    let depends = (sflags >> 24) & 0x3;
                    let key = audio || (!non_sync && depends != 1);
                    self.pending.push(PendingSample { track: ti, offset: cursor, size, dts, cts, duration: dur, key });
                    cursor += size as u64;
                    dts += dur as i64;
                }
            }
        }
        Ok(())
    }
}

impl Demuxer for Mp4Demuxer {
    fn push(&mut self, data: &[u8]) -> Result<()> {
        self.buf.extend_from_slice(data);
        self.parse()
    }
    fn pop(&mut self) -> Option<AccessUnit> {
        self.out.pop_front()
    }
    fn tracks(&self) -> &[TrackInfo] {
        &self.infos
    }
    fn index(&self) -> Option<&[SegmentRef]> {
        self.index.as_deref()
    }
    fn reset_to(&mut self, offset: u64) {
        self.buf.clear();
        self.pending.clear();
        self.out.clear();
        self.skip = 0;
        self.pos = offset;
    }
    fn duration_ns(&self) -> Option<i64> {
        self.duration_ns.or_else(|| {
            let ix = self.index.as_ref()?;
            ix.last().map(|s| s.start_ns + s.duration_ns)
        })
    }
}

/// (type, header length, total size; `None` = to end of file) of the box at the front of `b`.
fn box_header(b: &[u8]) -> Option<([u8; 4], usize, Option<u64>)> {
    if b.len() < 8 {
        return None;
    }
    let size = u32::from_be_bytes(b[0..4].try_into().unwrap()) as u64;
    let ty: [u8; 4] = b[4..8].try_into().unwrap();
    match size {
        0 => Some((ty, 8, None)),
        1 => {
            if b.len() < 16 {
                return None;
            }
            Some((ty, 16, Some(u64::from_be_bytes(b[8..16].try_into().unwrap()))))
        }
        s => Some((ty, 8, Some(s))),
    }
}

/// Iterates the child boxes of a box body (stops at the first malformed one).
pub(crate) fn boxes(mut b: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    std::iter::from_fn(move || {
        let (ty, hdr, size) = box_header(b)?;
        let size = size.map(|s| s as usize).unwrap_or(b.len());
        if size < hdr || size > b.len() {
            return None;
        }
        let body = &b[hdr..size];
        b = &b[size..];
        Some((ty, body))
    })
}

fn child<'a>(b: &'a [u8], ty: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(b).find(|(t, _)| t == ty).map(|(_, b)| b)
}

/// `sidx` body → absolute segment byte ranges (`end` = absolute offset just past the box).
pub fn parse_sidx(b: &[u8], end: u64) -> Result<Vec<SegmentRef>> {
    let mut r = Rd::new(b);
    let v = r.u8().unwrap_or(0);
    r.skip(3);
    let _ref_id = r.u32();
    let ts = r.u32().unwrap_or(0) as i64;
    let (ept, first) = if v == 0 {
        (r.u32().map(u64::from), r.u32().map(u64::from))
    } else {
        (r.u64(), r.u64())
    };
    let (Some(ept), Some(first)) = (ept, first) else { bail!("mp4: truncated sidx") };
    r.skip(2);
    let n = r.u16().unwrap_or(0);
    let mut out = Vec::with_capacity(n as usize);
    let mut off = end + first;
    let mut t = ept as i64;
    for _ in 0..n {
        let (Some(a), Some(dur), Some(_sap)) = (r.u32(), r.u32(), r.u32()) else { bail!("mp4: truncated sidx") };
        let size = (a & 0x7fff_ffff) as u64;
        out.push(SegmentRef {
            start_ns: scale(t, 1_000_000_000, ts),
            duration_ns: scale(dur as i64, 1_000_000_000, ts),
            offset: off,
            size,
        });
        off += size;
        t += dur as i64;
    }
    Ok(out)
}

fn parse_trak(b: &[u8]) -> Option<Trak> {
    let tkhd = child(b, b"tkhd")?;
    let mut r = Rd::new(tkhd);
    let v = r.u8()?;
    r.skip(3);
    r.skip(if v == 1 { 16 } else { 8 });
    let id = r.u32()? as u64;
    let mdia = child(b, b"mdia")?;
    let mdhd = child(mdia, b"mdhd")?;
    let mut r = Rd::new(mdhd);
    let v = r.u8()?;
    r.skip(3);
    r.skip(if v == 1 { 16 } else { 8 });
    let timescale = r.u32()?;
    let hdlr = child(mdia, b"hdlr")?;
    let handler = hdlr.get(8..12)?;
    let kind = match handler {
        b"vide" => Kind::Video,
        b"soun" => Kind::Audio,
        _ => return None,
    };
    let stsd = child(child(child(mdia, b"minf")?, b"stbl")?, b"stsd")?;
    let (fourcc, entry) = boxes(stsd.get(8..)?).next()?;
    let mut info = TrackInfo {
        id,
        kind,
        codec: Codec::Other(String::from_utf8_lossy(&fourcc).into_owned()),
        config: Vec::new(),
        width: 0,
        height: 0,
        sample_rate: 0,
        channels: 0,
        default_duration_ns: 0,
        color: None,
    };
    match kind {
        Kind::Video => parse_visual_entry(&fourcc, entry, &mut info)?,
        Kind::Audio => parse_audio_entry(&fourcc, entry, &mut info)?,
    }
    let mut edit_offset = 0;
    if let Some(elst) = child(b, b"edts").and_then(|e| child(e, b"elst")) {
        let mut r = Rd::new(elst);
        let v = r.u8()?;
        r.skip(3);
        let n = r.u32()?;
        for _ in 0..n {
            let (_dur, mt) = if v == 1 { (r.u64()?, r.u64()? as i64) } else { (r.u32()? as u64, r.u32()? as i32 as i64) };
            r.skip(4);
            if mt >= 0 {
                // Edit list media_time is in the track's timescale.
                edit_offset = mt;
                break;
            }
        }
    }
    let nal_len = (info.codec == Codec::H264).then(|| super::framing::AvcConfig::parse(&info.config).map_or(4, |c| c.nal_len));
    Some(Trak { info, timescale, edit_offset, trex: Trex::default(), nal_len })
}

fn parse_visual_entry(fourcc: &[u8; 4], entry: &[u8], info: &mut TrackInfo) -> Option<()> {
    let mut r = Rd::new(entry);
    r.skip(24)?;
    info.width = r.u16()? as u32;
    info.height = r.u16()? as u32;
    let kids = entry.get(78..)?;
    let get = |t: &[u8; 4]| child(kids, t).map(<[u8]>::to_vec);
    match fourcc {
        b"avc1" | b"avc3" => {
            info.codec = Codec::H264;
            info.config = get(b"avcC")?;
        }
        b"vp09" => {
            info.codec = Codec::Vp9;
            let c = get(b"vpcC")?;
            // vpcC (version 1): profile, level, bitDepth|chroma|fullRange, primaries, transfer, matrix.
            if c.len() >= 10 {
                info.color = Some(Color { primaries: c[7], transfer: c[8], matrix: c[9], full_range: c[6] & 1 != 0 });
            }
            info.config = c;
        }
        b"av01" => {
            info.codec = Codec::Av1;
            info.config = get(b"av1C")?;
        }
        _ => {}
    }
    if let Some(colr) = child(kids, b"colr") {
        if colr.get(0..4) == Some(b"nclx") && colr.len() >= 11 {
            info.color = Some(Color {
                primaries: colr[5],
                transfer: colr[7],
                matrix: colr[9],
                full_range: colr[10] & 0x80 != 0,
            });
        }
    }
    Some(())
}

fn parse_audio_entry(fourcc: &[u8; 4], entry: &[u8], info: &mut TrackInfo) -> Option<()> {
    let mut r = Rd::new(entry);
    r.skip(8)?;
    let ver = r.u16()?;
    r.skip(6)?;
    info.channels = r.u16()?;
    r.skip(6)?;
    info.sample_rate = r.u32()? >> 16;
    let kids_at = match ver {
        1 => 28 + 16,
        2 => 28 + 36,
        _ => 28,
    };
    let kids = entry.get(kids_at..)?;
    match fourcc {
        b"mp4a" => {
            info.codec = Codec::Aac;
            let esds = child(kids, b"esds")?;
            info.config = esds_asc(esds)?;
            if let Some(c) = super::framing::AacConfig::parse(&info.config) {
                if c.sample_rate != 0 {
                    info.sample_rate = c.sample_rate;
                }
                if c.channels != 0 {
                    info.channels = c.channels as u16;
                }
            }
        }
        b"Opus" => {
            info.codec = Codec::Opus;
            let dops = child(kids, b"dOps")?;
            info.config = super::framing::opus_head_from_dops(dops)?;
            info.channels = dops.get(1).copied().unwrap_or(2) as u16;
            info.sample_rate = 48_000;
        }
        _ => {}
    }
    Some(())
}

/// The AudioSpecificConfig inside an `esds` box (ES_Descriptor → DecoderConfigDescriptor →
/// DecoderSpecificInfo).
fn esds_asc(esds: &[u8]) -> Option<Vec<u8>> {
    let mut r = Rd::new(esds.get(4..)?);
    fn desc(r: &mut Rd) -> Option<(u8, usize)> {
        let tag = r.u8()?;
        let mut len = 0usize;
        for _ in 0..4 {
            let b = r.u8()?;
            len = (len << 7) | (b & 0x7f) as usize;
            if b & 0x80 == 0 {
                break;
            }
        }
        Some((tag, len))
    }
    let (tag, _) = desc(&mut r)?;
    if tag != 3 {
        return None;
    }
    r.skip(2)?;
    let fl = r.u8()?;
    if fl & 0x80 != 0 {
        r.skip(2)?;
    }
    if fl & 0x40 != 0 {
        let n = r.u8()? as usize;
        r.skip(n)?;
    }
    if fl & 0x20 != 0 {
        r.skip(2)?;
    }
    let (tag, _) = desc(&mut r)?;
    if tag != 4 {
        return None;
    }
    r.skip(13)?;
    let (tag, len) = desc(&mut r)?;
    if tag != 5 {
        return None;
    }
    r.bytes(len).map(<[u8]>::to_vec)
}

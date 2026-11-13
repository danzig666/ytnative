//! WebM / Matroska as YouTube serves it: EBML header, `Segment` with `SeekHead`, `Info`,
//! `Tracks`, `Cues` and `Cluster`s of `SimpleBlock`s (or `BlockGroup`s).
//!
//! Incremental: `Segment` and `Cluster` are entered without being buffered (either may have an
//! unknown size); every other element is parsed once complete, or skipped byte-wise.

use super::{AccessUnit, Codec, Color, Demuxer, Kind, SegmentRef, TrackInfo};
use anyhow::{bail, Result};
use std::collections::VecDeque;

pub const ID_EBML: u32 = 0x1A45_DFA3;
pub const ID_SEGMENT: u32 = 0x1853_8067;
pub const ID_INFO: u32 = 0x1549_A966;
pub const ID_TRACKS: u32 = 0x1654_AE6B;
pub const ID_CUES: u32 = 0x1C53_BB6B;
pub const ID_CLUSTER: u32 = 0x1F43_B675;
const ID_TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
const ID_DURATION: u32 = 0x4489;
const ID_TRACK_ENTRY: u32 = 0xAE;
const ID_TRACK_NUMBER: u32 = 0xD7;
const ID_TRACK_TYPE: u32 = 0x83;
const ID_CODEC_ID: u32 = 0x86;
const ID_CODEC_PRIVATE: u32 = 0x63A2;
const ID_DEFAULT_DURATION: u32 = 0x23_E383;
const ID_CODEC_DELAY: u32 = 0x56AA;
const ID_VIDEO: u32 = 0xE0;
const ID_PIXEL_WIDTH: u32 = 0xB0;
const ID_PIXEL_HEIGHT: u32 = 0xBA;
const ID_COLOUR: u32 = 0x55B0;
const ID_MATRIX: u32 = 0x55B1;
const ID_RANGE: u32 = 0x55B9;
const ID_TRANSFER: u32 = 0x55BA;
const ID_PRIMARIES: u32 = 0x55BB;
const ID_AUDIO: u32 = 0xE1;
const ID_SAMPLING_FREQ: u32 = 0xB5;
const ID_CHANNELS: u32 = 0x9F;
const ID_CUE_POINT: u32 = 0xBB;
const ID_CUE_TIME: u32 = 0xB3;
const ID_CUE_TRACK_POSITIONS: u32 = 0xB7;
const ID_CUE_CLUSTER_POSITION: u32 = 0xF1;
pub const ID_TIMESTAMP: u32 = 0xE7;
pub const ID_SIMPLE_BLOCK: u32 = 0xA3;
pub const ID_BLOCK_GROUP: u32 = 0xA0;
const ID_BLOCK: u32 = 0xA1;
const ID_BLOCK_DURATION: u32 = 0x9B;
const ID_REFERENCE_BLOCK: u32 = 0xFB;

/// Top-level children of a Segment; seeing one while inside an unknown-size Cluster ends it.
const SEGMENT_CHILDREN: [u32; 8] = [0x114D_9B74, ID_INFO, ID_TRACKS, ID_CUES, ID_CLUSTER, 0x1043_A770, 0x1941_A469, 0x1254_C367];

/// Reads an element ID (marker bits kept). Returns (id, length).
pub fn read_id(b: &[u8]) -> Option<(u32, usize)> {
    let first = *b.first()?;
    let len = first.leading_zeros() as usize + 1;
    if len > 4 || b.len() < len {
        return None;
    }
    let mut v = 0u32;
    for &x in &b[..len] {
        v = (v << 8) | x as u32;
    }
    Some((v, len))
}

/// Reads a data-size / track-number vint (marker removed). `None` value = unknown size.
pub fn read_vint(b: &[u8]) -> Option<(Option<u64>, usize)> {
    let first = *b.first()?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 || b.len() < len {
        return None;
    }
    let mut v = (first as u64) & (0xff >> len);
    let mut all_ones = v == (0xff >> len) as u64;
    for &x in &b[1..len] {
        v = (v << 8) | x as u64;
        all_ones &= x == 0xff;
    }
    Some((if all_ones { None } else { Some(v) }, len))
}

fn uint(b: &[u8]) -> u64 {
    b.iter().take(8).fold(0u64, |a, &x| (a << 8) | x as u64)
}

fn float(b: &[u8]) -> f64 {
    match b.len() {
        4 => f32::from_be_bytes(b.try_into().unwrap()) as f64,
        8 => f64::from_be_bytes(b.try_into().unwrap()),
        _ => 0.0,
    }
}

/// Iterates complete child elements of a master element body: (id, body).
pub fn elements(mut b: &[u8]) -> impl Iterator<Item = (u32, &[u8])> {
    std::iter::from_fn(move || {
        let (id, il) = read_id(b)?;
        let (size, sl) = read_vint(&b[il..])?;
        let start = il + sl;
        let end = match size {
            Some(s) => start.checked_add(s as usize)?,
            None => b.len(),
        };
        if end > b.len() {
            return None;
        }
        let body = &b[start..end];
        b = &b[end..];
        Some((id, body))
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Level {
    Top,
    Segment,
    Cluster,
}

pub struct MkvDemuxer {
    buf: Vec<u8>,
    pos: u64,
    level: Level,
    skip: u64,
    /// Absolute offset of the Segment's data (Cues positions are relative to it).
    segment_data: Option<u64>,
    segment_end: Option<u64>,
    cluster_end: Option<u64>,
    cluster_ts: i64,
    timestamp_scale: i64,
    duration_ns: Option<i64>,
    infos: Vec<TrackInfo>,
    /// Per track: CodecDelay (ns), subtracted from every block timestamp (Matroska spec).
    delays: Vec<(u64, i64)>,
    cues: Vec<(i64, u64)>,
    index: Option<Vec<SegmentRef>>,
    out: VecDeque<AccessUnit>,
}

impl Default for MkvDemuxer {
    fn default() -> Self {
        Self::new()
    }
}

impl MkvDemuxer {
    pub fn new() -> Self {
        MkvDemuxer {
            buf: Vec::new(),
            pos: 0,
            level: Level::Top,
            skip: 0,
            segment_data: None,
            segment_end: None,
            cluster_end: None,
            cluster_ts: 0,
            timestamp_scale: 1_000_000,
            duration_ns: None,
            infos: Vec::new(),
            delays: Vec::new(),
            cues: Vec::new(),
            index: None,
            out: VecDeque::new(),
        }
    }

    fn consume(&mut self, n: usize) {
        self.buf.drain(..n);
        self.pos += n as u64;
    }

    fn parse(&mut self) -> Result<()> {
        loop {
            if self.skip > 0 {
                let n = self.skip.min(self.buf.len() as u64) as usize;
                self.consume(n);
                self.skip -= n as u64;
                if self.skip > 0 {
                    return Ok(());
                }
            }
            if self.level == Level::Cluster && self.cluster_end.is_some_and(|e| self.pos >= e) {
                self.level = Level::Segment;
                self.cluster_end = None;
            }
            let Some((id, il)) = read_id(&self.buf) else {
                // Not enough bytes yet, unless the first byte can never start an id (0x00, or a
                // length over 4): then the stream is broken and waiting would buffer forever.
                if self.buf.first().is_some_and(|b| b.leading_zeros() >= 4) {
                    let at = self.pos;
                    self.buf.clear();
                    bail!("mkv: invalid element id at {at}");
                }
                return Ok(());
            };
            let Some((size, sl)) = read_vint(&self.buf[il..]) else {
                if self.buf.get(il) == Some(&0) {
                    let at = self.pos;
                    self.buf.clear();
                    bail!("mkv: invalid element size at {at}");
                }
                return Ok(());
            };
            let hdr = il + sl;
            match id {
                ID_EBML => {
                    // A (re-sent) init segment: start over at the top level, keep nothing.
                    self.level = Level::Top;
                }
                ID_SEGMENT => {
                    self.consume(hdr);
                    self.segment_data = Some(self.pos);
                    self.segment_end = size.map(|s| self.pos + s);
                    self.level = Level::Segment;
                    continue;
                }
                ID_CLUSTER => {
                    self.consume(hdr);
                    self.level = Level::Cluster;
                    self.cluster_end = size.map(|s| self.pos + s);
                    self.cluster_ts = 0;
                    continue;
                }
                _ => {}
            }
            if self.level == Level::Cluster && SEGMENT_CHILDREN.contains(&id) {
                self.level = Level::Segment;
            }
            let Some(size) = size else {
                // Unknown-size element we do not enter: cannot be delimited.
                let at = self.pos;
                self.buf.clear();
                bail!("mkv: unknown-size element {id:#x} at {at}");
            };
            let total = hdr as u64 + size;
            let wanted = match self.level {
                Level::Top => id == ID_EBML,
                Level::Segment => matches!(id, ID_INFO | ID_TRACKS | ID_CUES),
                Level::Cluster => matches!(id, ID_TIMESTAMP | ID_SIMPLE_BLOCK | ID_BLOCK_GROUP),
            };
            if !wanted {
                self.skip = total;
                continue;
            }
            if (self.buf.len() as u64) < total {
                return Ok(());
            }
            let body = self.buf[hdr..total as usize].to_vec();
            let at = self.pos;
            self.consume(total as usize);
            match id {
                ID_INFO => self.parse_info(&body),
                ID_TRACKS => self.parse_tracks(&body),
                ID_CUES => self.parse_cues(&body),
                ID_TIMESTAMP => self.cluster_ts = uint(&body) as i64,
                ID_SIMPLE_BLOCK => self.parse_block(&body, None, None),
                ID_BLOCK_GROUP => {
                    let mut block = None;
                    let mut dur = None;
                    let mut has_ref = false;
                    for (cid, b) in elements(&body) {
                        match cid {
                            ID_BLOCK => block = Some(b),
                            ID_BLOCK_DURATION => dur = Some(uint(b) as i64),
                            ID_REFERENCE_BLOCK => has_ref = true,
                            _ => {}
                        }
                    }
                    if let Some(b) = block {
                        self.parse_block(b, Some(!has_ref), dur);
                    }
                }
                ID_EBML => {}
                _ => {}
            }
            let _ = at;
        }
    }

    fn parse_info(&mut self, body: &[u8]) {
        let mut dur = None;
        for (id, b) in elements(body) {
            match id {
                ID_TIMESTAMP_SCALE => self.timestamp_scale = uint(b).max(1) as i64,
                ID_DURATION => dur = Some(float(b)),
                _ => {}
            }
        }
        if let Some(d) = dur {
            self.duration_ns = Some((d * self.timestamp_scale as f64) as i64);
        }
        self.build_index();
    }

    fn parse_tracks(&mut self, body: &[u8]) {
        let mut infos = Vec::new();
        let mut delays = Vec::new();
        for (id, te) in elements(body) {
            if id != ID_TRACK_ENTRY {
                continue;
            }
            let mut t = TrackInfo {
                id: 0,
                kind: Kind::Video,
                codec: Codec::Other(String::new()),
                config: Vec::new(),
                width: 0,
                height: 0,
                sample_rate: 0,
                channels: 0,
                default_duration_ns: 0,
                color: None,
            };
            let mut ty = 0;
            let mut delay = 0;
            for (id, b) in elements(te) {
                match id {
                    ID_TRACK_NUMBER => t.id = uint(b),
                    ID_TRACK_TYPE => ty = uint(b),
                    ID_CODEC_ID => {
                        let s = String::from_utf8_lossy(b).trim_end_matches('\0').to_string();
                        t.codec = match s.as_str() {
                            "V_VP9" => Codec::Vp9,
                            "V_AV1" => Codec::Av1,
                            "V_MPEG4/ISO/AVC" => Codec::H264,
                            "A_OPUS" => Codec::Opus,
                            "A_AAC" => Codec::Aac,
                            _ => Codec::Other(s),
                        };
                    }
                    ID_CODEC_PRIVATE => t.config = b.to_vec(),
                    ID_DEFAULT_DURATION => t.default_duration_ns = uint(b) as i64,
                    ID_CODEC_DELAY => delay = uint(b) as i64,
                    ID_VIDEO => {
                        for (id, b) in elements(b) {
                            match id {
                                ID_PIXEL_WIDTH => t.width = uint(b) as u32,
                                ID_PIXEL_HEIGHT => t.height = uint(b) as u32,
                                ID_COLOUR => {
                                    let mut c = Color { primaries: 2, transfer: 2, matrix: 2, full_range: false };
                                    for (id, b) in elements(b) {
                                        match id {
                                            ID_MATRIX => c.matrix = uint(b) as u8,
                                            ID_TRANSFER => c.transfer = uint(b) as u8,
                                            ID_PRIMARIES => c.primaries = uint(b) as u8,
                                            ID_RANGE => c.full_range = uint(b) == 2,
                                            _ => {}
                                        }
                                    }
                                    t.color = Some(c);
                                }
                                _ => {}
                            }
                        }
                    }
                    ID_AUDIO => {
                        for (id, b) in elements(b) {
                            match id {
                                ID_SAMPLING_FREQ => t.sample_rate = float(b) as u32,
                                ID_CHANNELS => t.channels = uint(b) as u16,
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            if delay != 0 {
                delays.push((t.id, delay));
            }
            t.kind = match ty {
                1 => Kind::Video,
                2 => Kind::Audio,
                _ => continue,
            };
            if t.codec == Codec::Opus {
                if let Some(h) = super::framing::OpusHead::parse(&t.config) {
                    t.channels = h.channels as u16;
                }
                t.sample_rate = 48_000;
            }
            infos.push(t);
        }
        self.infos = infos;
        self.delays = delays;
    }

    fn parse_cues(&mut self, body: &[u8]) {
        let mut cues = Vec::new();
        for (id, cp) in elements(body) {
            if id != ID_CUE_POINT {
                continue;
            }
            let mut time = None;
            let mut pos = None;
            for (id, b) in elements(cp) {
                match id {
                    ID_CUE_TIME => time = Some(uint(b) as i64),
                    ID_CUE_TRACK_POSITIONS => {
                        for (id, b) in elements(b) {
                            if id == ID_CUE_CLUSTER_POSITION {
                                pos = Some(uint(b));
                            }
                        }
                    }
                    _ => {}
                }
            }
            if let (Some(t), Some(p)) = (time, pos) {
                cues.push((t, p));
            }
        }
        cues.sort();
        cues.dedup_by_key(|c| c.1);
        self.cues = cues;
        self.build_index();
    }

    fn build_index(&mut self) {
        let Some(base) = self.segment_data else { return };
        if self.cues.is_empty() {
            return;
        }
        let ts = self.timestamp_scale;
        let mut out = Vec::with_capacity(self.cues.len());
        for (i, &(t, p)) in self.cues.iter().enumerate() {
            let next = self.cues.get(i + 1);
            let start_ns = t * ts;
            let end_ns = match next {
                Some(&(nt, _)) => nt * ts,
                None => self.duration_ns.unwrap_or(start_ns),
            };
            let size = match next {
                Some(&(_, np)) => np.saturating_sub(p),
                None => self.segment_end.map(|e| e.saturating_sub(base + p)).unwrap_or(0),
            };
            out.push(SegmentRef { start_ns, duration_ns: end_ns - start_ns, offset: base + p, size });
        }
        self.index = Some(out);
    }

    /// A Block / SimpleBlock body. `key` overrides the SimpleBlock flag (BlockGroup semantics).
    fn parse_block(&mut self, b: &[u8], key: Option<bool>, dur_ticks: Option<i64>) {
        let Some((Some(track), tl)) = read_vint(b) else { return };
        if b.len() < tl + 3 {
            return;
        }
        let rel = i16::from_be_bytes([b[tl], b[tl + 1]]) as i64;
        let flags = b[tl + 2];
        let Some(t) = self.infos.iter().find(|t| t.id == track) else { return };
        let (kind, def_dur) = (t.kind, t.default_duration_ns);
        let keyframe = key.unwrap_or(flags & 0x80 != 0) || kind == Kind::Audio;
        let ts = self.timestamp_scale;
        let delay = self.delays.iter().find(|d| d.0 == track).map_or(0, |d| d.1);
        let pts = (self.cluster_ts + rel) * ts - delay;
        let dur = dur_ticks.map(|d| d * ts).unwrap_or(def_dur);
        let payload = &b[tl + 3..];
        let frames = match (flags >> 1) & 3 {
            0 => vec![payload],
            lacing => match unlace(payload, lacing) {
                Some(f) => f,
                None => return,
            },
        };
        let n = frames.len() as i64;
        let each = if n > 1 { if def_dur > 0 { def_dur } else { dur / n } } else { dur };
        for (i, f) in frames.into_iter().enumerate() {
            let p = pts + each * i as i64;
            self.out.push_back(AccessUnit {
                track,
                kind,
                pts_ns: p,
                dts_ns: p,
                duration_ns: each,
                keyframe: keyframe && i == 0 || kind == Kind::Audio,
                data: f.to_vec(),
            });
        }
    }
}

/// Splits a laced block payload (1 = Xiph, 2 = fixed-size, 3 = EBML).
fn unlace(p: &[u8], lacing: u8) -> Option<Vec<&[u8]>> {
    let count = *p.first()? as usize + 1;
    let mut at = 1;
    let mut sizes = Vec::with_capacity(count);
    match lacing {
        1 => {
            for _ in 0..count - 1 {
                let mut s = 0usize;
                loop {
                    let x = *p.get(at)?;
                    at += 1;
                    s += x as usize;
                    if x != 255 {
                        break;
                    }
                }
                sizes.push(s);
            }
        }
        2 => {
            let rest = p.len() - 1;
            if rest % count != 0 {
                return None;
            }
            sizes = vec![rest / count; count - 1];
        }
        3 => {
            let (first, l) = read_vint(&p[at..])?;
            at += l;
            let mut prev = first? as i64;
            sizes.push(prev as usize);
            for _ in 1..count - 1 {
                let (v, l) = read_vint(&p[at..])?;
                let raw = v? as i64;
                at += l;
                // Signed vint: subtract the bias (2^(7l-1) - 1).
                let bias = (1i64 << (7 * l - 1)) - 1;
                prev += raw - bias;
                if prev < 0 {
                    return None;
                }
                sizes.push(prev as usize);
            }
        }
        _ => return None,
    }
    let used: usize = sizes.iter().sum();
    let last = p.len().checked_sub(at + used)?;
    sizes.push(last);
    let mut out = Vec::with_capacity(count);
    for s in sizes {
        out.push(p.get(at..at + s)?);
        at += s;
    }
    Some(out)
}

impl Demuxer for MkvDemuxer {
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
        self.out.clear();
        self.skip = 0;
        self.pos = offset;
        self.level = if self.segment_data.is_some() { Level::Segment } else { Level::Top };
        self.cluster_end = None;
    }
    fn duration_ns(&self) -> Option<i64> {
        self.duration_ns
    }
}

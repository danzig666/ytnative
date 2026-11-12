//! From container samples to decoder input, the way LG's BUFFERSTREAM pipeline (and plx-native's
//! Feed engine, which this follows) wants it:
//!
//! - H.264: `avcC` length-prefixed NAL units → Annex-B (4-byte start codes), SPS+PPS from the
//!   `avcC` in front of every IDR that does not already carry them.
//! - AV1: Section-5 low-overhead temporal units: a temporal delimiter, the `av1C` sequence header
//!   (+ metadata OBUs) in front of every keyframe that lacks one, every OBU with a size field. The
//!   TV pipeline gets no codec_data for AV1, so the configuration must travel in-band.
//! - VP9: frames as stored (a superframe is passed whole; [`vp9_split_superframe`] exists for
//!   tools that want single frames).
//! - AAC: a 7-byte ADTS header built from the AudioSpecificConfig.
//! - Opus: raw packets; the `OpusHead` (RFC 7845) travels in the Load payload's `opusInfo`.

use super::{AccessUnit, Codec, Rd, TrackInfo};
use anyhow::{bail, Result};

/// Per-track framer: holds the parsed decoder configuration.
#[derive(Clone, Debug)]
pub enum Framer {
    H264(AvcConfig),
    Av1 { config_obus: Vec<u8> },
    Vp9,
    Aac(AacConfig),
    Opus,
}

impl Framer {
    pub fn new(t: &TrackInfo) -> Result<Framer> {
        Ok(match &t.codec {
            Codec::H264 => match AvcConfig::parse(&t.config) {
                Some(c) => Framer::H264(c),
                None => bail!("H.264 track without a usable avcC"),
            },
            Codec::Av1 => Framer::Av1 { config_obus: av1c_config_obus(&t.config) },
            Codec::Vp9 => Framer::Vp9,
            Codec::Aac => match AacConfig::parse(&t.config) {
                Some(c) => Framer::Aac(c),
                None => bail!("AAC track without a usable AudioSpecificConfig"),
            },
            Codec::Opus => Framer::Opus,
            Codec::Other(s) => bail!("unsupported codec {s:?}"),
        })
    }

    /// The decoder-ready bytes of one access unit.
    pub fn frame(&self, au: &AccessUnit) -> Vec<u8> {
        let mut out = Vec::with_capacity(au.data.len() + 64);
        match self {
            Framer::H264(c) => h264_to_annexb(c, &au.data, au.keyframe, &mut out),
            Framer::Av1 { config_obus } => av1_to_tu(&au.data, au.keyframe, config_obus, &mut out),
            Framer::Vp9 | Framer::Opus => out.extend_from_slice(&au.data),
            Framer::Aac(c) => {
                out.extend_from_slice(&c.adts_header(au.data.len()));
                out.extend_from_slice(&au.data);
            }
        }
        out
    }
}

// ---- H.264 -------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AvcConfig {
    pub profile: u8,
    pub compat: u8,
    pub level: u8,
    pub nal_len: usize,
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
}

impl AvcConfig {
    pub fn parse(b: &[u8]) -> Option<AvcConfig> {
        let mut r = Rd::new(b);
        if r.u8()? != 1 {
            return None;
        }
        let (profile, compat, level) = (r.u8()?, r.u8()?, r.u8()?);
        let nal_len = (r.u8()? & 3) as usize + 1;
        let nsps = r.u8()? & 0x1f;
        let mut sps = Vec::new();
        for _ in 0..nsps {
            let n = r.u16()? as usize;
            sps.push(r.bytes(n)?.to_vec());
        }
        let npps = r.u8()?;
        let mut pps = Vec::new();
        for _ in 0..npps {
            let n = r.u16()? as usize;
            pps.push(r.bytes(n)?.to_vec());
        }
        Some(AvcConfig { profile, compat, level, nal_len, sps, pps })
    }

    /// SPS+PPS as Annex-B.
    pub fn annexb_parameter_sets(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for n in self.sps.iter().chain(&self.pps) {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(n);
        }
        out
    }
}

/// Length-prefixed NALs → Annex-B; parameter sets prepended on keyframes that lack an SPS.
pub fn h264_to_annexb(c: &AvcConfig, data: &[u8], key: bool, out: &mut Vec<u8>) {
    let nals = split_length_prefixed(data, c.nal_len);
    if key && !nals.iter().any(|n| n.first().map(|h| h & 0x1f) == Some(7)) {
        // Keep an access unit delimiter first if the sample starts with one.
        let mut rest = &nals[..];
        if let Some(first) = nals.first() {
            if first.first().map(|h| h & 0x1f) == Some(9) {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.extend_from_slice(first);
                rest = &nals[1..];
            }
        }
        out.extend_from_slice(&c.annexb_parameter_sets());
        for n in rest {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(n);
        }
        return;
    }
    for n in nals {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(n);
    }
}

/// Whether a length-prefixed H.264 access unit contains an IDR slice (NAL type 5).
pub fn h264_has_idr(data: &[u8], nal_len: usize) -> bool {
    split_length_prefixed(data, nal_len).iter().any(|n| n.first().map(|h| h & 0x1f) == Some(5))
}

pub fn split_length_prefixed(mut b: &[u8], len: usize) -> Vec<&[u8]> {
    let mut out = Vec::new();
    while b.len() >= len {
        let n = b[..len].iter().fold(0usize, |a, &x| (a << 8) | x as usize);
        let Some(nal) = b.get(len..len + n) else { break };
        if n > 0 {
            out.push(nal);
        }
        b = &b[len + n..];
    }
    out
}

// ---- AV1 ---------------------------------------------------------------------------------

pub const OBU_SEQUENCE_HEADER: u8 = 1;
pub const OBU_TEMPORAL_DELIMITER: u8 = 2;
pub const OBU_METADATA: u8 = 5;
/// A temporal delimiter OBU with an empty payload: header 0x12 (type 2, has_size_field), size 0.
pub const AV1_TD: [u8; 2] = [0x12, 0x00];

pub fn read_leb128(b: &[u8]) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for (i, &x) in b.iter().take(8).enumerate() {
        v |= u64::from(x & 0x7f) << (7 * i);
        if x & 0x80 == 0 {
            return (v <= u64::from(u32::MAX)).then_some((v, i + 1));
        }
    }
    None
}

pub fn write_leb128(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Obu {
    pub start: usize,
    pub header_len: usize,
    pub payload: usize,
    pub end: usize,
    pub obu_type: u8,
    pub has_size: bool,
}

/// Splits an OBU run. An OBU without a size field extends to the end of the run.
pub fn walk_obus(b: &[u8]) -> Option<Vec<Obu>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < b.len() {
        let h = b[at];
        if h & 0x80 != 0 {
            return None; // forbidden bit
        }
        let obu_type = (h >> 3) & 0x0f;
        let ext = h & 0x04 != 0;
        let has_size = h & 0x02 != 0;
        let header_len = 1 + ext as usize;
        let mut p = at + header_len;
        if p > b.len() {
            return None;
        }
        let end = if has_size {
            let (n, l) = read_leb128(&b[p..])?;
            p += l;
            p.checked_add(n as usize)?
        } else {
            b.len()
        };
        if end > b.len() {
            return None;
        }
        out.push(Obu { start: at, header_len, payload: p, end, obu_type, has_size });
        at = end;
    }
    Some(out)
}

fn push_obu_sized(b: &[u8], o: &Obu, out: &mut Vec<u8>) {
    if o.has_size {
        out.extend_from_slice(&b[o.start..o.end]);
        return;
    }
    out.push(b[o.start] | 0x02);
    out.extend_from_slice(&b[o.start + 1..o.start + o.header_len]);
    write_leb128((o.end - o.payload) as u64, out);
    out.extend_from_slice(&b[o.payload..o.end]);
}

/// The configOBUs of an `av1C` record (or a bare OBU run): every sequence header, then every
/// metadata OBU, each with a size field. Empty when there is no sequence header.
pub fn av1c_config_obus(rec: &[u8]) -> Vec<u8> {
    let obus = if rec.len() >= 4 && rec[0] == 0x81 { &rec[4..] } else { rec };
    let Some(spans) = walk_obus(obus) else { return Vec::new() };
    if !spans.iter().any(|s| s.obu_type == OBU_SEQUENCE_HEADER) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for s in spans.iter().filter(|s| s.obu_type == OBU_SEQUENCE_HEADER) {
        push_obu_sized(obus, s, &mut out);
    }
    for s in spans.iter().filter(|s| s.obu_type == OBU_METADATA) {
        push_obu_sized(obus, s, &mut out);
    }
    out
}

/// One AV1 sample → a temporal unit. A sample that does not parse is copied unchanged.
pub fn av1_to_tu(pkt: &[u8], key: bool, cfg: &[u8], out: &mut Vec<u8>) {
    let Some(spans) = walk_obus(pkt) else {
        out.extend_from_slice(pkt);
        return;
    };
    let has_td = spans.first().is_some_and(|s| s.obu_type == OBU_TEMPORAL_DELIMITER);
    let has_seq = spans.iter().any(|s| s.obu_type == OBU_SEQUENCE_HEADER);
    let mut rest = &spans[..];
    if has_td {
        push_obu_sized(pkt, &spans[0], out);
        rest = &spans[1..];
    } else {
        out.extend_from_slice(&AV1_TD);
    }
    if key && !has_seq && !cfg.is_empty() {
        out.extend_from_slice(cfg);
    }
    for s in rest {
        push_obu_sized(pkt, s, out);
    }
}

// ---- VP9 ---------------------------------------------------------------------------------

/// Splits a VP9 superframe (Annex B of the VP9 spec) into its frames; a plain frame is returned
/// as the only element.
pub fn vp9_split_superframe(b: &[u8]) -> Vec<&[u8]> {
    let Some(&last) = b.last() else { return Vec::new() };
    if last & 0xe0 == 0xc0 {
        let frames = (last & 0x7) as usize + 1;
        let mag = ((last >> 3) & 0x3) as usize + 1;
        let index_len = 2 + mag * frames;
        if b.len() >= index_len && b[b.len() - index_len] == last {
            let mut sizes = Vec::with_capacity(frames);
            let mut p = b.len() - index_len + 1;
            for _ in 0..frames {
                let mut s = 0usize;
                for i in 0..mag {
                    s |= (b[p + i] as usize) << (8 * i);
                }
                p += mag;
                sizes.push(s);
            }
            let mut out = Vec::with_capacity(frames);
            let mut at = 0;
            for s in sizes {
                if at + s > b.len() - index_len {
                    return vec![b];
                }
                out.push(&b[at..at + s]);
                at += s;
            }
            return out;
        }
    }
    vec![b]
}

/// Whether a VP9 frame (not a superframe) is a keyframe: the uncompressed header's frame_type.
pub fn vp9_is_keyframe(f: &[u8]) -> bool {
    let Some(&b0) = f.first() else { return false };
    if b0 >> 6 != 2 {
        return false; // frame marker
    }
    let profile = ((b0 >> 5) & 1) | (((b0 >> 4) & 1) << 1);
    let mut bit = 4;
    if profile == 3 {
        bit += 1;
    }
    let get = |i: usize| (f.get(i / 8).copied().unwrap_or(0) >> (7 - i % 8)) & 1;
    if get(bit) == 1 {
        return false; // show_existing_frame
    }
    get(bit + 1) == 0
}

// ---- AAC ---------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AacConfig {
    /// Audio object type of the core coder (2 = LC); SBR/PS signalling is folded away.
    pub object_type: u8,
    pub freq_index: u8,
    pub sample_rate: u32,
    pub channels: u8,
}

const AAC_RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];

impl AacConfig {
    pub fn parse(asc: &[u8]) -> Option<AacConfig> {
        let mut bits = Bits { b: asc, p: 0 };
        let mut aot = bits.get(5)? as u8;
        if aot == 31 {
            aot = 32 + bits.get(6)? as u8;
        }
        let rate = |bits: &mut Bits| -> Option<(u8, u32)> {
            let i = bits.get(4)? as u8;
            if i == 15 {
                Some((15, bits.get(24)?))
            } else {
                Some((i, *AAC_RATES.get(i as usize)?))
            }
        };
        let (fi, sr) = rate(&mut bits)?;
        let channels = bits.get(4)? as u8;
        if aot == 5 || aot == 29 {
            // Explicit SBR/PS: the extension rate follows, then the core object type. ADTS
            // describes the core (LC) at the core rate.
            let _ext = rate(&mut bits)?;
            aot = bits.get(5)? as u8;
        }
        Some(AacConfig { object_type: aot, freq_index: fi, sample_rate: sr, channels })
    }

    /// ADTS header (no CRC) for a raw AAC frame of `payload_len` bytes.
    pub fn adts_header(&self, payload_len: usize) -> [u8; 7] {
        let len = payload_len + 7;
        let profile = self.object_type.saturating_sub(1).min(3);
        let fi = if self.freq_index < 13 { self.freq_index } else { rate_index(self.sample_rate) };
        let ch = self.channels & 7;
        [
            0xFF,
            0xF1,
            (profile << 6) | (fi << 2) | (ch >> 2),
            ((ch & 3) << 6) | ((len >> 11) as u8 & 0x3),
            (len >> 3) as u8,
            (((len & 7) as u8) << 5) | 0x1F,
            0xFC,
        ]
    }
}

fn rate_index(sr: u32) -> u8 {
    AAC_RATES.iter().position(|&r| r == sr).unwrap_or(4) as u8
}

struct Bits<'a> {
    b: &'a [u8],
    p: usize,
}

impl Bits<'_> {
    fn get(&mut self, n: usize) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *self.b.get(self.p / 8)?;
            v = (v << 1) | ((byte >> (7 - self.p % 8)) & 1) as u32;
            self.p += 1;
        }
        Some(v)
    }
}

// ---- Opus --------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpusHead {
    pub channels: u8,
    pub pre_skip: u16,
    pub input_rate: u32,
    pub gain: i16,
    pub mapping_family: u8,
}

impl OpusHead {
    pub fn parse(b: &[u8]) -> Option<OpusHead> {
        if b.len() < 19 || &b[..8] != b"OpusHead" {
            return None;
        }
        Some(OpusHead {
            channels: b[9],
            pre_skip: u16::from_le_bytes([b[10], b[11]]),
            input_rate: u32::from_le_bytes([b[12], b[13], b[14], b[15]]),
            gain: i16::from_le_bytes([b[16], b[17]]),
            mapping_family: b[18],
        })
    }
}

/// MP4 `dOps` (big-endian, version 0) → RFC 7845 `OpusHead` (little-endian, version 1).
pub fn opus_head_from_dops(d: &[u8]) -> Option<Vec<u8>> {
    let mut r = Rd::new(d);
    let _ver = r.u8()?;
    let ch = r.u8()?;
    let pre_skip = r.u16()?;
    let rate = r.u32()?;
    let gain = r.u16()?;
    let family = r.u8()?;
    let mut h = b"OpusHead".to_vec();
    h.push(1);
    h.push(ch);
    h.extend_from_slice(&pre_skip.to_le_bytes());
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&gain.to_le_bytes());
    h.push(family);
    if family != 0 {
        h.push(r.u8()?);
        h.push(r.u8()?);
        h.extend_from_slice(r.bytes(ch as usize)?);
    }
    Some(h)
}

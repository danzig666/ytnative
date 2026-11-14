//! Pure-Rust demuxers for YouTube's DASH formats, for the Feed engine (engine B), which hands raw
//! elementary streams to the TV's `StarfishMediaAPIs` (BUFFERSTREAM) instead of a manifest.
//!
//! - [`mp4`]: fragmented MP4 (`ftyp`/`moov`/`sidx`/`moof`+`mdat`): H.264, VP9, AV1, AAC, Opus.
//! - [`mkv`]: WebM/Matroska (EBML header, `Info`, `Tracks`, `Cues`, `Cluster`s of `SimpleBlock`s
//!   and `BlockGroup`s): VP9, AV1, Opus.
//! - [`framing`]: turns container samples into what a hardware decoder is fed: H.264 Annex-B
//!   with SPS/PPS in front of every IDR, AV1 Section-5 temporal units with the `av1C` sequence
//!   header in front of every keyframe, VP9 frames as they are, AAC with a 7-byte ADTS header,
//!   Opus raw packets (+ the `OpusHead` the Load payload carries).
//! - [`ranged`]: direct-URL (`adaptiveFormats[].url`) access with bounded HTTP Range requests,
//!   driven by the format's `sidx` / `Cues` index.
//! - [`select`]: which video/audio formats to play for a requested height and codec.
//!
//! Both demuxers are incremental: [`Demuxer::push`] any amount of bytes (an init segment, one
//! SABR segment, a ranged chunk, ...) and drain finished [`AccessUnit`]s with [`Demuxer::pop`].
//! [`Demuxer::reset_to`] repositions to a segment boundary (seek) while keeping the parsed init.

pub mod framing;
pub mod mkv;
pub mod mp4;
pub mod ranged;
pub mod select;

#[cfg(test)]
mod live;
#[cfg(test)]
mod tests;

use anyhow::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Video,
    Audio,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Vp9,
    Av1,
    Aac,
    Opus,
    /// A sample entry / CodecID this crate does not frame (kept for diagnostics).
    Other(String),
}

/// Colour description from the container (`colr`/`vpcC` in MP4, `Colour` in Matroska), ISO/IEC
/// 23091-2 code points. `transfer == 16` is PQ (HDR10), `18` is HLG.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Color {
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    pub full_range: bool,
}

impl Color {
    pub fn is_hdr(&self) -> bool {
        self.transfer == 16 || self.transfer == 18
    }
}

#[derive(Clone, Debug)]
pub struct TrackInfo {
    /// MP4 track_ID / Matroska TrackNumber.
    pub id: u64,
    pub kind: Kind,
    pub codec: Codec,
    /// Decoder configuration as the container stores it: `avcC`, `vpcC`, `av1C`, the AAC
    /// AudioSpecificConfig, or an RFC 7845 `OpusHead` (converted from MP4 `dOps`).
    pub config: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub sample_rate: u32,
    pub channels: u16,
    /// Nominal frame/packet duration if the container states one (or 0).
    pub default_duration_ns: i64,
    pub color: Option<Color>,
}

/// One coded frame / audio packet, in the container's representation (length-prefixed NALs for
/// H.264, raw AAC, OBUs without temporal delimiter for AV1). Run it through a
/// [`framing::Framer`] before feeding a decoder.
#[derive(Clone, Debug)]
pub struct AccessUnit {
    pub track: u64,
    pub kind: Kind,
    pub pts_ns: i64,
    pub dts_ns: i64,
    pub duration_ns: i64,
    pub keyframe: bool,
    pub data: Vec<u8>,
}

/// One indexed media segment (a `sidx` reference or a Matroska cluster from `Cues`): absolute
/// byte range in the file and its time span. `size == 0` means "to the end of the file".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentRef {
    pub start_ns: i64,
    pub duration_ns: i64,
    pub offset: u64,
    pub size: u64,
}

pub trait Demuxer: Send {
    /// Appends bytes that continue the stream at the current position.
    fn push(&mut self, data: &[u8]) -> Result<()>;
    /// Next finished access unit, in file (decode) order.
    fn pop(&mut self) -> Option<AccessUnit>;
    /// Tracks, once the init part was parsed.
    fn tracks(&self) -> &[TrackInfo];
    /// Media segment index (`sidx` / `Cues`), once parsed.
    fn index(&self) -> Option<&[SegmentRef]>;
    /// Drops buffered bytes and pending units; the next push continues at absolute byte
    /// `offset`, which must be a segment (`moof` / `Cluster`) boundary. Parsed init is kept.
    fn reset_to(&mut self, offset: u64);
    /// Total duration if the container states it.
    fn duration_ns(&self) -> Option<i64>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Mp4,
    WebM,
}

impl Container {
    /// From a `mimeType` like `video/webm; codecs="vp9"`.
    pub fn from_mime(mime: &str) -> Container {
        if mime.contains("webm") || mime.contains("matroska") {
            Container::WebM
        } else {
            Container::Mp4
        }
    }

    /// Sniffs the first bytes of a file (EBML magic vs. ISO BMFF box).
    pub fn sniff(head: &[u8]) -> Option<Container> {
        if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
            Some(Container::WebM)
        } else if head.len() >= 8 && matches!(&head[4..8], b"ftyp" | b"moov" | b"styp" | b"moof" | b"sidx") {
            Some(Container::Mp4)
        } else {
            None
        }
    }

    pub fn demuxer(self) -> Box<dyn Demuxer> {
        match self {
            Container::Mp4 => Box::new(mp4::Mp4Demuxer::new()),
            Container::WebM => Box::new(mkv::MkvDemuxer::new()),
        }
    }
}

/// Big-endian cursor over a byte slice; every read is bounds-checked.
#[derive(Clone)]
pub(crate) struct Rd<'a> {
    pub b: &'a [u8],
    pub p: usize,
}

impl<'a> Rd<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Rd { b, p: 0 }
    }
    pub fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.p..self.p.checked_add(n)?)?;
        self.p += n;
        Some(s)
    }
    pub fn skip(&mut self, n: usize) -> Option<()> {
        self.bytes(n).map(|_| ())
    }
    pub fn u8(&mut self) -> Option<u8> {
        self.bytes(1).map(|b| b[0])
    }
    pub fn u16(&mut self) -> Option<u16> {
        self.bytes(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }
    pub fn u32(&mut self) -> Option<u32> {
        self.bytes(4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn u64(&mut self) -> Option<u64> {
        self.bytes(8).map(|b| u64::from_be_bytes(b.try_into().unwrap()))
    }
}

/// Largest box / element a demuxer buffers whole (a `moof`, an `mdat`, a `SimpleBlock`, `Cues`, ...).
/// YouTube's largest fragments are a few MB; a bigger declared size is a corrupt stream, and
/// waiting for it would buffer without bound.
pub const MAX_ELEMENT: u64 = 64 << 20;

/// `v * num / den` without overflow for the ranges media timestamps use.
pub(crate) fn scale(v: i64, num: i64, den: i64) -> i64 {
    if den == 0 {
        return 0;
    }
    ((v as i128 * num as i128) / den as i128) as i64
}

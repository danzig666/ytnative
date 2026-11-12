//! UMP ("Universal Media Protocol", `application/vnd.yt-ump`) framing: a stream of parts, each
//! `varint type, varint size, payload`. UMP varints are NOT protobuf varints: the count of leading
//! 1-bits in the first byte gives the length (1..5 bytes), the remaining bits are little-endian.
//!
//! [`UmpParser`] is incremental: feed it arbitrary HTTP chunks, take whole parts out.

use anyhow::{bail, Result};

/// Part types we act on (others are skipped).
pub mod part {
    pub const MEDIA_HEADER: u32 = 20;
    pub const MEDIA: u32 = 21;
    pub const MEDIA_END: u32 = 22;
    pub const LIVE_METADATA: u32 = 31;
    pub const NEXT_REQUEST_POLICY: u32 = 35;
    pub const FORMAT_INITIALIZATION_METADATA: u32 = 42;
    pub const SABR_REDIRECT: u32 = 43;
    pub const SABR_ERROR: u32 = 44;
    pub const SABR_SEEK: u32 = 45;
    pub const RELOAD_PLAYER_RESPONSE: u32 = 46;
    pub const PLAYBACK_START_POLICY: u32 = 47;
    pub const SELECTABLE_FORMATS: u32 = 51;
    pub const REQUEST_IDENTIFIER: u32 = 52;
    pub const SABR_CONTEXT_UPDATE: u32 = 57;
    pub const STREAM_PROTECTION_STATUS: u32 = 58;
    pub const SABR_CONTEXT_SENDING_POLICY: u32 = 59;
    pub const END_OF_TRACK: u32 = 62;
    pub const SNACKBAR_MESSAGE: u32 = 67;
}

/// Hard cap for one part (media parts are typically < 2 MiB).
pub const MAX_PART_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub ptype: u32,
    pub data: Vec<u8>,
}

/// Decodes a UMP varint at the start of `b`: `Some((value, length))`, or `None` if `b` is too short.
pub fn read_varint(b: &[u8]) -> Option<(u64, usize)> {
    let first = *b.first()?;
    let size = match first {
        0x00..=0x7f => 1,
        0x80..=0xbf => 2,
        0xc0..=0xdf => 3,
        0xe0..=0xef => 4,
        _ => 5,
    };
    if b.len() < size {
        return None;
    }
    if size == 5 {
        // First byte is only the length marker; the value is the next 4 bytes, little-endian.
        return Some((u32::from_le_bytes([b[1], b[2], b[3], b[4]]) as u64, 5));
    }
    let bits = 8 - size; // payload bits left in the first byte
    let mut v = (first as u64) & ((1u64 << bits) - 1);
    let mut shift = bits;
    for &x in &b[1..size] {
        v |= (x as u64) << shift;
        shift += 8;
    }
    Some((v, size))
}

/// Encodes a UMP varint (used by tests and for building fixtures).
pub fn write_varint(out: &mut Vec<u8>, v: u64) {
    if v < 0x80 {
        out.push(v as u8);
    } else if v < 1 << 14 {
        out.push(0x80 | (v & 0x3f) as u8);
        out.push((v >> 6) as u8);
    } else if v < 1 << 21 {
        out.push(0xc0 | (v & 0x1f) as u8);
        out.extend_from_slice(&[(v >> 5) as u8, (v >> 13) as u8]);
    } else if v < 1 << 28 {
        out.push(0xe0 | (v & 0x0f) as u8);
        out.extend_from_slice(&[(v >> 4) as u8, (v >> 12) as u8, (v >> 20) as u8]);
    } else {
        out.push(0xf0);
        out.extend_from_slice(&(v as u32).to_le_bytes());
    }
}

/// Encodes one part (tests / fixtures).
pub fn write_part(out: &mut Vec<u8>, ptype: u32, data: &[u8]) {
    write_varint(out, ptype as u64);
    write_varint(out, data.len() as u64);
    out.extend_from_slice(data);
}

/// Incremental UMP parser.
#[derive(Default)]
pub struct UmpParser {
    buf: Vec<u8>,
    pos: usize,
}

impl UmpParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends received bytes.
    pub fn feed(&mut self, data: &[u8]) {
        // Compact once the consumed prefix dominates, so the buffer doesn't grow without bound.
        if self.pos > 0 && self.pos * 2 >= self.buf.len() {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        self.buf.extend_from_slice(data);
    }

    /// Next complete part, or `Ok(None)` if more bytes are needed.
    pub fn next_part(&mut self) -> Result<Option<Part>> {
        let rest = &self.buf[self.pos..];
        let Some((ptype, a)) = read_varint(rest) else { return Ok(None) };
        let Some((size, b)) = read_varint(&rest[a..]) else { return Ok(None) };
        if size > MAX_PART_BYTES || ptype > u32::MAX as u64 {
            bail!("UMP part too large ({size} bytes, type {ptype})");
        }
        let start = a + b;
        let end = start + size as usize;
        if rest.len() < end {
            return Ok(None);
        }
        let part = Part { ptype: ptype as u32, data: rest[start..end].to_vec() };
        self.pos += end;
        Ok(Some(part))
    }

    /// Bytes buffered but not yet returned as a part (non-zero at EOF = truncated response).
    pub fn pending(&self) -> usize {
        self.buf.len() - self.pos
    }
}

/// Parses a complete UMP body.
pub fn parse_all(body: &[u8]) -> Result<Vec<Part>> {
    let mut p = UmpParser::new();
    p.feed(body);
    let mut out = Vec::new();
    while let Some(part) = p.next_part()? {
        out.push(part);
    }
    if p.pending() != 0 {
        bail!("truncated UMP body ({} trailing bytes)", p.pending());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip_all_sizes() {
        for v in [0u64, 1, 127, 128, 16383, 16384, (1 << 21) - 1, 1 << 21, (1 << 28) - 1, 1 << 28, u32::MAX as u64] {
            let mut b = Vec::new();
            write_varint(&mut b, v);
            assert_eq!(read_varint(&b), Some((v, b.len())), "value {v}");
            assert_eq!(read_varint(&b[..b.len() - 1]), None);
        }
    }

    #[test]
    fn known_encodings() {
        // Matches googlevideo's reader: 2-byte form = low 6 bits of byte0 + byte1 << 6.
        assert_eq!(read_varint(&[0x95, 0x01]), Some((0x15 + (1 << 6), 2)));
        assert_eq!(read_varint(&[0xf0, 1, 0, 0, 0]), Some((1, 5)));
    }

    #[test]
    fn partial_feeding_byte_by_byte() {
        let mut body = Vec::new();
        write_part(&mut body, 20, &[1, 2, 3]);
        write_part(&mut body, 21, &vec![7u8; 300]);
        write_part(&mut body, 35, &[]);
        let mut p = UmpParser::new();
        let mut got = Vec::new();
        for byte in &body {
            p.feed(std::slice::from_ref(byte));
            while let Some(part) = p.next_part().unwrap() {
                got.push(part);
            }
        }
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], Part { ptype: 20, data: vec![1, 2, 3] });
        assert_eq!(got[1].data.len(), 300);
        assert_eq!(got[2].ptype, 35);
        assert_eq!(p.pending(), 0);
        assert_eq!(parse_all(&body).unwrap(), got);
        assert!(parse_all(&body[..body.len() - 3]).is_err());
    }
}

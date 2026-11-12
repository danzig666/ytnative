//! Hand-written protobuf codec for the subset of YouTube's SABR (`videoplayback` UMP) messages we
//! need. Field numbers follow the public googlevideo / SmartTube / NewTube `.proto` definitions.
//!
//! Encoding is done with a tiny [`Writer`]; decoding with a field iterator ([`Reader`]) plus one
//! `decode` function per response message. Unknown fields are skipped, so new server fields never
//! break parsing.

use anyhow::{bail, Result};

// ---------------------------------------------------------------------------------------------
// Wire-level helpers
// ---------------------------------------------------------------------------------------------

/// Protobuf message writer (fields are appended in call order).
#[derive(Default, Clone, Debug)]
pub struct Writer {
    pub buf: Vec<u8>,
}

fn put_varint(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push((v as u8) | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }
    fn key(&mut self, field: u32, wire: u8) {
        put_varint(&mut self.buf, ((field as u64) << 3) | wire as u64);
    }
    /// int32/int64/uint32/uint64/enum (negative values use 10-byte two's complement, as protobuf does).
    pub fn int(&mut self, field: u32, v: i64) -> &mut Self {
        self.key(field, 0);
        put_varint(&mut self.buf, v as u64);
        self
    }
    pub fn uint(&mut self, field: u32, v: u64) -> &mut Self {
        self.key(field, 0);
        put_varint(&mut self.buf, v);
        self
    }
    /// sint32/sint64 (zig-zag).
    pub fn sint(&mut self, field: u32, v: i64) -> &mut Self {
        self.key(field, 0);
        put_varint(&mut self.buf, ((v << 1) ^ (v >> 63)) as u64);
        self
    }
    pub fn bool(&mut self, field: u32, v: bool) -> &mut Self {
        self.int(field, v as i64)
    }
    pub fn float(&mut self, field: u32, v: f32) -> &mut Self {
        self.key(field, 5);
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn bytes(&mut self, field: u32, v: &[u8]) -> &mut Self {
        self.key(field, 2);
        put_varint(&mut self.buf, v.len() as u64);
        self.buf.extend_from_slice(v);
        self
    }
    pub fn string(&mut self, field: u32, v: &str) -> &mut Self {
        self.bytes(field, v.as_bytes())
    }
    pub fn opt_string(&mut self, field: u32, v: &Option<String>) -> &mut Self {
        if let Some(s) = v {
            self.string(field, s);
        }
        self
    }
    /// Embedded message built by `f`.
    pub fn msg(&mut self, field: u32, f: impl FnOnce(&mut Writer)) -> &mut Self {
        let mut w = Writer::new();
        f(&mut w);
        self.bytes(field, &w.buf)
    }
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

/// One decoded field value.
#[derive(Debug, Clone, Copy)]
pub enum Val<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
}

impl<'a> Val<'a> {
    pub fn u64(&self) -> u64 {
        match *self {
            Val::Varint(v) | Val::Fixed64(v) => v,
            Val::Fixed32(v) => v as u64,
            Val::Bytes(_) => 0,
        }
    }
    pub fn i64(&self) -> i64 {
        self.u64() as i64
    }
    /// int32 fields are sign-extended to 64 bits on the wire; truncation recovers the value.
    pub fn i32(&self) -> i32 {
        self.u64() as i32
    }
    pub fn bool(&self) -> bool {
        self.u64() != 0
    }
    pub fn bytes(&self) -> &'a [u8] {
        match *self {
            Val::Bytes(b) => b,
            _ => &[],
        }
    }
    pub fn string(&self) -> String {
        String::from_utf8_lossy(self.bytes()).into_owned()
    }
}

/// Iterator over the (field number, value) pairs of one message.
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    err: bool,
}

pub fn read_varint(data: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let b = *data.get(*pos)?;
        *pos += 1;
        if shift < 64 {
            v |= ((b & 0x7f) as u64) << shift;
        }
        if b & 0x80 == 0 {
            return Some(v);
        }
        shift += 7;
        if shift > 70 {
            return None;
        }
    }
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0, err: false }
    }
    /// True if iteration stopped on malformed input rather than at the end.
    pub fn failed(&self) -> bool {
        self.err
    }
}

impl<'a> Iterator for Reader<'a> {
    type Item = (u32, Val<'a>);
    fn next(&mut self) -> Option<Self::Item> {
        if self.err || self.pos >= self.data.len() {
            return None;
        }
        let r = (|| {
            let key = read_varint(self.data, &mut self.pos)?;
            let field = (key >> 3) as u32;
            let val = match key & 7 {
                0 => Val::Varint(read_varint(self.data, &mut self.pos)?),
                1 => {
                    let b = self.data.get(self.pos..self.pos + 8)?;
                    self.pos += 8;
                    Val::Fixed64(u64::from_le_bytes(b.try_into().ok()?))
                }
                2 => {
                    let len = read_varint(self.data, &mut self.pos)? as usize;
                    let b = self.data.get(self.pos..self.pos.checked_add(len)?)?;
                    self.pos += len;
                    Val::Bytes(b)
                }
                5 => {
                    let b = self.data.get(self.pos..self.pos + 4)?;
                    self.pos += 4;
                    Val::Fixed32(u32::from_le_bytes(b.try_into().ok()?))
                }
                _ => return None, // groups (3/4) are not used by these messages
            };
            Some((field, val))
        })();
        if r.is_none() {
            self.err = true;
        }
        r
    }
}

/// Repeated int32 that may arrive packed (one length-delimited blob) or unpacked.
fn push_repeated_i32(out: &mut Vec<i32>, v: Val) {
    match v {
        Val::Bytes(b) => {
            let mut p = 0;
            while p < b.len() {
                match read_varint(b, &mut p) {
                    Some(x) => out.push(x as i32),
                    None => break,
                }
            }
        }
        other => out.push(other.i32()),
    }
}

fn check(r: &Reader, what: &str) -> Result<()> {
    if r.failed() {
        bail!("malformed protobuf in {what}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Shared messages
// ---------------------------------------------------------------------------------------------

/// `sabr.misc.FormatId`: a format's identity (itag + last-modified + xtags).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct FormatId {
    pub itag: i32,
    pub lmt: u64,
    pub xtags: Option<String>,
}

impl FormatId {
    pub fn encode(&self, w: &mut Writer) {
        w.int(1, self.itag as i64);
        if self.lmt != 0 {
            w.uint(2, self.lmt);
        }
        w.opt_string(3, &self.xtags);
    }
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut f = FormatId::default();
        let mut r = Reader::new(b);
        for (n, v) in &mut r {
            match n {
                1 => f.itag = v.i32(),
                2 => f.lmt = v.u64(),
                3 => f.xtags = Some(v.string()),
                _ => {}
            }
        }
        check(&r, "FormatId")?;
        Ok(f)
    }
}

/// `TimeRange` (ticks in `timescale` units).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TimeRange {
    pub start_ticks: i64,
    pub duration_ticks: i64,
    pub timescale: i32,
}

impl TimeRange {
    pub fn encode(&self, w: &mut Writer) {
        w.int(1, self.start_ticks).int(2, self.duration_ticks).int(3, self.timescale as i64);
    }
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut t = TimeRange::default();
        let mut r = Reader::new(b);
        for (n, v) in &mut r {
            match n {
                1 => t.start_ticks = v.i64(),
                2 => t.duration_ticks = v.i64(),
                3 => t.timescale = v.i32(),
                _ => {}
            }
        }
        check(&r, "TimeRange")?;
        Ok(t)
    }
    pub fn start_ms(&self) -> i64 {
        if self.timescale > 0 { self.start_ticks * 1000 / self.timescale as i64 } else { 0 }
    }
    pub fn duration_ms(&self) -> i64 {
        if self.timescale > 0 { self.duration_ticks * 1000 / self.timescale as i64 } else { 0 }
    }
}

// ---------------------------------------------------------------------------------------------
// Request side
// ---------------------------------------------------------------------------------------------

/// `BufferedRange`: what the client already holds for one format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferedRange {
    pub format_id: FormatId,
    pub start_time_ms: i64,
    pub duration_ms: i64,
    pub start_segment_index: i32,
    pub end_segment_index: i32,
    pub time_range: Option<TimeRange>,
}

impl BufferedRange {
    pub fn encode(&self, w: &mut Writer) {
        w.msg(1, |w| self.format_id.encode(w));
        w.int(2, self.start_time_ms).int(3, self.duration_ms);
        w.int(4, self.start_segment_index as i64).int(5, self.end_segment_index as i64);
        if let Some(t) = &self.time_range {
            w.msg(6, |w| t.encode(w));
        }
    }
}

/// `ClientAbrState` (the fields we set).
#[derive(Debug, Clone, Default)]
pub struct ClientAbrState {
    pub last_manual_selected_resolution: Option<i32>,
    pub client_viewport_width: Option<i32>,
    pub client_viewport_height: Option<i32>,
    pub sticky_resolution: Option<i32>,
    pub client_viewport_is_flexible: Option<bool>,
    pub bandwidth_estimate: Option<i64>,
    pub player_time_ms: i64,
    pub time_since_last_seek: Option<i64>,
    pub visibility: Option<i32>,
    pub playback_rate: f32,
    pub elapsed_wall_time_ms: Option<i64>,
    pub time_since_last_action_ms: Option<i64>,
    /// 0 = audio+video, 1 = audio only (measured: there is no video-only value).
    pub enabled_track_types_bitfield: i32,
    pub drc_enabled: bool,
    pub audio_track_id: Option<String>,
}

impl ClientAbrState {
    pub fn encode(&self, w: &mut Writer) {
        if let Some(v) = self.last_manual_selected_resolution {
            w.int(16, v as i64);
        }
        if let Some(v) = self.client_viewport_width {
            w.int(18, v as i64);
        }
        if let Some(v) = self.client_viewport_height {
            w.int(19, v as i64);
        }
        if let Some(v) = self.sticky_resolution {
            w.int(21, v as i64);
        }
        if let Some(v) = self.client_viewport_is_flexible {
            w.bool(22, v);
        }
        if let Some(v) = self.bandwidth_estimate {
            w.int(23, v);
        }
        w.int(28, self.player_time_ms);
        if let Some(v) = self.time_since_last_seek {
            w.int(29, v);
        }
        if let Some(v) = self.visibility {
            w.int(34, v as i64);
        }
        w.float(35, if self.playback_rate > 0.0 { self.playback_rate } else { 1.0 });
        if let Some(v) = self.elapsed_wall_time_ms {
            w.int(36, v);
        }
        if let Some(v) = self.time_since_last_action_ms {
            w.int(39, v);
        }
        w.int(40, self.enabled_track_types_bitfield as i64);
        w.bool(46, self.drc_enabled);
        w.opt_string(69, &self.audio_track_id);
    }
}

/// `StreamerContext.ClientInfo`.
#[derive(Debug, Clone, Default)]
pub struct ClientInfo {
    pub hl: Option<String>,
    pub gl: Option<String>,
    pub device_make: Option<String>,
    pub device_model: Option<String>,
    /// `StreamerContext.ClientName` enum value (the `X-YouTube-Client-Name` number).
    pub client_name: i32,
    pub client_version: String,
    pub os_name: Option<String>,
    pub os_version: Option<String>,
}

impl ClientInfo {
    pub fn encode(&self, w: &mut Writer) {
        w.opt_string(1, &self.hl).opt_string(2, &self.gl);
        w.opt_string(12, &self.device_make).opt_string(13, &self.device_model);
        w.int(16, self.client_name as i64).string(17, &self.client_version);
        w.opt_string(18, &self.os_name).opt_string(19, &self.os_version);
    }

    /// Builds it from an InnerTube `context.client` JSON object (what `/player` was called with).
    pub fn from_innertube(client: &serde_json::Value) -> Option<Self> {
        let s = |k: &str| client[k].as_str().map(str::to_string);
        Some(ClientInfo {
            hl: s("hl"),
            gl: s("gl"),
            device_make: s("deviceMake"),
            device_model: s("deviceModel"),
            client_name: client_name_id(client["clientName"].as_str()?)?,
            client_version: s("clientVersion")?,
            os_name: s("osName"),
            os_version: s("osVersion"),
        })
    }
}

/// `StreamerContext.ClientName` ids for the clients we may use.
pub fn client_name_id(name: &str) -> Option<i32> {
    Some(match name.to_ascii_uppercase().as_str() {
        "WEB" => 1,
        "MWEB" => 2,
        "ANDROID" => 3,
        "IOS" => 5,
        "TVHTML5" => 7,
        "TVLITE" => 8,
        "TVANDROID" => 10,
        "ANDROID_VR" => 28,
        "WEB_EMBEDDED_PLAYER" => 56,
        "TVHTML5_SIMPLY_EMBEDDED_PLAYER" => 85,
        "TVHTML5_SIMPLY" => 75,
        "WEB_REMIX" => 67,
        "VISIONOS" => 101,
        _ => return None,
    })
}

/// `StreamerContext.SabrContext`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SabrContext {
    pub ctx_type: i32,
    pub value: Vec<u8>,
}

/// `StreamerContext`.
#[derive(Debug, Clone, Default)]
pub struct StreamerContext {
    pub client_info: ClientInfo,
    pub po_token: Option<Vec<u8>>,
    pub playback_cookie: Option<Vec<u8>>,
    pub sabr_contexts: Vec<SabrContext>,
    pub unsent_sabr_contexts: Vec<i32>,
}

impl StreamerContext {
    pub fn encode(&self, w: &mut Writer) {
        w.msg(1, |w| self.client_info.encode(w));
        if let Some(p) = &self.po_token {
            w.bytes(2, p);
        }
        if let Some(c) = &self.playback_cookie {
            w.bytes(3, c);
        }
        for c in &self.sabr_contexts {
            w.msg(5, |w| {
                w.int(1, c.ctx_type as i64).bytes(2, &c.value);
            });
        }
        for t in &self.unsent_sabr_contexts {
            w.int(6, *t as i64);
        }
    }
}

/// `VideoPlaybackAbrRequest`: the POST body of every SABR request.
#[derive(Debug, Clone, Default)]
pub struct VideoPlaybackAbrRequest {
    pub client_abr_state: ClientAbrState,
    pub selected_format_ids: Vec<FormatId>,
    pub buffered_ranges: Vec<BufferedRange>,
    pub player_time_ms: Option<i64>,
    pub video_playback_ustreamer_config: Vec<u8>,
    pub preferred_audio_format_ids: Vec<FormatId>,
    pub preferred_video_format_ids: Vec<FormatId>,
    pub streamer_context: StreamerContext,
}

impl VideoPlaybackAbrRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.msg(1, |w| self.client_abr_state.encode(w));
        for f in &self.selected_format_ids {
            w.msg(2, |w| f.encode(w));
        }
        for b in &self.buffered_ranges {
            w.msg(3, |w| b.encode(w));
        }
        if let Some(t) = self.player_time_ms {
            w.int(4, t);
        }
        w.bytes(5, &self.video_playback_ustreamer_config);
        for f in &self.preferred_audio_format_ids {
            w.msg(16, |w| f.encode(w));
        }
        for f in &self.preferred_video_format_ids {
            w.msg(17, |w| f.encode(w));
        }
        w.msg(19, |w| self.streamer_context.encode(w));
        w.finish()
    }
}

// ---------------------------------------------------------------------------------------------
// Response side
// ---------------------------------------------------------------------------------------------

/// `MediaHeader`: announces a segment; its bytes follow in MEDIA parts tagged with `header_id`.
#[derive(Debug, Clone, Default)]
pub struct MediaHeader {
    pub header_id: u32,
    pub video_id: Option<String>,
    pub itag: i32,
    pub lmt: u64,
    pub xtags: Option<String>,
    pub start_range: i64,
    pub compression: i32,
    pub is_init_seg: bool,
    pub sequence_number: Option<i32>,
    pub bitrate_bps: i64,
    pub start_ms: i64,
    pub duration_ms: i64,
    pub format_id: Option<FormatId>,
    pub content_length: Option<i64>,
    pub time_range: Option<TimeRange>,
}

impl MediaHeader {
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut h = MediaHeader::default();
        let mut r = Reader::new(b);
        for (n, v) in &mut r {
            match n {
                1 => h.header_id = v.u64() as u32,
                2 => h.video_id = Some(v.string()),
                3 => h.itag = v.i32(),
                4 => h.lmt = v.u64(),
                5 => h.xtags = Some(v.string()),
                6 => h.start_range = v.i64(),
                7 => h.compression = v.i32(),
                8 => h.is_init_seg = v.bool(),
                9 => h.sequence_number = Some(v.i32()),
                10 => h.bitrate_bps = v.i64(),
                11 => h.start_ms = v.i64(),
                12 => h.duration_ms = v.i64(),
                13 => h.format_id = Some(FormatId::decode(v.bytes())?),
                14 => h.content_length = Some(v.i64()),
                15 => h.time_range = Some(TimeRange::decode(v.bytes())?),
                _ => {}
            }
        }
        check(&r, "MediaHeader")?;
        Ok(h)
    }

    /// The segment's itag (from `format_id` if present).
    pub fn itag(&self) -> i32 {
        self.format_id.as_ref().map(|f| f.itag).filter(|&i| i != 0).unwrap_or(self.itag)
    }

    /// (start, duration) in ms, preferring the precise `time_range`.
    pub fn timing_ms(&self) -> (i64, i64) {
        match &self.time_range {
            Some(t) if t.timescale > 0 => (t.start_ms(), t.duration_ms()),
            _ => (self.start_ms, self.duration_ms),
        }
    }
}

/// `FormatInitializationMetadata`: sent once per format before its first segment.
#[derive(Debug, Clone, Default)]
pub struct FormatInitializationMetadata {
    pub video_id: Option<String>,
    pub format_id: FormatId,
    pub end_time_ms: i64,
    pub end_segment_number: i64,
    pub mime_type: Option<String>,
    pub duration_units: i64,
    pub duration_timescale: i64,
}

impl FormatInitializationMetadata {
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut m = FormatInitializationMetadata::default();
        let mut r = Reader::new(b);
        for (n, v) in &mut r {
            match n {
                1 => m.video_id = Some(v.string()),
                2 => m.format_id = FormatId::decode(v.bytes())?,
                3 => m.end_time_ms = v.i64(),
                4 => m.end_segment_number = v.i64(),
                5 => m.mime_type = Some(v.string()),
                9 => m.duration_units = v.i64(),
                10 => m.duration_timescale = v.i64(),
                _ => {}
            }
        }
        check(&r, "FormatInitializationMetadata")?;
        Ok(m)
    }
}

/// `NextRequestPolicy`: pacing hints plus the opaque playback cookie to echo back.
#[derive(Debug, Clone, Default)]
pub struct NextRequestPolicy {
    pub target_audio_readahead_ms: i32,
    pub target_video_readahead_ms: i32,
    pub max_time_since_last_request_ms: i32,
    pub backoff_time_ms: i32,
    pub min_audio_readahead_ms: i32,
    pub min_video_readahead_ms: i32,
    /// Raw `PlaybackCookie` bytes (sent back verbatim in `StreamerContext.playback_cookie`).
    pub playback_cookie: Option<Vec<u8>>,
    pub video_id: Option<String>,
}

impl NextRequestPolicy {
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut p = NextRequestPolicy::default();
        let mut r = Reader::new(b);
        for (n, v) in &mut r {
            match n {
                1 => p.target_audio_readahead_ms = v.i32(),
                2 => p.target_video_readahead_ms = v.i32(),
                3 => p.max_time_since_last_request_ms = v.i32(),
                4 => p.backoff_time_ms = v.i32(),
                5 => p.min_audio_readahead_ms = v.i32(),
                6 => p.min_video_readahead_ms = v.i32(),
                7 => p.playback_cookie = Some(v.bytes().to_vec()),
                8 => p.video_id = Some(v.string()),
                _ => {}
            }
        }
        check(&r, "NextRequestPolicy")?;
        Ok(p)
    }
}

/// `SabrRedirect`.
pub fn decode_redirect(b: &[u8]) -> Result<String> {
    let mut url = String::new();
    let mut r = Reader::new(b);
    for (n, v) in &mut r {
        if n == 1 {
            url = v.string();
        }
    }
    check(&r, "SabrRedirect")?;
    Ok(url)
}

/// `SabrError`.
#[derive(Debug, Clone, Default)]
pub struct SabrError {
    pub error_type: Option<String>,
    pub action: i32,
    pub status_code: Option<i32>,
    pub code_type: Option<i32>,
}

impl SabrError {
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut e = SabrError::default();
        let mut r = Reader::new(b);
        for (n, v) in &mut r {
            match n {
                1 => e.error_type = Some(v.string()),
                2 => e.action = v.i32(),
                3 => {
                    for (m, x) in Reader::new(v.bytes()) {
                        match m {
                            1 => e.status_code = Some(x.i32()),
                            4 => e.code_type = Some(x.i32()),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        check(&r, "SabrError")?;
        Ok(e)
    }
}

/// `StreamProtectionStatus.status`: 1 OK, 2 ATTESTATION_PENDING, 3 ATTESTATION_REQUIRED.
#[derive(Debug, Clone, Copy, Default)]
pub struct StreamProtectionStatus {
    pub status: i32,
    pub max_retries: i32,
}

impl StreamProtectionStatus {
    pub const OK: i32 = 1;
    pub const ATTESTATION_PENDING: i32 = 2;
    pub const ATTESTATION_REQUIRED: i32 = 3;
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut s = StreamProtectionStatus::default();
        let mut r = Reader::new(b);
        for (n, v) in &mut r {
            match n {
                1 => s.status = v.i32(),
                2 => s.max_retries = v.i32(),
                _ => {}
            }
        }
        check(&r, "StreamProtectionStatus")?;
        Ok(s)
    }
}

/// `SabrContextUpdate`: opaque context the server wants echoed in later requests.
#[derive(Debug, Clone, Default)]
pub struct SabrContextUpdate {
    pub ctx_type: i32,
    /// 1 playback, 2 request, 3 watch endpoint, 4 content ads.
    pub scope: i32,
    pub value: Vec<u8>,
    pub send_by_default: bool,
    /// 1 overwrite, 2 keep existing.
    pub write_policy: i32,
}

impl SabrContextUpdate {
    pub const SCOPE_REQUEST: i32 = 2;
    pub const KEEP_EXISTING: i32 = 2;
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut u = SabrContextUpdate::default();
        let mut r = Reader::new(b);
        for (n, v) in &mut r {
            match n {
                1 => u.ctx_type = v.i32(),
                2 => u.scope = v.i32(),
                3 => u.value = v.bytes().to_vec(),
                4 => u.send_by_default = v.bool(),
                5 => u.write_policy = v.i32(),
                _ => {}
            }
        }
        check(&r, "SabrContextUpdate")?;
        Ok(u)
    }
}

/// `SabrContextSendingPolicy`.
#[derive(Debug, Clone, Default)]
pub struct SabrContextSendingPolicy {
    pub start_policy: Vec<i32>,
    pub stop_policy: Vec<i32>,
    pub discard_policy: Vec<i32>,
}

impl SabrContextSendingPolicy {
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut p = SabrContextSendingPolicy::default();
        let mut r = Reader::new(b);
        for (n, v) in &mut r {
            match n {
                1 => push_repeated_i32(&mut p.start_policy, v),
                2 => push_repeated_i32(&mut p.stop_policy, v),
                3 => push_repeated_i32(&mut p.discard_policy, v),
                _ => {}
            }
        }
        check(&r, "SabrContextSendingPolicy")?;
        Ok(p)
    }
}

/// `ReloadPlayerResponse` -> the `reload_playback_params.token` to pass to a new `/player` call.
pub fn decode_reload_player_response(b: &[u8]) -> Result<Option<String>> {
    let mut token = None;
    let mut r = Reader::new(b);
    for (n, v) in &mut r {
        if n == 1 {
            for (m, x) in Reader::new(v.bytes()) {
                if m == 1 {
                    token = Some(x.string());
                }
            }
        }
    }
    check(&r, "ReloadPlayerResponse")?;
    Ok(token)
}

/// `SabrSeek`: the server moves the playhead (seek time in `timescale` units).
#[derive(Debug, Clone, Copy, Default)]
pub struct SabrSeek {
    pub seek_media_time: i64,
    pub seek_media_timescale: i32,
}

impl SabrSeek {
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut s = SabrSeek::default();
        let mut r = Reader::new(b);
        for (n, v) in &mut r {
            match n {
                1 => s.seek_media_time = v.i64(),
                2 => s.seek_media_timescale = v.i32(),
                _ => {}
            }
        }
        check(&r, "SabrSeek")?;
        Ok(s)
    }
    pub fn ms(&self) -> i64 {
        if self.seek_media_timescale > 0 {
            self.seek_media_time * 1000 / self.seek_media_timescale as i64
        } else {
            self.seek_media_time
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip_and_negative() {
        let mut w = Writer::new();
        w.int(1, 300).int(2, -1).sint(3, -2).bool(4, true).float(5, 1.5).string(6, "hi");
        let b = w.finish();
        let got: Vec<_> = Reader::new(&b).collect();
        assert_eq!(got[0].0, 1);
        assert_eq!(got[0].1.u64(), 300);
        assert_eq!(got[1].1.i32(), -1);
        assert_eq!(got[2].1.u64(), 3); // zig-zag(-2) = 3
        assert!(got[3].1.bool());
        assert_eq!(f32::from_bits(got[4].1.u64() as u32), 1.5);
        assert_eq!(got[5].1.string(), "hi");
    }

    #[test]
    fn format_id_roundtrip() {
        let f = FormatId { itag: 137, lmt: 1_700_000_000_123_456, xtags: Some("x".into()) };
        let mut w = Writer::new();
        f.encode(&mut w);
        assert_eq!(FormatId::decode(&w.finish()).unwrap(), f);
    }

    #[test]
    fn media_header_decode() {
        let mut w = Writer::new();
        w.int(1, 3).string(2, "abc").int(3, 140).int(9, 7).int(11, 10_000).int(12, 5_000);
        w.msg(13, |w| FormatId { itag: 140, lmt: 9, xtags: None }.encode(w));
        w.int(14, 1234);
        let h = MediaHeader::decode(&w.finish()).unwrap();
        assert_eq!(h.header_id, 3);
        assert_eq!(h.itag(), 140);
        assert_eq!(h.sequence_number, Some(7));
        assert_eq!(h.timing_ms(), (10_000, 5_000));
        assert_eq!(h.content_length, Some(1234));
    }

    #[test]
    fn packed_repeated() {
        let mut w = Writer::new();
        w.bytes(1, &[1, 2, 3]).int(2, 4).int(2, 5);
        let p = SabrContextSendingPolicy::decode(&w.finish()).unwrap();
        assert_eq!(p.start_policy, vec![1, 2, 3]);
        assert_eq!(p.stop_policy, vec![4, 5]);
    }

    #[test]
    fn malformed_is_error() {
        assert!(MediaHeader::decode(&[0x0a, 0x10, 1]).is_err());
    }
}

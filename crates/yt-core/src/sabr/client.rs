//! Blocking SABR session: POSTs `VideoPlaybackAbrRequest`s to a `serverAbrStreamingUrl`, parses the
//! UMP response and hands back finished init/media segments for one video + one audio format.
//!
//! Protocol behaviour implemented here (all measured against real YouTube, see `sabr/mod.rs`):
//! - every request carries the `videoPlaybackUstreamerConfig` from the player response, the client
//!   info the player response was fetched with, the last playback cookie and any SABR contexts;
//! - `rn` (request number) is incremented per request;
//! - formats become "selected" once their FormatInitializationMetadata arrived; what we already hold
//!   is reported as BufferedRanges (with segment indices — time-only ranges are ignored);
//! - a response without media is pacing, not EOF: wait `backoff_time_ms` (or an own backoff) and ask
//!   again; end of stream = every track received its `end_segment_number`;
//! - SabrRedirect switches host, SabrContextUpdate/SendingPolicy are honoured, ReloadPlayerResponse
//!   and attestation demands surface as typed errors ([`SabrFailure`]).

use super::formats::{self, SabrFormat, Track};
use super::proto::*;
use super::ump::{part, UmpParser};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::time::{Duration, Instant};

/// A finished segment (`is_init` = the format's initialization segment: `ftyp+moov` / EBML header).
#[derive(Debug, Clone)]
pub struct Segment {
    pub track: Track,
    pub itag: i32,
    /// Media sequence number (1-based); 0 for init segments.
    pub sequence: i64,
    pub start_ms: i64,
    pub duration_ms: i64,
    pub data: Vec<u8>,
    pub is_init: bool,
}

/// Failures a caller may want to react to specifically (downcast from `anyhow::Error`).
#[derive(Debug, Clone)]
pub enum SabrFailure {
    /// The server wants a fresh `/player` response (pass `token` as `reloadPlaybackContext`).
    ReloadPlayerResponse { token: Option<String> },
    /// StreamProtectionStatus ATTESTATION_REQUIRED: a PO token is needed for this client/position.
    AttestationRequired,
    /// HTTP error status from the media server (403 = wrong client / missing PO token / expired URL).
    Http(u16),
    /// SabrError part.
    Server(String),
    /// Too many responses in a row without any media.
    NoProgress,
}

impl std::fmt::Display for SabrFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SabrFailure::ReloadPlayerResponse { .. } => write!(f, "SABR: server requested a player-response reload"),
            SabrFailure::AttestationRequired => write!(f, "SABR: attestation required (PO token)"),
            SabrFailure::Http(s) => write!(f, "SABR: HTTP {s}"),
            SabrFailure::Server(s) => write!(f, "SABR: server error {s}"),
            SabrFailure::NoProgress => write!(f, "SABR: no media progress"),
        }
    }
}
impl std::error::Error for SabrFailure {}

/// Everything needed to talk to one SABR endpoint.
#[derive(Debug, Clone)]
pub struct SabrConfig {
    pub url: String,
    pub ustreamer_config: Vec<u8>,
    pub client: ClientInfo,
    pub user_agent: String,
    pub po_token: Option<Vec<u8>>,
    /// Extra HTTP headers for the media POSTs (e.g. Origin/Referer).
    pub extra_headers: Vec<(String, String)>,
    pub video_id: String,
    pub duration_ms: u64,
}

impl SabrConfig {
    /// Reads URL, ustreamer config, video id and duration from a `/player` response.
    pub fn from_player_response(pr: &Value, client: ClientInfo, user_agent: &str) -> Result<SabrConfig> {
        let url = pr["streamingData"]["serverAbrStreamingUrl"].as_str().context("no serverAbrStreamingUrl")?;
        let cfg = pr["playerConfig"]["mediaCommonConfig"]["mediaUstreamerRequestConfig"]["videoPlaybackUstreamerConfig"]
            .as_str()
            .context("no videoPlaybackUstreamerConfig")?;
        let ustreamer_config = base64_decode(cfg).context("bad videoPlaybackUstreamerConfig")?;
        let adaptive = pr["streamingData"]["adaptiveFormats"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let duration_ms = adaptive
            .iter()
            .filter_map(|f| f["approxDurationMs"].as_str()?.parse::<u64>().ok())
            .max()
            .or_else(|| pr["videoDetails"]["lengthSeconds"].as_str()?.parse::<u64>().ok().map(|s| s * 1000))
            .unwrap_or(0);
        Ok(SabrConfig {
            url: url.to_string(),
            ustreamer_config,
            client,
            user_agent: user_agent.to_string(),
            po_token: None,
            extra_headers: Vec::new(),
            video_id: pr["videoDetails"]["videoId"].as_str().unwrap_or("").to_string(),
            duration_ms,
        })
    }
}

/// URL-safe or standard base64, padding optional.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// A contiguous run of segments we hold for one format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    first_seq: i64,
    last_seq: i64,
    start_ms: i64,
    end_ms: i64,
}

#[derive(Debug, Clone)]
struct TrackState {
    fmt: SabrFormat,
    initialized: bool,
    end_segment: Option<i64>,
    end_time_ms: Option<i64>,
    init_data: Option<Vec<u8>>,
    runs: Vec<Run>,
}

impl TrackState {
    fn new(fmt: SabrFormat) -> Self {
        TrackState { fmt, initialized: false, end_segment: None, end_time_ms: None, init_data: None, runs: Vec::new() }
    }

    fn has(&self, seq: i64) -> bool {
        self.runs.iter().any(|r| seq >= r.first_seq && seq <= r.last_seq)
    }

    fn add(&mut self, seq: i64, start_ms: i64, dur_ms: i64) {
        let end = start_ms + dur_ms;
        if let Some(r) = self.runs.iter_mut().find(|r| r.last_seq + 1 == seq) {
            r.last_seq = seq;
            r.end_ms = r.end_ms.max(end);
        } else if let Some(r) = self.runs.iter_mut().find(|r| r.first_seq == seq + 1) {
            r.first_seq = seq;
            r.start_ms = start_ms;
        } else {
            self.runs.push(Run { first_seq: seq, last_seq: seq, start_ms, end_ms: end });
        }
        // Merge runs that now touch.
        self.runs.sort_by_key(|r| r.first_seq);
        let mut merged: Vec<Run> = Vec::with_capacity(self.runs.len());
        for r in self.runs.drain(..) {
            match merged.last_mut() {
                Some(m) if m.last_seq + 1 >= r.first_seq => {
                    m.last_seq = m.last_seq.max(r.last_seq);
                    m.end_ms = m.end_ms.max(r.end_ms);
                }
                _ => merged.push(r),
            }
        }
        self.runs = merged;
    }

    /// End (ms) of the run containing `pos`, or `pos` if nothing is buffered there.
    fn buffered_end(&self, pos: i64) -> i64 {
        self.runs.iter().filter(|r| r.start_ms <= pos + 50 && r.end_ms > pos).map(|r| r.end_ms).max().unwrap_or(pos)
    }

    fn done(&self) -> bool {
        match (self.end_segment, self.runs.last()) {
            (Some(end), Some(r)) if end > 0 => r.last_seq >= end,
            (None, Some(r)) => self.end_time_ms.is_some_and(|e| e > 0 && r.end_ms >= e - 50),
            _ => false,
        }
    }
}

/// Counters for diagnostics.
#[derive(Debug, Clone, Default)]
pub struct SabrStats {
    pub requests: u32,
    pub bytes: u64,
    pub empty_responses: u32,
    pub redirects: u32,
    pub backoff_ms_total: u64,
    /// Last StreamProtectionStatus (1 OK, 2 pending, 3 required).
    pub protection_status: i32,
}

/// A SABR session for one video and up to one video + one audio format.
pub struct SabrStream {
    http: reqwest::blocking::Client,
    pub cfg: SabrConfig,
    video: Option<TrackState>,
    audio: Option<TrackState>,
    rn: u32,
    cookie: Option<Vec<u8>>,
    contexts: HashMap<i32, SabrContextUpdate>,
    send_contexts: BTreeSet<i32>,
    backoff_ms: u64,
    player_time_override: Option<i64>,
    base_ms: i64,
    started: Instant,
    last_seek: Instant,
    reemit_init: bool,
    empty_streak: u32,
    finished: bool,
    pub stats: SabrStats,
    /// Give up after this many consecutive responses without media.
    pub max_empty_responses: u32,
}

impl SabrStream {
    /// `video`/`audio`: the formats to stream (at least one).
    pub fn new(http: reqwest::blocking::Client, cfg: SabrConfig, video: Option<SabrFormat>, audio: Option<SabrFormat>) -> Result<SabrStream> {
        if video.is_none() && audio.is_none() {
            bail!("SABR: no format selected");
        }
        Ok(SabrStream {
            http,
            cfg,
            video: video.map(TrackState::new),
            audio: audio.map(TrackState::new),
            rn: 0,
            cookie: None,
            contexts: HashMap::new(),
            send_contexts: BTreeSet::new(),
            backoff_ms: 0,
            player_time_override: None,
            base_ms: 0,
            started: Instant::now(),
            last_seek: Instant::now(),
            reemit_init: false,
            empty_streak: 0,
            finished: false,
            stats: SabrStats::default(),
            max_empty_responses: 30,
        })
    }

    /// Builds a session from a raw `/player` response. `client` is the InnerTube `context.client`
    /// the response was fetched with (see [`ClientInfo::from_innertube`]).
    pub fn from_player_response(
        http: reqwest::blocking::Client,
        pr: &Value,
        client: ClientInfo,
        user_agent: &str,
        video_itag: Option<i32>,
        audio_itag: Option<i32>,
    ) -> Result<SabrStream> {
        let cfg = SabrConfig::from_player_response(pr, client, user_agent)?;
        let adaptive = pr["streamingData"]["adaptiveFormats"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let pick = |itag: Option<i32>| -> Result<Option<SabrFormat>> {
            match itag {
                None => Ok(None),
                Some(i) => {
                    let f = formats::by_itag(adaptive, i).with_context(|| format!("itag {i} not in adaptiveFormats"))?;
                    Ok(Some(SabrFormat::from_json(f).with_context(|| format!("itag {i} unusable"))?))
                }
            }
        };
        SabrStream::new(http, cfg, pick(video_itag)?, pick(audio_itag)?)
    }

    /// Convenience over [`crate::streams::Resolved`]: uses the client the response came from.
    pub fn from_resolved(
        http: reqwest::blocking::Client,
        r: &crate::streams::Resolved,
        tv: &crate::bootstrap::TvConfig,
        video_itag: Option<i32>,
        audio_itag: Option<i32>,
    ) -> Result<SabrStream> {
        let client = ClientInfo::from_innertube(&r.client.context(tv).0).context("unknown client name")?;
        SabrStream::from_player_response(http, &r.raw, client, r.client.user_agent(), video_itag, audio_itag)
    }

    /// Sets the PO token (raw bytes, i.e. base64-decoded) sent in StreamerContext.
    pub fn set_po_token(&mut self, token: Option<Vec<u8>>) {
        self.cfg.po_token = token;
    }

    /// Player mode: report the real playhead so the server paces delivery to its readahead target.
    /// `None` (default) = download mode: the playhead is assumed to sit at the end of what we hold,
    /// so the server keeps sending.
    pub fn set_player_time(&mut self, ms: Option<i64>) {
        self.player_time_override = ms;
    }

    /// Client-side seek: forgets buffered ranges and continues from `ms`. Cached init segments are
    /// re-emitted with the next batch so a flushed decoder pipeline can restart.
    pub fn seek(&mut self, ms: i64) {
        self.base_ms = ms.max(0);
        for t in self.tracks_mut() {
            t.runs.clear();
        }
        if self.player_time_override.is_some() {
            self.player_time_override = Some(self.base_ms);
        }
        self.reemit_init = true;
        self.empty_streak = 0;
        self.finished = false;
        self.last_seek = Instant::now();
    }

    /// True once every track delivered its last segment.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn video_format(&self) -> Option<&SabrFormat> {
        self.video.as_ref().map(|t| &t.fmt)
    }

    pub fn audio_format(&self) -> Option<&SabrFormat> {
        self.audio.as_ref().map(|t| &t.fmt)
    }

    /// Cached initialization segment of a track, once received.
    pub fn init_segment(&self, track: Track) -> Option<&[u8]> {
        self.track(track)?.init_data.as_deref()
    }

    /// End (ms) of contiguous buffered media from the current position, per track.
    pub fn buffered_until(&self, track: Track) -> Option<i64> {
        Some(self.track(track)?.buffered_end(self.position()))
    }

    fn track(&self, t: Track) -> Option<&TrackState> {
        match t {
            Track::Video => self.video.as_ref(),
            Track::Audio => self.audio.as_ref(),
        }
    }

    fn tracks_mut(&mut self) -> impl Iterator<Item = &mut TrackState> {
        self.video.iter_mut().chain(self.audio.iter_mut())
    }

    fn tracks(&self) -> impl Iterator<Item = &TrackState> {
        self.video.iter().chain(self.audio.iter())
    }

    fn position(&self) -> i64 {
        self.player_time_override.unwrap_or(self.base_ms)
    }

    /// Player time sent to the server.
    fn player_time(&self) -> i64 {
        if let Some(t) = self.player_time_override {
            return t;
        }
        // Download mode: the slowest unfinished track's buffered end.
        self.tracks().filter(|t| !t.done()).map(|t| t.buffered_end(self.base_ms)).min().unwrap_or(self.base_ms)
    }

    /// The protobuf body of the next request.
    pub fn build_request(&self) -> Vec<u8> {
        let v = self.video.as_ref();
        let height = v.map(|t| t.fmt.height as i32).filter(|&h| h > 0);
        let now = Instant::now();
        let abr = ClientAbrState {
            last_manual_selected_resolution: height,
            client_viewport_width: v.map(|t| t.fmt.width.max(1) as i32),
            client_viewport_height: height,
            sticky_resolution: height,
            client_viewport_is_flexible: v.map(|_| false),
            bandwidth_estimate: Some(10_000_000),
            player_time_ms: self.player_time(),
            time_since_last_seek: Some(now.duration_since(self.last_seek).as_millis() as i64),
            visibility: Some(0),
            playback_rate: 1.0,
            elapsed_wall_time_ms: Some(now.duration_since(self.started).as_millis() as i64),
            time_since_last_action_ms: None,
            enabled_track_types_bitfield: if v.is_some() { 0 } else { 1 },
            drc_enabled: self.audio.as_ref().is_some_and(|a| a.fmt.is_drc),
            audio_track_id: self.audio.as_ref().and_then(|a| a.fmt.audio_track_id.clone()),
        };
        let mut req = VideoPlaybackAbrRequest {
            client_abr_state: abr,
            video_playback_ustreamer_config: self.cfg.ustreamer_config.clone(),
            ..Default::default()
        };
        for t in self.tracks() {
            if t.initialized {
                req.selected_format_ids.push(t.fmt.id.clone());
            }
            for r in &t.runs {
                req.buffered_ranges.push(BufferedRange {
                    format_id: t.fmt.id.clone(),
                    start_time_ms: r.start_ms,
                    duration_ms: r.end_ms - r.start_ms,
                    start_segment_index: r.first_seq as i32,
                    end_segment_index: r.last_seq as i32,
                    time_range: Some(TimeRange { start_ticks: r.start_ms, duration_ticks: r.end_ms - r.start_ms, timescale: 1000 }),
                });
            }
        }
        if let Some(t) = &self.video {
            req.preferred_video_format_ids.push(t.fmt.id.clone());
        }
        if let Some(t) = &self.audio {
            req.preferred_audio_format_ids.push(t.fmt.id.clone());
        }
        let mut sc = StreamerContext {
            client_info: self.cfg.client.clone(),
            po_token: self.cfg.po_token.clone(),
            playback_cookie: self.cookie.clone(),
            ..Default::default()
        };
        for ty in &self.send_contexts {
            match self.contexts.get(ty) {
                Some(c) => sc.sabr_contexts.push(SabrContext { ctx_type: *ty, value: c.value.clone() }),
                None => sc.unsent_sabr_contexts.push(*ty),
            }
        }
        req.streamer_context = sc;
        req.encode()
    }

    fn request_url(&self) -> String {
        let (base, query) = self.cfg.url.split_once('?').unwrap_or((&self.cfg.url, ""));
        let mut q: Vec<&str> = query.split('&').filter(|kv| !kv.is_empty() && !kv.starts_with("rn=")).collect();
        let rn = format!("rn={}", self.rn);
        q.push(&rn);
        format!("{base}?{}", q.join("&"))
    }

    /// Blocks until at least one new media segment arrived (sleeping as the server asks), and
    /// returns everything received in arrival order (init segments included). Returns an empty
    /// vector only when the stream has ended. In player mode prefer [`Self::poll_segments`], which
    /// does not block through pacing.
    pub fn next_segments(&mut self) -> Result<Vec<Segment>> {
        let mut out = Vec::new();
        let mut rounds = 0;
        loop {
            let got = self.poll_segments()?;
            let media = got.iter().any(|s| !s.is_init);
            out.extend(got);
            if media || self.finished {
                return Ok(out);
            }
            rounds += 1;
            if self.empty_streak > self.max_empty_responses || rounds > self.max_empty_responses + 3 {
                return Err(SabrFailure::NoProgress.into());
            }
        }
    }

    /// Milliseconds the server asked us to wait before the next request (slept at the start of
    /// the next poll).
    pub fn pending_backoff_ms(&self) -> u64 {
        self.backoff_ms
    }

    /// One request/response round (after sleeping any pending backoff; redirects are followed).
    /// An empty result with `!is_finished()` means the server is pacing: the client is far enough
    /// ahead of the reported playhead. Call again later (after moving the playhead).
    pub fn poll_segments(&mut self) -> Result<Vec<Segment>> {
        let mut out = Vec::new();
        if self.reemit_init {
            self.reemit_init = false;
            for t in self.tracks() {
                if let Some(d) = &t.init_data {
                    out.push(Segment { track: t.fmt.track, itag: t.fmt.itag(), sequence: 0, start_ms: 0, duration_ms: 0, data: d.clone(), is_init: true });
                }
            }
        }
        if self.finished {
            return Ok(out);
        }
        if self.backoff_ms > 0 {
            std::thread::sleep(Duration::from_millis(self.backoff_ms));
            self.stats.backoff_ms_total += self.backoff_ms;
            self.backoff_ms = 0;
        }
        let before = out.len();
        let mut redirects = 0;
        while self.round_trip(&mut out)? {
            redirects += 1;
            if redirects > 5 {
                bail!("SABR: too many redirects");
            }
        }
        if self.tracks().all(TrackState::done) {
            self.finished = true;
        }
        if out.len() > before || self.finished {
            self.empty_streak = 0;
        } else {
            self.empty_streak += 1;
            self.stats.empty_responses += 1;
            if self.backoff_ms == 0 {
                // Pacing without an explicit backoff: 250 ms doubling, capped at 4 s.
                self.backoff_ms = (250u64 << (self.empty_streak - 1).min(4)).min(4000);
            }
        }
        Ok(out)
    }

    /// One POST + response. Returns true if the server redirected us (retry immediately).
    fn round_trip(&mut self, out: &mut Vec<Segment>) -> Result<bool> {
        let body = self.build_request();
        let url = self.request_url();
        self.rn += 1;
        self.stats.requests += 1;
        let mut req = self
            .http
            .post(&url)
            .header("User-Agent", &self.cfg.user_agent)
            .header("Content-Type", "application/x-protobuf")
            .header("Accept", "application/vnd.yt-ump")
            .header("Accept-Encoding", "identity");
        for (k, v) in &self.cfg.extra_headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let mut resp = req.body(body).send().context("SABR request")?;
        let status = resp.status();
        if !status.is_success() {
            return Err(SabrFailure::Http(status.as_u16()).into());
        }
        let mut parser = UmpParser::new();
        let mut rx = ResponseState::default();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let n = resp.read(&mut chunk).context("SABR read")?;
            if n == 0 {
                break;
            }
            self.stats.bytes += n as u64;
            parser.feed(&chunk[..n]);
            while let Some(p) = parser.next_part()? {
                self.process_part(p.ptype, &p.data, &mut rx, out)?;
            }
        }
        if parser.pending() != 0 {
            bail!("SABR: truncated UMP response ({} bytes left)", parser.pending());
        }
        Ok(rx.redirected)
    }

    /// Applies one UMP part. Public for offline tests.
    pub fn process_part(&mut self, ptype: u32, data: &[u8], rx: &mut ResponseState, out: &mut Vec<Segment>) -> Result<()> {
        match ptype {
            part::FORMAT_INITIALIZATION_METADATA => {
                let m = FormatInitializationMetadata::decode(data)?;
                if let Some(t) = self.track_by_itag(m.format_id.itag) {
                    t.initialized = true;
                    if m.end_segment_number > 0 {
                        t.end_segment = Some(m.end_segment_number);
                    }
                    if m.end_time_ms > 0 {
                        t.end_time_ms = Some(m.end_time_ms);
                    }
                }
            }
            part::MEDIA_HEADER => {
                let h = MediaHeader::decode(data)?;
                if h.compression != 0 {
                    bail!("SABR: compressed media segments are not supported");
                }
                rx.headers.insert(h.header_id, (h, Vec::new()));
            }
            part::MEDIA => {
                let (id, n) = super::ump::read_varint(data).context("SABR: empty MEDIA part")?;
                if let Some((_, buf)) = rx.headers.get_mut(&(id as u32)) {
                    buf.extend_from_slice(&data[n..]);
                }
            }
            part::MEDIA_END => {
                let (id, _) = super::ump::read_varint(data).context("SABR: empty MEDIA_END part")?;
                if let Some((h, buf)) = rx.headers.remove(&(id as u32)) {
                    if let Some(len) = h.content_length {
                        if len as usize != buf.len() {
                            bail!("SABR: segment itag {} length {} != {}", h.itag(), buf.len(), len);
                        }
                    }
                    self.finish_segment(h, buf, out);
                }
            }
            part::NEXT_REQUEST_POLICY => {
                let p = NextRequestPolicy::decode(data)?;
                self.backoff_ms = p.backoff_time_ms.clamp(0, 60_000) as u64;
                if p.playback_cookie.is_some() {
                    self.cookie = p.playback_cookie;
                }
            }
            part::SABR_REDIRECT => {
                let url = decode_redirect(data)?;
                if !url.is_empty() {
                    self.cfg.url = url;
                    self.stats.redirects += 1;
                    rx.redirected = true;
                }
            }
            part::SABR_ERROR => {
                let e = SabrError::decode(data)?;
                return Err(SabrFailure::Server(format!(
                    "{} (action {}, code {:?})",
                    e.error_type.unwrap_or_default(),
                    e.action,
                    e.status_code
                ))
                .into());
            }
            part::RELOAD_PLAYER_RESPONSE => {
                let token = decode_reload_player_response(data)?;
                return Err(SabrFailure::ReloadPlayerResponse { token }.into());
            }
            part::STREAM_PROTECTION_STATUS => {
                let s = StreamProtectionStatus::decode(data)?;
                self.stats.protection_status = s.status;
                // PENDING rides along with normally served media; only REQUIRED is terminal.
                if s.status == StreamProtectionStatus::ATTESTATION_REQUIRED {
                    return Err(SabrFailure::AttestationRequired.into());
                }
            }
            part::SABR_CONTEXT_UPDATE => {
                let u = SabrContextUpdate::decode(data)?;
                if u.scope != SabrContextUpdate::SCOPE_REQUEST
                    && (u.write_policy != SabrContextUpdate::KEEP_EXISTING || !self.contexts.contains_key(&u.ctx_type))
                {
                    if u.send_by_default {
                        self.send_contexts.insert(u.ctx_type);
                    }
                    self.contexts.insert(u.ctx_type, u);
                }
            }
            part::SABR_CONTEXT_SENDING_POLICY => {
                let p = SabrContextSendingPolicy::decode(data)?;
                self.send_contexts.extend(p.start_policy);
                for t in p.stop_policy {
                    self.send_contexts.remove(&t);
                }
                for t in p.discard_policy {
                    self.contexts.remove(&t);
                    self.send_contexts.remove(&t);
                }
            }
            part::SABR_SEEK => {
                // The server moved the playhead (e.g. a resume token); continue from there.
                let ms = SabrSeek::decode(data)?.ms();
                self.base_ms = ms.max(0);
                if self.player_time_override.is_some() {
                    self.player_time_override = Some(self.base_ms);
                }
            }
            _ => {} // SELECTABLE_FORMATS, REQUEST_IDENTIFIER, END_OF_TRACK, hints...: nothing to do
        }
        Ok(())
    }

    fn track_by_itag(&mut self, itag: i32) -> Option<&mut TrackState> {
        self.tracks_mut().find(|t| t.fmt.itag() == itag)
    }

    fn finish_segment(&mut self, h: MediaHeader, data: Vec<u8>, out: &mut Vec<Segment>) {
        let itag = h.itag();
        let (start_ms, duration_ms) = h.timing_ms();
        let Some(t) = self.track_by_itag(itag) else {
            return; // a companion format we did not ask for
        };
        if h.is_init_seg {
            t.initialized = true;
            if t.init_data.is_none() {
                t.init_data = Some(data.clone());
                out.push(Segment { track: t.fmt.track, itag, sequence: 0, start_ms: 0, duration_ms: 0, data, is_init: true });
            }
            return;
        }
        let seq = h.sequence_number.unwrap_or(0) as i64;
        if t.has(seq) {
            return; // duplicate
        }
        t.add(seq, start_ms, duration_ms);
        out.push(Segment { track: t.fmt.track, itag, sequence: seq, start_ms, duration_ms, data, is_init: false });
    }
}

/// Per-response parsing state (open segments by header id, redirect flag).
#[derive(Default)]
pub struct ResponseState {
    headers: HashMap<u32, (MediaHeader, Vec<u8>)>,
    redirected: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sabr::ump::write_part;

    fn fmt(itag: i32, track: Track) -> SabrFormat {
        SabrFormat {
            id: FormatId { itag, lmt: 1, xtags: None },
            track,
            mime_type: if track == Track::Video { "video/mp4".into() } else { "audio/mp4".into() },
            width: 1920,
            height: if track == Track::Video { 1080 } else { 0 },
            fps: 30,
            bitrate: 1,
            approx_duration_ms: 10_000,
            audio_track_id: None,
            is_drc: false,
        }
    }

    fn stream() -> SabrStream {
        let cfg = SabrConfig {
            url: "https://x.googlevideo.com/videoplayback?a=1&rn=9".into(),
            ustreamer_config: vec![1, 2, 3],
            client: ClientInfo { client_name: 101, client_version: "1.02".into(), ..Default::default() },
            user_agent: "ua".into(),
            po_token: None,
            extra_headers: Vec::new(),
            video_id: "abc".into(),
            duration_ms: 10_000,
        };
        SabrStream::new(reqwest::blocking::Client::new(), cfg, Some(fmt(137, Track::Video)), Some(fmt(140, Track::Audio))).unwrap()
    }

    fn header(id: i64, itag: i32, init: bool, seq: i64, start: i64, dur: i64, len: usize) -> Vec<u8> {
        let mut w = Writer::new();
        w.int(1, id).string(2, "abc").int(3, itag as i64).bool(8, init);
        if !init {
            w.int(9, seq).int(11, start).int(12, dur);
        }
        w.int(14, len as i64);
        w.finish()
    }

    fn segment_parts(body: &mut Vec<u8>, id: u8, itag: i32, init: bool, seq: i64, start: i64, payload: &[u8]) {
        write_part(body, part::MEDIA_HEADER, &header(id as i64, itag, init, seq, start, 5000, payload.len()));
        let half = payload.len() / 2;
        for chunk in [&payload[..half], &payload[half..]] {
            let mut d = vec![id];
            d.extend_from_slice(chunk);
            write_part(body, part::MEDIA, &d);
        }
        write_part(body, part::MEDIA_END, &[id]);
    }

    fn run(s: &mut SabrStream, body: &[u8]) -> (Vec<Segment>, bool) {
        let mut out = Vec::new();
        let mut rx = ResponseState::default();
        for p in crate::sabr::ump::parse_all(body).unwrap() {
            s.process_part(p.ptype, &p.data, &mut rx, &mut out).unwrap();
        }
        (out, rx.redirected)
    }

    #[test]
    fn segments_ranges_end_and_seek() {
        let mut s = stream();
        assert!(s.request_url().ends_with("?a=1&rn=0"));
        let mut body = Vec::new();
        for itag in [137, 140] {
            let mut w = Writer::new();
            w.string(1, "abc").msg(2, |w| FormatId { itag, lmt: 1, xtags: None }.encode(w)).int(3, 10_000).int(4, 2);
            write_part(&mut body, part::FORMAT_INITIALIZATION_METADATA, &w.finish());
        }
        let mut w = Writer::new();
        w.int(4, 1500).bytes(7, b"cookie");
        write_part(&mut body, part::NEXT_REQUEST_POLICY, &w.finish());
        segment_parts(&mut body, 0, 137, true, 0, 0, b"VINIT");
        segment_parts(&mut body, 1, 140, true, 0, 0, b"AINIT");
        segment_parts(&mut body, 2, 137, false, 1, 0, b"video-seg-1");
        segment_parts(&mut body, 3, 140, false, 1, 0, b"audio-seg-1");
        segment_parts(&mut body, 4, 999, false, 1, 0, b"foreign"); // ignored
        let (out, _) = run(&mut s, &body);
        assert_eq!(out.len(), 4);
        assert!(out[0].is_init && out[0].data == b"VINIT");
        assert_eq!((out[2].track, out[2].sequence, out[2].data.as_slice()), (Track::Video, 1, &b"video-seg-1"[..]));
        assert_eq!(s.backoff_ms, 1500);
        assert_eq!(s.cookie.as_deref(), Some(&b"cookie"[..]));
        assert_eq!(s.player_time(), 5000);
        // Request now declares both formats selected and buffered with segment indices.
        let req = s.build_request();
        let fields: Vec<u32> = Reader::new(&req).map(|(n, _)| n).collect();
        assert_eq!(fields.iter().filter(|&&n| n == 2).count(), 2);
        assert_eq!(fields.iter().filter(|&&n| n == 3).count(), 2);

        // Duplicate seg 1 is dropped; seg 2 finishes both tracks.
        let mut body = Vec::new();
        segment_parts(&mut body, 0, 137, false, 1, 0, b"video-seg-1");
        segment_parts(&mut body, 1, 137, false, 2, 5000, b"video-seg-2");
        segment_parts(&mut body, 2, 140, false, 2, 5000, b"audio-seg-2");
        let (out, _) = run(&mut s, &body);
        assert_eq!(out.len(), 2);
        assert!(s.tracks().all(TrackState::done));
        assert_eq!(s.video.as_ref().unwrap().runs, vec![Run { first_seq: 1, last_seq: 2, start_ms: 0, end_ms: 10_000 }]);

        // Seek clears ranges and re-emits init segments.
        s.seek(5000);
        assert!(!s.is_finished());
        assert_eq!(s.player_time(), 5000);
        assert!(s.reemit_init);
        assert_eq!(s.init_segment(Track::Audio), Some(&b"AINIT"[..]));
    }

    #[test]
    fn redirect_and_errors() {
        let mut s = stream();
        let mut body = Vec::new();
        let mut w = Writer::new();
        w.string(1, "https://y.googlevideo.com/videoplayback?b=2");
        write_part(&mut body, part::SABR_REDIRECT, &w.finish());
        let (_, redirected) = run(&mut s, &body);
        assert!(redirected);
        s.rn = 3;
        assert_eq!(s.request_url(), "https://y.googlevideo.com/videoplayback?b=2&rn=3");

        let mut w = Writer::new();
        w.int(1, 3);
        let mut rx = ResponseState::default();
        let e = s.process_part(part::STREAM_PROTECTION_STATUS, &w.finish(), &mut rx, &mut Vec::new()).unwrap_err();
        assert!(matches!(e.downcast_ref::<SabrFailure>(), Some(SabrFailure::AttestationRequired)));
        let mut w = Writer::new();
        w.msg(1, |w| {
            w.string(1, "tok");
        });
        let e = s.process_part(part::RELOAD_PLAYER_RESPONSE, &w.finish(), &mut rx, &mut Vec::new()).unwrap_err();
        assert!(matches!(e.downcast_ref::<SabrFailure>(), Some(SabrFailure::ReloadPlayerResponse { token: Some(t) }) if t == "tok"));
    }

    #[test]
    fn base64() {
        assert_eq!(base64_decode("aGVsbG8").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("-_8").unwrap(), vec![0xfb, 0xff]);
    }
}

//! Where access units come from: per-track lanes (bounded queues of framed units) filled by
//! fetcher threads, either from direct-URL ranged downloads (sidx/Cues driven) or from SABR.
//!
//! Seeks are generations: the engine bumps [`SourceCtl::gen`] with a target; fetchers notice,
//! reposition (segment containing the target / `SabrStream::seek`), and push units tagged with the
//! new generation. For video they drop everything before the last keyframe at or before the
//! target, so the first unit of a generation is the keyframe the pipeline's `Seek` anchors on.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::*};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use yt_core::demux::framing::Framer;
use yt_core::demux::ranged::RangedTrack;
use yt_core::demux::{AccessUnit, Container, Demuxer, Kind, TrackInfo};

/// How far (content time) a fetcher reads ahead of the playhead.
pub const READ_AHEAD_NS: i64 = 30_000_000_000;
/// Byte cap per lane (4K VP9 runs ~2.5 MB/s).
pub const VIDEO_MAX_BYTES: usize = 48 << 20;
pub const AUDIO_MAX_BYTES: usize = 6 << 20;
/// On a seek, audio starts this much before the target, so it covers the video keyframe at or
/// before the target (YouTube GOPs are at most ~5 s); the engine drops audio before that keyframe.
const AUDIO_SEEK_LEAD_NS: i64 = 5_000_000_000;

/// One framed access unit, ready for `Feed`.
pub struct Unit {
    pub pts_ns: i64,
    pub dur_ns: i64,
    pub key: bool,
    pub data: Vec<u8>,
}

#[derive(Default)]
struct LaneState {
    q: VecDeque<Unit>,
    gen: u64,
    bytes: usize,
    finished: bool,
    error: Option<String>,
    track: Option<TrackInfo>,
    /// Highest pushed pts + duration in this generation.
    end_ns: i64,
}

#[derive(Default)]
pub struct Lane {
    st: Mutex<LaneState>,
    cv: Condvar,
}

impl Lane {
    pub fn new() -> Arc<Lane> {
        Arc::new(Lane::default())
    }

    /// The lane state. A fetcher that panicked while holding the lock must not take the engine
    /// down with it, so a poisoned lock is used as it is.
    fn state(&self) -> std::sync::MutexGuard<'_, LaneState> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Fetcher side: false when `gen` is stale (unit dropped).
    fn push(&self, gen: u64, u: Unit) -> bool {
        let mut s = self.state();
        if s.gen != gen {
            return false;
        }
        s.bytes += u.data.len();
        s.end_ns = s.end_ns.max(u.pts_ns + u.dur_ns);
        s.q.push_back(u);
        self.cv.notify_all();
        true
    }

    pub fn pop(&self) -> Option<Unit> {
        let mut s = self.state();
        let u = s.q.pop_front()?;
        s.bytes -= u.data.len();
        self.cv.notify_all();
        Some(u)
    }

    pub fn peek_pts(&self) -> Option<i64> {
        self.state().q.front().map(|u| u.pts_ns)
    }

    /// Engine side, on a seek: drop everything and expect generation `gen`. Must be called
    /// before the generation is published in [`SourceCtl::gen`].
    pub fn reset(&self, gen: u64) {
        let mut s = self.state();
        s.q.clear();
        s.bytes = 0;
        s.gen = gen;
        s.finished = false;
        s.end_ns = 0;
        self.cv.notify_all();
    }

    fn set_track(&self, t: TrackInfo) {
        self.state().track = Some(t);
        self.cv.notify_all();
    }

    pub fn track(&self) -> Option<TrackInfo> {
        self.state().track.clone()
    }

    fn finish(&self, gen: u64) {
        let mut s = self.state();
        if s.gen == gen {
            s.finished = true;
        }
        self.cv.notify_all();
    }

    pub fn fail(&self, e: String) {
        let mut s = self.state();
        s.error.get_or_insert(e);
        self.cv.notify_all();
    }

    pub fn error(&self) -> Option<String> {
        self.state().error.clone()
    }

    /// All units of the current generation were delivered and consumed.
    pub fn drained(&self) -> bool {
        let s = self.state();
        s.finished && s.q.is_empty()
    }

    pub fn is_empty(&self) -> bool {
        self.state().q.is_empty()
    }

    /// Fetcher side: true when there is room; otherwise blocks (≤ 200 ms) and returns false.
    fn wait_room(&self, ctl: &SourceCtl, max_bytes: usize) -> bool {
        let s = self.state();
        let ahead = s.end_ns - ctl.playhead_ns.load(Relaxed);
        if s.bytes < max_bytes && (s.q.is_empty() || ahead < READ_AHEAD_NS) {
            return true;
        }
        let _ = self.cv.wait_timeout(s, Duration::from_millis(200)).unwrap();
        false
    }

    pub fn wake(&self) {
        self.cv.notify_all();
    }
}

/// Shared between the engine and the fetchers.
#[derive(Default)]
pub struct SourceCtl {
    pub stop: AtomicBool,
    pub gen: AtomicU64,
    /// Content-time target of the current generation (ns).
    pub target_ns: AtomicI64,
    /// Content-time playhead (ns), for read-ahead and SABR pacing.
    pub playhead_ns: AtomicI64,
}

impl SourceCtl {
    fn current(&self) -> (u64, i64) {
        (self.gen.load(Acquire), self.target_ns.load(Acquire))
    }
}

/// Trims the first batch of a generation: video starts at the last keyframe ≤ target.
struct Trim {
    target: Option<i64>,
}

impl Trim {
    fn apply(&mut self, kind: Kind, units: Vec<Unit>) -> Vec<Unit> {
        let Some(t) = self.target else { return units };
        if kind != Kind::Video {
            self.target = None;
            return units;
        }
        // Keep looking until a keyframe appears.
        let Some(first_key) = units.iter().position(|u| u.key) else { return Vec::new() };
        let start = units.iter().enumerate().filter(|(_, u)| u.key && u.pts_ns <= t).map(|(i, _)| i).last().unwrap_or(first_key);
        self.target = None;
        units.into_iter().skip(start).collect()
    }
}

fn frame_all(framer: &Framer, aus: Vec<AccessUnit>) -> Vec<Unit> {
    aus.into_iter()
        .map(|a| Unit { pts_ns: a.pts_ns, dur_ns: a.duration_ns, key: a.keyframe, data: framer.frame(&a) })
        .collect()
}

/// Resolves the playing video again (fresh stream URLs) when the old ones are refused.
pub type Refresh = Arc<dyn Fn() -> Option<yt_core::streams::Resolved> + Send + Sync>;

/// Wraps `f` so the video and audio fetchers share one re-resolve: a result younger than a minute
/// is handed out again instead of resolving twice.
pub fn refresher(f: impl Fn() -> Option<yt_core::streams::Resolved> + Send + Sync + 'static) -> Refresh {
    let last: Mutex<Option<(std::time::Instant, yt_core::streams::Resolved)>> = Mutex::new(None);
    Arc::new(move || {
        let mut g = last.lock().unwrap();
        if let Some((t, r)) = &*g {
            if t.elapsed() < Duration::from_secs(60) {
                return Some(r.clone());
            }
        }
        let r = f()?;
        *g = Some((std::time::Instant::now(), r.clone()));
        Some(r)
    })
}

/// The URL was refused or has expired (googlevideo answers 403; 410 for a gone URL).
fn url_refused(e: &str) -> bool {
    e.contains("HTTP 403") || e.contains("HTTP 410")
}

/// Points `track` at a freshly resolved URL for the same itag. False when that is not possible.
fn renew_url(track: &mut RangedTrack, refresh: &Option<Refresh>) -> bool {
    let Some(refresh) = refresh else { return false };
    let Some(r) = refresh() else { return false };
    let Some(f) = r.adaptive.iter().find(|f| f["itag"].as_i64() == Some(track.itag as i64)) else {
        crate::log(&format!("feed: itag {} is gone after re-resolving", track.itag));
        return false;
    };
    let Some(url) = f["url"].as_str() else { return false };
    track.url = url.to_string();
    track.user_agent = r.client.user_agent().to_string();
    crate::log(&format!("feed: itag {}: continuing with a fresh URL", track.itag));
    true
}

/// Ranged fetcher for one direct-URL format.
pub fn run_ranged(fmt: serde_json::Value, ua: String, lane: Arc<Lane>, ctl: Arc<SourceCtl>, kind: Kind, refresh: Option<Refresh>) {
    let http = match reqwest::blocking::Client::builder().timeout(Duration::from_secs(20)).build() {
        Ok(h) => h,
        Err(e) => return lane.fail(format!("http: {e}")),
    };
    let mut track = match RangedTrack::open(http, &fmt, &ua) {
        Ok(t) => t,
        Err(e) => return lane.fail(format!("open itag {}: {e:#}", fmt["itag"])),
    };
    let info = track.track().clone();
    let framer = match Framer::new(&info) {
        Ok(f) => f,
        Err(e) => return lane.fail(format!("{e:#}")),
    };
    lane.set_track(info);
    let max_bytes = if kind == Kind::Video { VIDEO_MAX_BYTES } else { AUDIO_MAX_BYTES };
    let lead = if kind == Kind::Audio { AUDIO_SEEK_LEAD_NS } else { 0 };
    let (mut gen, target) = ctl.current();
    let mut seg = track.segment_at(target - lead);
    let mut trim = Trim { target: Some(target) };
    let mut failures = 0;
    let mut renewals = 0;
    while !ctl.stop.load(Relaxed) {
        let (g, t) = ctl.current();
        if g != gen {
            gen = g;
            seg = track.segment_at(t - lead);
            trim = Trim { target: Some(t) };
        }
        if seg >= track.segments.len() {
            lane.finish(gen);
            let _ = lane.wait_room(&ctl, 0);
            continue;
        }
        if !lane.wait_room(&ctl, max_bytes) {
            continue;
        }
        // Give up between two ranges when the session stops or a seek starts a new generation.
        let abort = || ctl.stop.load(Relaxed) || ctl.gen.load(Acquire) != gen;
        match track.fetch_segment_abortable(seg, &abort) {
            Err(e) if e.is::<yt_core::demux::ranged::Aborted>() => continue,
            Ok(aus) => {
                failures = 0;
                renewals = 0;
                if ctl.gen.load(Acquire) != gen {
                    continue;
                }
                for u in trim.apply(kind, frame_all(&framer, aus)) {
                    if !lane.push(gen, u) {
                        break;
                    }
                }
                seg += 1;
            }
            Err(e) => {
                crate::log(&format!("feed: itag {} segment {seg}: {e:#}", track.itag));
                if url_refused(&format!("{e:#}")) && renewals < 3 {
                    renewals += 1;
                    if renew_url(&mut track, &refresh) {
                        continue;
                    }
                }
                failures += 1;
                if failures >= 5 {
                    return lane.fail(format!("itag {}: {e:#}", track.itag));
                }
                std::thread::sleep(Duration::from_millis(500 * failures));
            }
        }
    }
}

/// SABR fetcher feeding both lanes.
pub fn run_sabr(
    resolved: yt_core::streams::Resolved,
    tv: yt_core::bootstrap::TvConfig,
    itags: (i32, Option<i32>),
    video: Arc<Lane>,
    audio: Arc<Lane>,
    ctl: Arc<SourceCtl>,
) {
    use yt_core::sabr::{SabrStream, Track};
    let fail = |e: String| {
        video.fail(e.clone());
        audio.fail(e);
    };
    let http = match reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build() {
        Ok(h) => h,
        Err(e) => return fail(format!("http: {e}")),
    };
    let mut s = match SabrStream::from_resolved(http, &resolved, &tv, Some(itags.0), itags.1) {
        Ok(s) => s,
        Err(e) => return fail(format!("SABR: {e:#}")),
    };
    let (mut gen, target) = ctl.current();
    if target > 0 {
        s.seek(target / 1_000_000);
    }
    let mut trims = [Trim { target: Some(target) }, Trim { target: Some(target) }];
    let mut demux: [Option<(Box<dyn Demuxer>, Framer)>; 2] = [None, None];
    let mut failures = 0;
    while !ctl.stop.load(Relaxed) {
        let (g, t) = ctl.current();
        if g != gen {
            gen = g;
            s.seek(t / 1_000_000);
            trims = [Trim { target: Some(t) }, Trim { target: Some(t) }];
        }
        if s.is_finished() {
            video.finish(gen);
            audio.finish(gen);
            let _ = video.wait_room(&ctl, 0);
            continue;
        }
        let has_audio = itags.1.is_some();
        if !video.wait_room(&ctl, VIDEO_MAX_BYTES) || (has_audio && !audio.wait_room(&ctl, AUDIO_MAX_BYTES)) {
            continue;
        }
        s.set_player_time(Some(ctl.playhead_ns.load(Relaxed).max(0) / 1_000_000));
        let segs = match s.poll_segments() {
            Ok(v) => {
                failures = 0;
                v
            }
            Err(e) => {
                failures += 1;
                crate::log(&format!("feed: SABR poll: {e:#}"));
                if failures >= 5 {
                    return fail(format!("SABR: {e:#}"));
                }
                std::thread::sleep(Duration::from_millis(500 * failures));
                continue;
            }
        };
        if ctl.gen.load(Acquire) != gen {
            continue;
        }
        for seg in segs {
            let (i, lane, kind) = match seg.track {
                Track::Video => (0, &video, Kind::Video),
                Track::Audio => (1, &audio, Kind::Audio),
            };
            if seg.is_init {
                let Some(c) = Container::sniff(&seg.data) else { continue };
                let mut d = c.demuxer();
                if let Err(e) = d.push(&seg.data) {
                    return fail(format!("SABR init: {e:#}"));
                }
                let Some(info) = d.tracks().first().cloned() else { return fail("SABR init without tracks".into()) };
                match Framer::new(&info) {
                    Ok(f) => demux[i] = Some((d, f)),
                    Err(e) => return fail(format!("{e:#}")),
                }
                lane.set_track(info);
                continue;
            }
            let Some((d, f)) = demux[i].as_mut() else { continue };
            if let Err(e) = d.push(&seg.data) {
                crate::log(&format!("feed: SABR segment {}: {e:#}", seg.sequence));
                continue;
            }
            let aus: Vec<AccessUnit> = std::iter::from_fn(|| d.pop()).collect();
            for u in trims[i].apply(kind, frame_all(f, aus)) {
                if !lane.push(gen, u) {
                    break;
                }
            }
        }
    }
}

/// How far behind the newest segment live playback starts (YouTube's own players sit ~2-3
/// segments back; less risks stalling on a segment that is not published yet).
pub const LIVE_DELAY_NS: i64 = 12_000_000_000;

/// The newest segment of a live format: (head sequence number, head stream time in ns).
pub fn live_head(http: &reqwest::blocking::Client, url: &str, ua: &str) -> anyhow::Result<(u64, i64)> {
    let r = http.get(url).header("user-agent", ua).header("range", "bytes=0-0").send()?;
    let h = |k: &str| r.headers().get(k).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<i64>().ok());
    match (h("x-head-seqnum"), h("x-head-time-millis")) {
        (Some(seq), Some(ms)) => Ok((seq as u64, ms * 1_000_000)),
        _ => anyhow::bail!("no live head headers (HTTP {})", r.status()),
    }
}

/// Live fetcher for one direct-URL format: every `&sq=N` segment is a complete fMP4 file
/// (`ftyp moov emsg moof mdat`) on one continuous stream timeline, so each gets its own demuxer.
/// A generation's target maps to a sequence number through the head (`x-head-seqnum` /
/// `x-head-time-millis`); the whole DVR window back to sequence 0 is addressable. `edge` gets the
/// head's stream time for the HUD.
pub fn run_live(fmt: serde_json::Value, ua: String, lane: Arc<Lane>, ctl: Arc<SourceCtl>, kind: Kind, edge: Arc<AtomicI64>) {
    use yt_core::demux::mp4::Mp4Demuxer;
    let http = match reqwest::blocking::Client::builder().timeout(Duration::from_secs(20)).build() {
        Ok(h) => h,
        Err(e) => return lane.fail(format!("http: {e}")),
    };
    let Some(url) = fmt["url"].as_str().map(str::to_string) else { return lane.fail("live format without url".into()) };
    let itag = fmt["itag"].as_i64().unwrap_or(0);
    // A missing, zero or nonsensical target duration falls back to YouTube's usual 5 s (the
    // sequence arithmetic below divides by it).
    let seg_ns = match fmt["targetDurationSec"].as_f64() {
        Some(s) if (0.1..=60.0).contains(&s) => (s * 1e9) as i64,
        _ => 5_000_000_000,
    };
    let (mut head, mut head_ns) = match live_head(&http, &url, &ua) {
        Ok(h) => h,
        Err(e) => return lane.fail(format!("itag {itag}: {e:#}")),
    };
    edge.fetch_max(head_ns, Relaxed);
    // Sequence whose span covers stream time `t` (the head segment ends at `head_ns`).
    let seq_for = |t: i64, head: u64, head_ns: i64| -> u64 {
        let back = ((head_ns - t).max(0) + seg_ns - 1) / seg_ns;
        head.saturating_sub(back as u64)
    };
    let lead = if kind == Kind::Audio { AUDIO_SEEK_LEAD_NS } else { 0 };
    let (mut gen, target) = ctl.current();
    let mut seq = seq_for(target - lead, head, head_ns);
    let mut trim = Trim { target: Some(target) };
    let mut framer: Option<Framer> = None;
    let mut failures = 0u32;
    // Consecutive answers with an HTTP error status other than "not published yet".
    let mut http_failures = 0u32;
    let mut idle_since: Option<std::time::Instant> = None;
    while !ctl.stop.load(Relaxed) {
        let (g, t) = ctl.current();
        if g != gen {
            gen = g;
            seq = seq_for(t - lead, head, head_ns);
            trim = Trim { target: Some(t) };
        }
        if !lane.wait_room(&ctl, if kind == Kind::Video { VIDEO_MAX_BYTES } else { AUDIO_MAX_BYTES }) {
            continue;
        }
        let r = http.get(format!("{url}&sq={seq}")).header("user-agent", &ua).send();
        let resp = match r {
            Ok(r) => r,
            Err(e) => {
                failures += 1;
                crate::log(&format!("feed: live itag {itag} sq {seq}: {e}"));
                if failures >= 8 {
                    return lane.fail(format!("live itag {itag}: {e}"));
                }
                std::thread::sleep(Duration::from_millis(500 * failures as u64));
                continue;
            }
        };
        let hv = |k: &str| resp.headers().get(k).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<i64>().ok());
        if let (Some(s), Some(ms)) = (hv("x-head-seqnum"), hv("x-head-time-millis")) {
            if s as u64 > head {
                idle_since = None;
            }
            head = head.max(s as u64);
            head_ns = head_ns.max(ms * 1_000_000);
            edge.fetch_max(head_ns, Relaxed);
        }
        let status = resp.status();
        let code = status.as_u16();
        if !status.is_success() && !matches!(code, 404 | 204) {
            // A refused or expired URL (403/410) or a server error: these do not heal by waiting
            // for the head, so they count as failures (generously, as the server may recover).
            http_failures += 1;
            let limit = if matches!(code, 403 | 410) { 10 } else { 60 };
            if http_failures >= limit {
                return lane.fail(format!("live itag {itag} sq {seq}: HTTP {status}"));
            }
        } else if status.is_success() && code != 204 {
            http_failures = 0;
        }
        if !status.is_success() || code == 204 {
            // Not published yet (ahead of the head), or the stream ended.
            let since = *idle_since.get_or_insert_with(std::time::Instant::now);
            if seq > head && since.elapsed() > Duration::from_secs(60) {
                crate::log(&format!("feed: live itag {itag}: no new segment for 60 s, treating as ended"));
                lane.finish(gen);
                let _ = lane.wait_room(&ctl, 0);
                continue;
            }
            if seq + 3 < head && status.as_u16() == 404 {
                // Fell out of the DVR window: jump forward.
                seq += 1;
                continue;
            }
            std::thread::sleep(Duration::from_millis(1000));
            continue;
        }
        let body = match resp.bytes() {
            Ok(b) => b,
            Err(e) => {
                failures += 1;
                crate::log(&format!("feed: live itag {itag} sq {seq} body: {e}"));
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        failures = 0;
        let mut d = Mp4Demuxer::new();
        if let Err(e) = d.push(&body) {
            crate::log(&format!("feed: live itag {itag} sq {seq}: {e:#}"));
            seq += 1;
            continue;
        }
        if framer.is_none() {
            let Some(info) = d.tracks().first().cloned() else {
                seq += 1;
                continue;
            };
            match Framer::new(&info) {
                Ok(f) => framer = Some(f),
                Err(e) => return lane.fail(format!("{e:#}")),
            }
            lane.set_track(info);
        }
        let mut aus = Vec::new();
        while let Some(a) = d.pop() {
            aus.push(a);
        }
        if ctl.gen.load(Acquire) != gen {
            continue;
        }
        for u in trim.apply(kind, frame_all(framer.as_ref().unwrap(), aus)) {
            if !lane.push(gen, u) {
                break;
            }
        }
        seq += 1;
    }
}

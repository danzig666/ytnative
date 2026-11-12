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

    /// Fetcher side: false when `gen` is stale (unit dropped).
    fn push(&self, gen: u64, u: Unit) -> bool {
        let mut s = self.st.lock().unwrap();
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
        let mut s = self.st.lock().unwrap();
        let u = s.q.pop_front()?;
        s.bytes -= u.data.len();
        self.cv.notify_all();
        Some(u)
    }

    pub fn peek_pts(&self) -> Option<i64> {
        self.st.lock().unwrap().q.front().map(|u| u.pts_ns)
    }

    /// Engine side, on a seek: drop everything and expect generation `gen`. Must be called
    /// before the generation is published in [`SourceCtl::gen`].
    pub fn reset(&self, gen: u64) {
        let mut s = self.st.lock().unwrap();
        s.q.clear();
        s.bytes = 0;
        s.gen = gen;
        s.finished = false;
        s.end_ns = 0;
        self.cv.notify_all();
    }

    fn set_track(&self, t: TrackInfo) {
        self.st.lock().unwrap().track = Some(t);
        self.cv.notify_all();
    }

    pub fn track(&self) -> Option<TrackInfo> {
        self.st.lock().unwrap().track.clone()
    }

    fn finish(&self, gen: u64) {
        let mut s = self.st.lock().unwrap();
        if s.gen == gen {
            s.finished = true;
        }
        self.cv.notify_all();
    }

    fn fail(&self, e: String) {
        let mut s = self.st.lock().unwrap();
        s.error.get_or_insert(e);
        self.cv.notify_all();
    }

    pub fn error(&self) -> Option<String> {
        self.st.lock().unwrap().error.clone()
    }

    /// All units of the current generation were delivered and consumed.
    pub fn drained(&self) -> bool {
        let s = self.st.lock().unwrap();
        s.finished && s.q.is_empty()
    }

    pub fn is_empty(&self) -> bool {
        self.st.lock().unwrap().q.is_empty()
    }

    /// Fetcher side: true when there is room; otherwise blocks (≤ 200 ms) and returns false.
    fn wait_room(&self, ctl: &SourceCtl, max_bytes: usize) -> bool {
        let s = self.st.lock().unwrap();
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

/// Ranged fetcher for one direct-URL format.
pub fn run_ranged(fmt: serde_json::Value, ua: String, lane: Arc<Lane>, ctl: Arc<SourceCtl>, kind: Kind) {
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
        match track.fetch_segment(seg) {
            Ok(aus) => {
                failures = 0;
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
                failures += 1;
                crate::log(&format!("feed: itag {} segment {seg}: {e:#}", track.itag));
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

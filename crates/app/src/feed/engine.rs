//! The feed thread: Load → prime → Play → feed with a feed-ahead throttle; pause/resume/seek;
//! end-of-stream; teardown. Every StarfishMediaAPIs call of a session happens on this thread
//! (so `Seek` never races a `Feed`).
//!
//! Timeline: the pipeline is fed `pts_fed = pts_content - shift`. The first Load uses
//! `shift = pts of the first keyframe` (so feeding starts at 0, plx-native's proven shape); a
//! native seek switches to `shift = 0` and `Seek(<keyframe ms>)` re-anchors the pipeline's segment
//! on content time (plx-native `NativeSeek`). Position = FRAMEREADY's fed PTS + shift.

use super::payload::{self, AudioDecl, VideoDecl};
use super::source::{self, Lane, SourceCtl, Unit};
use super::sys::{self, Events, FeedReply, PRES_NONE};
use crate::player::{Phase, Status};
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering::*};
use std::sync::atomic::{AtomicI64, AtomicU32};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use yt_core::demux::{Codec, Kind, TrackInfo};

/// Feed-ahead throttle: video at most this far ahead of the presented position (plx-native).
pub const MAX_FEED_AHEAD_NS: i64 = 1_600_000_000;
/// Audio may run this much further ahead (it is the clock master, and cheap).
pub const AUDIO_SLACK_NS: i64 = 2_000_000_000;
/// Buffer this much video and audio (paused) before Play, after a Load or a seek.
pub const PRIME_NS: i64 = 700_000_000;
pub const PRIME_AUDIO_NS: i64 = 300_000_000;
/// ...or start anyway once this much video is buffered (audioless / starved audio)...
pub const PRIME_VIDEO_MAX_NS: i64 = 2_500_000_000;
/// ...but with an audio track, only after waiting this long for its first audio: video
/// downloads faster, and Play without audio left the picture without its clock (about one start
/// in ten on the C3; after a reload it ran at about 3x).
const PRIME_AUDIO_WAIT: Duration = Duration::from_secs(8);
/// Cap on what is fed past the anchor while no position is known.
const BLIND_AHEAD_NS: i64 = 4_000_000_000;
/// Per-lane wall-clock budget per tick once the lane is healthy.
const LANE_SLICE: Duration = Duration::from_millis(3);
/// How long a seek may wait for the first video unit of its generation (slow networks included).
const SEEK_LANDING_TIMEOUT: Duration = Duration::from_secs(30);
/// Client-side trick play (found by youtube-webos-cobalt-adfree): a pipeline loaded with it
/// changes speed in place. On the C3 such a pipeline reports loadCompleted only after 4 to
/// 10 s, and some videos (a 720p AV1 one; a 4K one fed early) never show a picture, so videos
/// load the ordinary way and a 4K one is reloaded with trick play only when a speed other than
/// 1x is chosen, waiting for loadCompleted. No picture within `TRICK_FIRST_FRAME` of Play: back
/// to the ordinary pipeline, and the speed goes to the system player. Other videos change speed
/// on the system player.
const TRICK_PLAY: bool = false;
const TRICK_MIN_HEIGHT: u32 = 2160;
const TRICK_FIRST_FRAME: Duration = Duration::from_secs(8);
/// How long playback is timed after a speed change to see whether the TV applied it.
const RATE_PROBE: Duration = Duration::from_secs(4);
/// Firmware number for "decoder taken back by policy" (plx-native, webOS 4.10).
const DECODER_REVOKED: i64 = 600;
/// webOS 4 (ACB): how long a teardown waits for the main thread to set the plane UNLOADED before
/// the object is destroyed anyway (the main thread answers within a frame or two).
const PLANE_RELEASE_WAIT: Duration = Duration::from_secs(2);

pub enum Cmd {
    Pause,
    Resume,
    Seek(u64),
    Volume(i32),
    /// Playback speed (1.0 = normal).
    Rate(f32),
}

#[derive(Clone)]
pub enum Transport {
    Direct,
    /// A live stream: `&sq=N` segments from the formats' URLs (see `source::run_live`).
    Live,
    Sabr(yt_core::bootstrap::TvConfig),
}

pub struct Plan {
    pub video_fmt: Value,
    pub audio_fmt: Option<Value>,
    pub transport: Transport,
    pub resolved: yt_core::streams::Resolved,
    pub user_agent: String,
    pub app_id: String,
    pub window_id: Option<String>,
    pub start_ms: u64,
    pub duration_ms: u64,
    /// Fresh URLs for an expired or refused stream.
    pub refresh: Option<source::Refresh>,
}

pub struct Shared {
    pub status: Arc<Mutex<Status>>,
    pub cmds: Mutex<VecDeque<Cmd>>,
    pub stop: AtomicBool,
    /// Decoded raster to place the exported window for (taken by the main thread).
    pub place: Mutex<Option<(u32, u32)>>,
    /// webOS 4 (ACB): what the main thread's plane bind reads and records (`FeedPlayer::acb_tick`).
    pub plane: Mutex<PlaneLink>,
    /// webOS 4: epoch of a pipeline whose plane binding the main thread must set UNLOADED now
    /// (between the callback gate's retirement and the object's destruction), and its answer.
    pub release_req: AtomicU32,
    pub release_ack: AtomicU32,
}

/// webOS 4 (ACB). One lock so a pipeline is withdrawn and its binding read atomically: the main
/// thread binds only while holding it, so once `pipe` is withdrawn no new binding of it can start.
#[derive(Default)]
pub struct PlaneLink {
    /// The live pipeline's epoch and events (set after each Load, withdrawn before its teardown).
    pub pipe: Option<(u32, Arc<Events>)>,
    /// Epoch of the pipeline Play was last issued on (0: none yet). The plane binds only after it:
    /// stricter than plx-native, which binds at loadCompleted (before Play on its prime path), so
    /// a reload while paused stays black until the resume.
    pub played: u32,
    /// Epoch the main thread bound the plane to (0: none).
    pub bound: u32,
}

impl Shared {
    pub fn new(status: Arc<Mutex<Status>>) -> Arc<Shared> {
        Arc::new(Shared {
            status,
            cmds: Mutex::new(VecDeque::new()),
            stop: AtomicBool::new(false),
            place: Mutex::new(None),
            plane: Mutex::new(PlaneLink::default()),
            release_req: AtomicU32::new(0),
            release_ack: AtomicU32::new(0),
        })
    }
}

fn wake() {
    plx_machine::present::wake_from_worker();
}

fn set_phase(sh: &Shared, p: Phase) {
    let mut s = sh.status.lock().unwrap();
    if s.phase != p {
        s.phase = p;
        drop(s);
        wake();
    }
}

fn fail(sh: &Shared, why: String) {
    crate::log(&format!("feed: {why}"));
    set_phase(sh, Phase::Failed(why));
}

/// One loaded StarfishMediaAPIs object.
struct Pipe {
    epoch: u32,
    ev: Arc<Events>,
}

impl Pipe {
    /// `max_wait`: how long to wait for loadCompleted before feeding anyway (a trick-play
    /// pipeline only completes once it has data: 4 to 10 s of nothing on the C3). The wait gives
    /// up at once when the session is being stopped.
    fn open(payload: &str, sh: &Shared, max_wait: Duration) -> Result<Pipe, String> {
        let stop = &sh.stop;
        let ev = Events::new();
        let epoch = sys::route(ev.clone());
        let t0 = Instant::now();
        if !sys::load(payload, epoch) {
            sys::teardown(Duration::from_secs(3), || {});
            sys::unroute(epoch);
            return Err(if sys::blocked() { "Starfish is quarantined (restart the app)".into() } else { "Starfish refused the Load".into() });
        }
        crate::log(&format!("feed: Load returned after {} ms", t0.elapsed().as_millis()));
        while !(ev.load_completed.load(Relaxed) || sys::is_load_completed()) {
            if let Some((num, s)) = ev.error.lock().unwrap().clone() {
                let p = Pipe { epoch, ev: ev.clone() };
                p.close(sh);
                return Err(format!("pipeline refused the stream (error {num}: {})", s.chars().take(120).collect::<String>()));
            }
            if t0.elapsed() > max_wait {
                crate::log(&format!("feed: no loadCompleted after {} ms; feeding anyway", max_wait.as_millis()));
                break;
            }
            if stop.load(Relaxed) {
                break; // stopping: the caller's loop sees the flag and tears down
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if ev.load_completed.load(Relaxed) || sys::is_load_completed() {
            crate::log(&format!("feed: loadCompleted after {} ms", t0.elapsed().as_millis()));
        }
        sh.plane.lock().unwrap().pipe = Some((epoch, ev.clone()));
        Ok(Pipe { epoch, ev })
    }

    fn close(self, sh: &Shared) {
        // webOS 4: withdraw the pipeline from the plane bind first; a binding made on it is set
        // UNLOADED between the gate's retirement and D1 (plx-native's order, `native_release.rs`).
        let bound = {
            let mut link = sh.plane.lock().unwrap();
            if link.pipe.as_ref().is_some_and(|(e, _)| *e == self.epoch) {
                link.pipe = None;
            }
            link.bound == self.epoch
        };
        let destroyed = sys::teardown(Duration::from_secs(3), || {
            if bound {
                release_plane(sh, self.epoch);
            }
        });
        sys::unroute(self.epoch);
        crate::log(&format!("feed: pipeline closed (destroyed={destroyed})"));
    }
}

/// webOS 4 (ACB): the plane's `setState(UNLOADED)` is a main-thread verb, so ask the main thread
/// (`FeedPlayer::tick`, or `FeedPlayer::stop` while it waits for this thread) and wait for its
/// answer. Without one in time the object is destroyed first and the main thread unloads later.
fn release_plane(sh: &Shared, epoch: u32) {
    let t0 = Instant::now();
    sh.release_req.store(epoch, Release);
    wake();
    while sh.release_ack.load(Acquire) != epoch {
        if t0.elapsed() > PLANE_RELEASE_WAIT {
            crate::log("feed: the main thread did not unload the video plane in time; destroying the pipeline first");
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    crate::log(&format!("feed: video plane unloaded before the pipeline's destruction ({} ms)", t0.elapsed().as_millis()));
}

/// Play was issued on `epoch`: the main thread may bind the plane to it now.
fn played(sh: &Shared, epoch: u32) {
    sh.plane.lock().unwrap().played = epoch;
}

#[derive(Default)]
struct LaneFeed {
    pending: Option<Unit>,
    fed_max: Option<i64>,
    errors: u32,
}

/// Feeds one lane up to `limit` (fed timeline). Returns the number of units fed, or an error.
fn feed_lane(lane: &Lane, lf: &mut LaneFeed, es: i32, shift: i64, floor: i64, limit: i64, starved: bool) -> Result<usize, String> {
    let t0 = Instant::now();
    let mut n = 0;
    loop {
        let Some(u) = lf.pending.take().or_else(|| lane.pop()) else { break };
        if u.pts_ns < floor {
            continue; // audio before the video anchor
        }
        let fed = u.pts_ns - shift;
        if fed < 0 {
            continue;
        }
        if fed > limit {
            lf.pending = Some(u);
            break;
        }
        match sys::feed(&u.data, fed, es) {
            FeedReply::Ok => {
                // The limit below is for consecutive failures, not for sporadic ones over a long video.
                lf.errors = 0;
                lf.fed_max = Some(lf.fed_max.map_or(fed, |m| m.max(fed)));
                n += 1;
            }
            FeedReply::BufferFull => {
                lf.pending = Some(u);
                break;
            }
            FeedReply::Error => {
                lf.errors += 1;
                if lf.errors > 100 {
                    return Err("Feed keeps failing".into());
                }
                lf.pending = Some(u);
                break;
            }
        }
        if !starved && t0.elapsed() > LANE_SLICE {
            break;
        }
    }
    Ok(n)
}

fn video_decl(t: &TrackInfo) -> Option<VideoDecl> {
    match t.codec {
        Codec::H264 => Some(VideoDecl::H264),
        Codec::Vp9 => Some(VideoDecl::Vp9),
        Codec::Av1 => Some(VideoDecl::Av1),
        _ => None,
    }
}

fn audio_decl(t: &TrackInfo) -> Option<AudioDecl> {
    match t.codec {
        Codec::Aac => Some(AudioDecl::Aac),
        Codec::Opus => Some(AudioDecl::Opus { channels: t.channels.max(1) as u8, head: t.config.clone() }),
        _ => None,
    }
}

fn num(v: &Value) -> f64 {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0)
}

pub fn run(sh: Arc<Shared>, plan: Plan) {
    let ctl = Arc::new(SourceCtl::default());
    let edge = Arc::new(AtomicI64::new(0));
    let mut start_ns = plan.start_ms as i64 * 1_000_000;
    if matches!(plan.transport, Transport::Live) {
        // Start a little behind the newest segment, on the stream's own timeline. The head request
        // runs on a helper thread so that a stop (the main thread joins this one) is not held up
        // by a slow network; an abandoned request simply finishes in the background.
        let Some(url) = plan.video_fmt["url"].as_str().map(str::to_string) else {
            return fail(&sh, "live: format without url".into());
        };
        let ua = plan.user_agent.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new().name("feed-head".into()).stack_size(512 << 10).spawn(move || {
            let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(15)).build().unwrap_or_default();
            let _ = tx.send(source::live_head(&http, &url, &ua));
        });
        if spawned.is_err() {
            return fail(&sh, "live: could not start the head request".into());
        }
        let head = loop {
            if sh.stop.load(Relaxed) {
                return;
            }
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(r) => break Some(r),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break None,
            }
        };
        match head {
            Some(Ok((_, head_ns))) => {
                edge.store(head_ns, Relaxed);
                start_ns = (head_ns - source::LIVE_DELAY_NS).max(0);
            }
            Some(Err(e)) => return fail(&sh, format!("live: {e:#}")),
            None => return fail(&sh, "live: the head request ended without an answer".into()),
        }
        let mut s = sh.status.lock().unwrap();
        s.live = true;
        s.live_edge_ms = (edge.load(Relaxed) / 1_000_000) as u64;
        s.position_ms = (start_ns / 1_000_000) as u64;
        s.duration_ms = 0;
    }
    ctl.target_ns.store(start_ns, Release);
    ctl.playhead_ns.store(start_ns, Relaxed);
    let (video, audio) = (Lane::new(), Lane::new());
    let has_audio = plan.audio_fmt.is_some();
    spawn_fetchers(&plan, &video, &audio, &ctl, &edge);
    let r = session(&sh, &plan, &ctl, &video, &audio, has_audio, &edge);
    ctl.stop.store(true, Relaxed);
    video.wake();
    audio.wake();
    if let Err(e) = r {
        fail(&sh, e);
    }
}

fn spawn_fetchers(plan: &Plan, video: &Arc<Lane>, audio: &Arc<Lane>, ctl: &Arc<SourceCtl>, edge: &Arc<AtomicI64>) {
    // A fetcher that panics would otherwise leave its lane silent and the engine waiting for
    // media forever: the panic is turned into a lane error, which ends the session as a failure.
    let spawn = |name: &str, f: Box<dyn FnOnce() + Send>| {
        let (v, a, n) = (video.clone(), audio.clone(), name.to_string());
        let body = move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err() {
                crate::log(&format!("feed: {n} panicked"));
                v.fail(format!("{n} stopped unexpectedly"));
                a.fail(format!("{n} stopped unexpectedly"));
            }
        };
        if std::thread::Builder::new().name(name.into()).stack_size(512 << 10).spawn(body).is_err() {
            crate::log(&format!("feed: could not spawn {name}"));
        }
    };
    match &plan.transport {
        Transport::Direct => {
            let (f, ua, l, c, rf) = (plan.video_fmt.clone(), plan.user_agent.clone(), video.clone(), ctl.clone(), plan.refresh.clone());
            spawn("feed-v", Box::new(move || source::run_ranged(f, ua, l, c, Kind::Video, rf)));
            if let Some(af) = plan.audio_fmt.clone() {
                let (ua, l, c, rf) = (plan.user_agent.clone(), audio.clone(), ctl.clone(), plan.refresh.clone());
                spawn("feed-a", Box::new(move || source::run_ranged(af, ua, l, c, Kind::Audio, rf)));
            }
        }
        Transport::Live => {
            let (f, ua, l, c, e) = (plan.video_fmt.clone(), plan.user_agent.clone(), video.clone(), ctl.clone(), edge.clone());
            spawn("feed-lv", Box::new(move || source::run_live(f, ua, l, c, Kind::Video, e)));
            if let Some(af) = plan.audio_fmt.clone() {
                let (ua, l, c, e) = (plan.user_agent.clone(), audio.clone(), ctl.clone(), Arc::new(AtomicI64::new(0)));
                spawn("feed-la", Box::new(move || source::run_live(af, ua, l, c, Kind::Audio, e)));
            }
        }
        Transport::Sabr(tv) => {
            let itag = |f: &Value| f["itag"].as_i64().unwrap_or(0) as i32;
            let itags = (itag(&plan.video_fmt), plan.audio_fmt.as_ref().map(itag));
            let (r, tv, v, a, c) = (plan.resolved.clone(), tv.clone(), video.clone(), audio.clone(), ctl.clone());
            spawn("feed-sabr", Box::new(move || source::run_sabr(r, tv, itags, v, a, c)));
        }
    }
}

fn session(sh: &Arc<Shared>, plan: &Plan, ctl: &SourceCtl, video: &Lane, audio: &Lane, has_audio: bool, edge: &AtomicI64) -> Result<(), String> {
    let live = matches!(plan.transport, Transport::Live);
    // ---- wait for the tracks and the first keyframe ----
    let t0 = Instant::now();
    loop {
        if sh.stop.load(Relaxed) {
            return Ok(());
        }
        if let Some(e) = video.error().or_else(|| audio.error()) {
            return Err(e);
        }
        let ready_v = video.track().is_some() && video.peek_pts().is_some();
        let ready_a = !has_audio || audio.track().is_some();
        if ready_v && ready_a {
            break;
        }
        if t0.elapsed() > Duration::from_secs(30) {
            return Err("timed out waiting for media".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let vt = video.track().unwrap();
    let at = if has_audio { audio.track() } else { None };
    let vdecl = video_decl(&vt).ok_or_else(|| format!("unsupported video codec {:?}", vt.codec))?;
    let adecl = match &at {
        Some(t) => Some(audio_decl(t).ok_or_else(|| format!("unsupported audio codec {:?}", t.codec))?),
        None => None,
    };
    let mut fps = num(&plan.video_fmt["fps"]);
    if vt.default_duration_ns > 0 {
        fps = 1e9 / vt.default_duration_ns as f64;
    }
    let spec = payload::Spec {
        app_id: plan.app_id.clone(),
        window_id: plan.window_id.clone(),
        video: vdecl,
        width: vt.width,
        height: vt.height,
        fps,
        audio: adecl,
        trick_play: false,
    };
    let plain_payload = payload::build(&spec);
    // The pipeline a speed change reloads into (4K only), until it failed once.
    let mut trick_payload = (TRICK_PLAY && !live && vt.height >= TRICK_MIN_HEIGHT).then(|| payload::build(&payload::Spec { trick_play: true, ..spec.clone() }));
    let mut trick = false;
    let mut payload = plain_payload.clone();
    let load_wait = Duration::from_secs(10);
    // Trick play: the time by which the first frame must show (else reload without it).
    let mut first_frame_by: Option<Instant> = None;
    // A reload asked for by this loop itself (the trick-play fallback), taken as a seek.
    let mut self_seek: Option<u64> = None;
    crate::log(&format!("feed: speed in place {}", if trick_payload.is_some() { "possible (4K)" } else { "not possible" }));
    let env = payload::sink_envelope(vdecl, vt.width, vt.height, fps);
    crate::log(&format!(
        "feed: Load {} {}x{}@{fps:.3} (declared {}x{}@{}) audio={:?} hdr={:?} window={}",
        vdecl.name(),
        vt.width,
        vt.height,
        env.w,
        env.h,
        env.fps,
        at.as_ref().map(|t| &t.codec),
        vt.color,
        plan.window_id.as_deref().unwrap_or("none (ACB)")
    ));
    {
        let mut s = sh.status.lock().unwrap();
        s.width = vt.width;
        s.height = vt.height;
        s.fps = fps.round() as u32;
    }
    *sh.place.lock().unwrap() = Some((vt.width, vt.height));

    let mut shift = video.peek_pts().unwrap_or(0).max(0);
    let mut pipe = Pipe::open(&payload, sh, load_wait)?;
    // Since when the current priming (Load or seek) has been waiting.
    let mut prime_since = Instant::now();
    let mut base = 0i64; // fed-timeline anchor of the current Load/seek
    let mut floor = shift; // content-time: no audio before the video anchor
    let mut vf = LaneFeed::default();
    let mut af = LaneFeed::default();
    let mut priming = true;
    let mut user_paused = false;
    // Playback speed asked for; applied once playing and again after every Play (a reload or a
    // seek that rebuilds the pipeline starts at normal speed).
    let mut rate: f32 = 1.0;
    // The speed asked for. It is sent once the pipeline is playing and has shown a frame (a
    // SetPlayRate before the first frame stalls the picture on the C3; youtube-webos-cobalt-adfree
    // found the same), after every Play, seek and reload.
    let mut wanted: f32 = 1.0;
    // Some TVs accept SetPlayRate and keep playing at normal speed: after a speed change the
    // reported position is timed against the clock, and a speed that did not take is refused.
    let mut probe_rate = false;
    let mut rate_probe: Option<(Instant, i64)> = None;
    let mut eos_pushed = false;
    let mut seeking: Option<i64> = None;
    let mut gen = 0u64;
    let mut native_seek = true;
    // The next seek reloads the pipeline instead of a native Seek (leaving a speed: see Rate).
    let mut reload_seek = false;
    let mut play_at: Option<Instant> = None;
    let mut seek_started = Instant::now();
    let duration_ns = plan.duration_ms as i64 * 1_000_000;

    let result = loop {
        if sh.stop.load(Relaxed) {
            break Ok(());
        }
        // ---- commands ----
        let cmds: Vec<Cmd> = sh.cmds.lock().unwrap().drain(..).collect();
        let mut seek_to = self_seek.take();
        // The wall-clock estimate (below) counts from `base` at `play_at` at the current speed:
        // move that anchor to now before the speed changes or the clock stops (pause).
        let reanchor = |base: &mut i64, play_at: &mut Option<Instant>, rate: f32, running: bool| {
            if let Some(t) = play_at.take() {
                *base += (t.elapsed().as_nanos() as f64 * rate as f64) as i64;
                if running {
                    *play_at = Some(Instant::now());
                }
            }
        };
        for c in cmds {
            match c {
                Cmd::Pause => {
                    rate_probe = None;
                    reanchor(&mut base, &mut play_at, rate, false);
                    user_paused = true;
                    if !priming && seeking.is_none() {
                        sys::pause();
                    }
                    set_phase(sh, Phase::Paused);
                }
                Cmd::Resume => {
                    user_paused = false;
                    if !priming && seeking.is_none() && sys::play() {
                        played(sh, pipe.epoch);
                    }
                    set_phase(sh, Phase::Playing);
                }
                Cmd::Seek(ms) => seek_to = Some(ms), // coalesce: the last one wins
                Cmd::Volume(v) => {
                    sys::set_volume(v);
                }
                Cmd::Rate(r) => {
                    if (r - wanted).abs() <= 0.001 {
                        // The same speed again: nothing to do.
                    } else if (r - 1.0).abs() > 0.01 || (rate - 1.0).abs() <= 0.01 {
                        // Sent by the step below once a frame shows.
                        wanted = r;
                    } else {
                        // Back to normal: the firmware answers SetPlayRate 1.0 with success and
                        // keeps the old speed (youtube-webos-cobalt-adfree found the same), so the
                        // pipeline is reloaded at the current position, which starts at 1x.
                        crate::log(&format!("feed: speed {rate} -> 1: reloading at the current position"));
                        reanchor(&mut base, &mut play_at, rate, true);
                        rate = 1.0;
                        wanted = 1.0;
                        probe_rate = false;
                        reload_seek = true;
                        if trick {
                            trick = false;
                            payload = plain_payload.clone();
                        }
                        seek_to = Some(sh.status.lock().unwrap().position_ms);
                    }
                }
            }
        }
        if let Some(ms) = seek_to {
            let t = if live {
                // Anywhere in the DVR window, but not past what is published.
                (ms as i64 * 1_000_000).clamp(0, (edge.load(Relaxed) - source::LIVE_DELAY_NS).max(0))
            } else {
                (ms as i64 * 1_000_000).clamp(0, (duration_ns - 1_000_000_000).max(0))
            };
            if !priming && seeking.is_none() {
                sys::pause();
            }
            gen += 1;
            video.reset(gen);
            audio.reset(gen);
            ctl.target_ns.store(t, Release);
            ctl.gen.store(gen, Release);
            ctl.playhead_ns.store(t, Relaxed);
            vf = LaneFeed::default();
            af = LaneFeed::default();
            pipe.ev.pres_fed_ns.store(PRES_NONE, Relaxed);
            pipe.ev.eos.store(false, Relaxed);
            eos_pushed = false;
            priming = true;
            play_at = None;
            seeking = Some(t);
            seek_started = Instant::now();
            prime_since = Instant::now();
            rate_probe = None;
            probe_rate = false;
            rate = 1.0;
            let mut s = sh.status.lock().unwrap();
            s.position_ms = t as u64 / 1_000_000;
            if !matches!(s.phase, Phase::Paused | Phase::Failed(_)) {
                s.phase = Phase::Loading;
            }
            drop(s);
            wake();
        }
        // ---- seek landing: the first video unit of the new generation is its keyframe ----
        if let Some(t) = seeking {
            if let Some(e) = video.error() {
                break Err(e);
            }
            let Some(k) = video.peek_pts() else {
                // Without a deadline a generation that never yields a video unit (its fetcher
                // finished empty-handed, or keeps waiting for a segment) would leave the player
                // loading forever.
                if video.drained() {
                    break Err("no video after the seek target".into());
                }
                if seek_started.elapsed() > SEEK_LANDING_TIMEOUT {
                    break Err("timed out waiting for media after a seek".into());
                }
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            let forced_reload = std::mem::take(&mut reload_seek);
            let landed = native_seek && !forced_reload && sys::seek_ms(k.max(0) / 1_000_000);
            if landed {
                // A position report that arrived between the seek command and this Seek belongs
                // to the old timeline (and the old shift): drop it.
                pipe.ev.pres_fed_ns.store(PRES_NONE, Relaxed);
                shift = 0;
                base = k.max(0);
            } else {
                if native_seek && !forced_reload {
                    crate::log("feed: native Seek refused; reloading per seek from now on");
                    native_seek = false;
                }
                pipe.close(sh);
                pipe = Pipe::open(&payload, sh, load_wait)?;
                shift = k.max(0);
                base = 0;
            }
            floor = k;
            seeking = None;
            crate::log(&format!("feed: seek to {} ms landed on keyframe {} ms ({})", t / 1_000_000, k / 1_000_000, if landed { "Seek" } else { "reload" }));
        }

        // ---- position / events ----
        if let Some(e) = video.error().or_else(|| audio.error()) {
            break Err(e);
        }
        let pres = pipe.ev.pres_fed_ns.load(Relaxed);
        if let Some((num, s)) = pipe.ev.error.lock().unwrap().take() {
            if pipe.ev.frames.load(Relaxed) == 0 {
                break Err(format!("pipeline refused the stream (error {num}: {})", s.chars().take(120).collect::<String>()));
            }
            if num == DECODER_REVOKED {
                break Err("the TV took the video decoder back".into());
            }
        }
        if pres != PRES_NONE {
            first_frame_by = None;
        } else if trick && !user_paused && seeking.is_none() && first_frame_by.is_some_and(|t| Instant::now() > t) {
            // No picture with trick play: load this video the ordinary way from here on.
            crate::log("feed: no frame with trick play; reloading without it");
            trick = false;
            trick_payload = None;
            payload = plain_payload.clone();
            first_frame_by = None;
            reload_seek = true;
            self_seek = Some(sh.status.lock().unwrap().position_ms);
        }
        if !trick && (wanted - 1.0).abs() > 0.01 && seeking.is_none() && self_seek.is_none() {
            if let Some(tp) = trick_payload.clone() {
                // A 4K video: reload it with trick play here, the speed is sent once it shows.
                crate::log(&format!("feed: speed {wanted}: reloading with trick play"));
                trick = true;
                payload = tp;
                reload_seek = true;
                self_seek = Some(sh.status.lock().unwrap().position_ms);
            } else {
                // Without trick play SetPlayRate stops the picture (C3): the system player.
                crate::log(&format!("feed: speed {wanted} needs trick play; handing over to the system player"));
                wanted = 1.0;
                sh.status.lock().unwrap().rate_refused = true;
            }
        }
        if pres != PRES_NONE && trick && self_seek.is_none() && !priming && !user_paused && seeking.is_none() && (wanted - rate).abs() > 0.001 && (wanted - 1.0).abs() > 0.01 {
            let ok = sys::set_play_rate(wanted);
            crate::log(&format!("feed: SetPlayRate {wanted} -> {ok}"));
            if ok {
                reanchor(&mut base, &mut play_at, rate, true);
                rate = wanted;
                probe_rate = true;
                rate_probe = None;
            } else {
                // Refused: the host restarts the video on the system player.
                wanted = rate;
                sh.status.lock().unwrap().rate_refused = true;
            }
        }
        if pres != PRES_NONE && probe_rate && !priming && !user_paused && seeking.is_none() {
            match rate_probe {
                None => rate_probe = Some((Instant::now(), pres)),
                Some((t0, p0)) if t0.elapsed() >= RATE_PROBE => {
                    let ratio = (pres - p0) as f64 / t0.elapsed().as_nanos() as f64;
                    // Taken only when playback runs near the asked speed: the C3 accepts 1.5x and
                    // stops the picture (0x), which is no more a speed change than 1x is.
                    let took = (ratio - rate as f64).abs() < (rate as f64 - 1.0).abs() / 2.0;
                    crate::log(&format!("feed: speed {rate}: playing at {ratio:.2}x ({})", if took { "applied" } else { "not applied" }));
                    if !took {
                        rate = 1.0;
                        wanted = 1.0;
                        sh.status.lock().unwrap().rate_refused = true;
                    }
                    probe_rate = false;
                    rate_probe = None;
                }
                Some(_) => {}
            }
        }
        if pres != PRES_NONE {
            let content = pres + shift;
            ctl.playhead_ns.store(content, Relaxed);
            let mut st = sh.status.lock().unwrap();
            if live {
                st.live_edge_ms = (edge.load(Relaxed) / 1_000_000) as u64;
            }
            let ms = (content.max(0) / 1_000_000) as u64;
            let mut changed = st.position_ms / 200 != ms / 200;
            st.position_ms = ms;
            if st.phase == Phase::Loading && !user_paused {
                st.phase = Phase::Playing;
                changed = true;
            }
            drop(st);
            if changed {
                wake();
            }
        }
        if let Some(h) = pipe.ev.hdr.lock().unwrap().take() {
            crate::log(&format!("feed: pipeline hdrType={h}"));
            sh.status.lock().unwrap().hdr = h;
            wake();
        }
        if pipe.ev.eos.load(Relaxed) && eos_pushed {
            set_phase(sh, Phase::Ended);
        }

        // ---- feed ----
        // Position estimate on the fed timeline: the reported one, else wall clock since Play.
        let est = if pres != PRES_NONE {
            Some(pres)
        } else {
            play_at.filter(|t| t.elapsed() > Duration::from_secs(2)).map(|t| base + (t.elapsed().as_nanos() as f64 * rate as f64) as i64)
        };
        let vlimit = if priming {
            base + BLIND_AHEAD_NS
        } else {
            match est {
                Some(p) => p + MAX_FEED_AHEAD_NS,
                None => base + BLIND_AHEAD_NS,
            }
        };
        let vstarved = priming || est.is_some_and(|p| vf.fed_max.map_or(true, |m| m - p < 250_000_000));
        let nv = match feed_lane(video, &mut vf, 1, shift, i64::MIN, vlimit, vstarved) {
            Ok(n) => n,
            Err(e) => break Err(e),
        };
        let na = if has_audio {
            let astarved = priming || est.is_some_and(|p| af.fed_max.map_or(true, |m| m - p < 250_000_000));
            match feed_lane(audio, &mut af, 2, shift, floor, vlimit + AUDIO_SLACK_NS, astarved) {
                Ok(n) => n,
                Err(e) => break Err(e),
            }
        } else {
            0
        };

        // ---- prime → Play ----
        let vbuf = vf.fed_max.map_or(0, |m| m - base);
        let abuf = af.fed_max.map_or(0, |m| m - base);
        let v_done = video.drained() && vf.pending.is_none();
        let a_done = !has_audio || (audio.drained() && af.pending.is_none());
        let audio_ready_or_given_up = !has_audio || af.fed_max.is_some() || prime_since.elapsed() > PRIME_AUDIO_WAIT;
        if priming && seeking.is_none() && (vbuf >= PRIME_NS && (!has_audio || abuf >= PRIME_AUDIO_NS) || (vbuf >= PRIME_VIDEO_MAX_NS && audio_ready_or_given_up) || (v_done && a_done && vf.fed_max.is_some())) {
            priming = false;
            crate::log(&format!("feed: primed v={} ms a={} ms", vbuf / 1_000_000, abuf / 1_000_000));
            if !user_paused {
                if sys::play() {
                    played(sh, pipe.epoch);
                } else {
                    crate::log("feed: Play refused");
                }
                play_at = Some(Instant::now());
                if trick {
                    first_frame_by = Some(Instant::now() + TRICK_FIRST_FRAME);
                }
            }
        }
        if !priming && !user_paused && play_at.is_none() {
            play_at = Some(Instant::now());
        }

        // ---- end of stream ----
        if !eos_pushed && !priming && v_done && a_done {
            crate::log("feed: all media fed; pushEOS");
            sys::push_eos();
            eos_pushed = true;
        }

        if nv + na == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    pipe.close(sh);
    result
}

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
/// ...or start anyway once this much video is buffered (audioless / starved audio).
pub const PRIME_VIDEO_MAX_NS: i64 = 2_500_000_000;
/// Cap on what is fed past the anchor while no position is known.
const BLIND_AHEAD_NS: i64 = 4_000_000_000;
/// Per-lane wall-clock budget per tick once the lane is healthy.
const LANE_SLICE: Duration = Duration::from_millis(3);
/// Firmware number for "decoder taken back by policy" (plx-native, webOS 4.10).
const DECODER_REVOKED: i64 = 600;

pub enum Cmd {
    Pause,
    Resume,
    Seek(u64),
    Volume(i32),
}

#[derive(Clone)]
pub enum Transport {
    Direct,
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
}

pub struct Shared {
    pub status: Arc<Mutex<Status>>,
    pub cmds: Mutex<VecDeque<Cmd>>,
    pub stop: AtomicBool,
    /// Decoded raster to place the exported window for (taken by the main thread).
    pub place: Mutex<Option<(u32, u32)>>,
}

impl Shared {
    pub fn new(status: Arc<Mutex<Status>>) -> Arc<Shared> {
        Arc::new(Shared { status, cmds: Mutex::new(VecDeque::new()), stop: AtomicBool::new(false), place: Mutex::new(None) })
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
    fn open(payload: &str) -> Result<Pipe, String> {
        let ev = Events::new();
        let epoch = sys::route(ev.clone());
        let t0 = Instant::now();
        if !sys::load(payload, epoch) {
            sys::teardown(Duration::from_secs(3));
            sys::unroute(epoch);
            return Err(if sys::blocked() { "Starfish is quarantined (restart the app)".into() } else { "Starfish refused the Load".into() });
        }
        crate::log(&format!("feed: Load returned after {} ms", t0.elapsed().as_millis()));
        while !(ev.load_completed.load(Relaxed) || sys::is_load_completed()) {
            if let Some((num, s)) = ev.error.lock().unwrap().clone() {
                let p = Pipe { epoch, ev: ev.clone() };
                p.close();
                return Err(format!("pipeline refused the stream (error {num}: {})", s.chars().take(120).collect::<String>()));
            }
            if t0.elapsed() > Duration::from_secs(10) {
                crate::log("feed: no loadCompleted after 10 s; feeding anyway");
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        crate::log(&format!("feed: loadCompleted after {} ms", t0.elapsed().as_millis()));
        Ok(Pipe { epoch, ev })
    }

    fn close(self) {
        let destroyed = sys::teardown(Duration::from_secs(3));
        sys::unroute(self.epoch);
        crate::log(&format!("feed: pipeline closed (destroyed={destroyed})"));
    }
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
    let start_ns = plan.start_ms as i64 * 1_000_000;
    ctl.target_ns.store(start_ns, Release);
    ctl.playhead_ns.store(start_ns, Relaxed);
    let (video, audio) = (Lane::new(), Lane::new());
    let has_audio = plan.audio_fmt.is_some();
    spawn_fetchers(&plan, &video, &audio, &ctl);
    let r = session(&sh, &plan, &ctl, &video, &audio, has_audio);
    ctl.stop.store(true, Relaxed);
    video.wake();
    audio.wake();
    if let Err(e) = r {
        fail(&sh, e);
    }
}

fn spawn_fetchers(plan: &Plan, video: &Arc<Lane>, audio: &Arc<Lane>, ctl: &Arc<SourceCtl>) {
    let spawn = |name: &str, f: Box<dyn FnOnce() + Send>| {
        if std::thread::Builder::new().name(name.into()).stack_size(512 << 10).spawn(f).is_err() {
            crate::log(&format!("feed: could not spawn {name}"));
        }
    };
    match &plan.transport {
        Transport::Direct => {
            let (f, ua, l, c) = (plan.video_fmt.clone(), plan.user_agent.clone(), video.clone(), ctl.clone());
            spawn("feed-v", Box::new(move || source::run_ranged(f, ua, l, c, Kind::Video)));
            if let Some(af) = plan.audio_fmt.clone() {
                let (ua, l, c) = (plan.user_agent.clone(), audio.clone(), ctl.clone());
                spawn("feed-a", Box::new(move || source::run_ranged(af, ua, l, c, Kind::Audio)));
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

fn session(sh: &Arc<Shared>, plan: &Plan, ctl: &SourceCtl, video: &Lane, audio: &Lane, has_audio: bool) -> Result<(), String> {
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
    };
    let payload = payload::build(&spec);
    let env = payload::sink_envelope(vdecl, vt.width, vt.height, fps);
    crate::log(&format!(
        "feed: Load {} {}x{}@{fps:.3} (declared {}x{}@{}) audio={:?} hdr={:?}",
        vdecl.name(),
        vt.width,
        vt.height,
        env.w,
        env.h,
        env.fps,
        at.as_ref().map(|t| &t.codec),
        vt.color
    ));
    {
        let mut s = sh.status.lock().unwrap();
        s.width = vt.width;
        s.height = vt.height;
        s.fps = fps.round() as u32;
    }
    *sh.place.lock().unwrap() = Some((vt.width, vt.height));

    let mut shift = video.peek_pts().unwrap_or(0).max(0);
    let mut pipe = Pipe::open(&payload)?;
    let mut base = 0i64; // fed-timeline anchor of the current Load/seek
    let mut floor = shift; // content-time: no audio before the video anchor
    let mut vf = LaneFeed::default();
    let mut af = LaneFeed::default();
    let mut priming = true;
    let mut user_paused = false;
    let mut eos_pushed = false;
    let mut seeking: Option<i64> = None;
    let mut gen = 0u64;
    let mut native_seek = true;
    let mut play_at: Option<Instant> = None;
    let duration_ns = plan.duration_ms as i64 * 1_000_000;

    let result = loop {
        if sh.stop.load(Relaxed) {
            break Ok(());
        }
        // ---- commands ----
        let cmds: Vec<Cmd> = sh.cmds.lock().unwrap().drain(..).collect();
        let mut seek_to = None;
        for c in cmds {
            match c {
                Cmd::Pause => {
                    user_paused = true;
                    if !priming && seeking.is_none() {
                        sys::pause();
                    }
                    set_phase(sh, Phase::Paused);
                }
                Cmd::Resume => {
                    user_paused = false;
                    if !priming && seeking.is_none() {
                        sys::play();
                    }
                    set_phase(sh, Phase::Playing);
                }
                Cmd::Seek(ms) => seek_to = Some(ms), // coalesce: the last one wins
                Cmd::Volume(v) => {
                    sys::set_volume(v);
                }
            }
        }
        if let Some(ms) = seek_to {
            let t = (ms as i64 * 1_000_000).clamp(0, (duration_ns - 1_000_000_000).max(0));
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
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            let landed = native_seek && sys::seek_ms(k.max(0) / 1_000_000);
            if landed {
                shift = 0;
                base = k.max(0);
            } else {
                if native_seek {
                    crate::log("feed: native Seek refused; reloading per seek from now on");
                    native_seek = false;
                }
                pipe.close();
                pipe = Pipe::open(&payload)?;
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
            let content = pres + shift;
            ctl.playhead_ns.store(content, Relaxed);
            let mut st = sh.status.lock().unwrap();
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
        if pipe.ev.eos.load(Relaxed) && eos_pushed {
            set_phase(sh, Phase::Ended);
        }

        // ---- feed ----
        // Position estimate on the fed timeline: the reported one, else wall clock since Play.
        let est = if pres != PRES_NONE {
            Some(pres)
        } else {
            play_at.filter(|t| t.elapsed() > Duration::from_secs(2)).map(|t| base + t.elapsed().as_nanos() as i64)
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
        if priming && seeking.is_none() && (vbuf >= PRIME_NS && (!has_audio || abuf >= PRIME_AUDIO_NS) || vbuf >= PRIME_VIDEO_MAX_NS || (v_done && a_done && vf.fed_max.is_some())) {
            priming = false;
            crate::log(&format!("feed: primed v={} ms a={} ms", vbuf / 1_000_000, abuf / 1_000_000));
            if !user_paused {
                if !sys::play() {
                    crate::log("feed: Play refused");
                }
                play_at = Some(Instant::now());
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
    pipe.close();
    result
}

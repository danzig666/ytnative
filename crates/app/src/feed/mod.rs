//! Playback engine B ("Feed"): the app demuxes YouTube's DASH formats itself
//! (`yt_core::demux`) and feeds elementary streams to the TV's in-process `StarfishMediaAPIs`
//! (libplayerAPIs.so.1, BUFFERSTREAM), with the SDL exported window as the video plane (webOS 5+)
//! or the ACB plane bound by [`crate::vplane`] (webOS 4).
//! Unlike engine A (DASH MPD → uMediaServer → firmware dashdemux2, H.264 ≤ 1080p in practice)
//! this can play VP9 and AV1, i.e. YouTube's 1440p/2160p formats, and Opus.
//!
//! Design and the TV validation checklist: `docs/feed-engine.md`. The Starfish shim and its
//! lifecycle rules (callback gate, quarantine) are plx-native's (`csrc/starfish.c`).
//!
//! API mirrors engine A's [`crate::player::Player`] (same [`Status`]), so the app can pick an
//! engine per video:
//!
//! ```ignore
//! let mut b = feed::FeedPlayer::new();
//! if feed::FeedPlayer::available() {
//!     b.start(video_id, resolved, feed::Prefs::new(2160, feed::VideoCodec::Vp9), None, None)?;
//! }
//! // every frame (main thread): b.tick();  status: b.status.lock()
//! ```

// Not wired into the app shell yet (the host picks engine A today).
#![allow(dead_code)]

pub mod engine;
pub mod payload;
pub mod source;
pub mod sys;
pub mod window;

pub use engine::Transport;
pub use yt_core::sabr::VideoCodec;

use crate::player::{Phase, Status};
use engine::{Cmd, Plan, Shared};
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use yt_core::demux::select;

/// webOS 4: frame ticks (FRAMEREADY, ~5 Hz) before setMediaVideoData (plx `acb_bind_frames_needed`).
const ACB_FRAMES: u64 = 2;
/// webOS 4: a refused plane bind is retried this long, then given up (audio without a picture).
const ACB_REFUSED_GIVE_UP: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub struct Prefs {
    pub max_height: u32,
    pub codec: VideoCodec,
    /// Play HDR (PQ/HLG) formats when they are the best match (TV-side HDR switching is
    /// unverified for VP9.2 / AV1 on webOS 5+: see docs/feed-engine.md).
    pub allow_hdr: bool,
    pub transport: Transport,
}

impl Prefs {
    pub fn new(max_height: u32, codec: VideoCodec) -> Prefs {
        Prefs { max_height, codec, allow_hdr: false, transport: Transport::Direct }
    }
}

pub struct FeedPlayer {
    pub status: Arc<Mutex<Status>>,
    shared: Option<Arc<Shared>>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The exported window, when this player created it (destroyed after the unload).
    window: Option<window::Window>,
    /// webOS 4: the plane binding of this session's current pipeline (main thread).
    acb: Option<FeedAcb>,
    /// webOS 4: a refused bind (epoch, first refusal, given up); logged once, given up after a
    /// few seconds.
    acb_refused: Option<(u32, Instant, bool)>,
}

/// webOS 4: the ACB plane is bound to pipeline `epoch`.
struct FeedAcb {
    epoch: u32,
    /// setMediaVideoData + window + PLAYING sent (plx's `Stage::Streaming`); the pause mirror runs from here.
    streaming: bool,
    /// setMediaVideoData was refused client-side (the envelope is captured once: not retried).
    refused: bool,
    paused: bool,
}

impl Default for FeedPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl FeedPlayer {
    pub fn new() -> Self {
        FeedPlayer { status: Default::default(), shared: None, thread: None, window: None, acb: None, acb_refused: None }
    }

    /// libplayerAPIs is present, every needed symbol resolved and no object was quarantined.
    /// When false, use engine A.
    pub fn available() -> bool {
        sys::available()
    }

    /// The formats engine B would play for these prefs (None: nothing playable).
    pub fn choose(resolved: &yt_core::streams::Resolved, prefs: &Prefs) -> Option<select::Choice> {
        select::choose(
            &resolved.adaptive,
            select::Request {
                max_height: prefs.max_height,
                codec: prefs.codec,
                // webOS 4: a VP9 PQ stream stays SDR without plx's sourceInfo hdrType rewrite
                // before setMediaVideoData (not ported; docs/feed-engine.md).
                allow_hdr: prefs.allow_hdr && !crate::vplane::feed(),
                allow_av1: crate::device::av1_decoder(),
                need_url: matches!(prefs.transport, Transport::Direct),
            },
        )
    }

    /// A session (and possibly its window) exists.
    pub fn active(&self) -> bool {
        self.shared.is_some()
    }

    /// Main thread. Plays an already resolved video. `window_id`: an exported window the caller
    /// owns (must outlive [`stop`](Self::stop)), or `None` to create one here (webOS 5+) or to
    /// bind the ACB plane once the pipeline plays (webOS 4).
    pub fn start(
        &mut self,
        video_id: String,
        resolved: yt_core::streams::Resolved,
        prefs: Prefs,
        window_id: Option<String>,
        start_ms: Option<u64>,
        refresh: Option<source::Refresh>,
    ) -> anyhow::Result<()> {
        self.stop();
        let choice = Self::choose(&resolved, &prefs).ok_or_else(|| anyhow::anyhow!("no format engine B can play"))?;
        let duration_ms = choice.video["approxDurationMs"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(resolved.length_secs * 1000);
        *self.status.lock().unwrap() = Status {
            phase: Phase::Loading,
            rate: 1.0,
            video_id: video_id.clone(),
            channel_id: resolved.channel_id.clone(),
            storyboard: resolved.storyboard_spec.clone(),
            captions: resolved.captions.clone(),
            title: resolved.title.clone(),
            author: resolved.author.clone(),
            duration_ms,
            position_ms: start_ms.unwrap_or(0),
            ..Default::default()
        };
        let window_id = match window_id {
            Some(w) => Some(w),
            // webOS 4: no windowId in the Load; acb_tick binds the plane.
            None if crate::vplane::feed() => None,
            None => {
                let Some(w) = window::Window::create() else {
                    self.fail("could not create the video window");
                    anyhow::bail!("could not create the video window");
                };
                let n = |k: &str| choice.video[k].as_u64().unwrap_or(0) as u32;
                w.place(n("width"), n("height"));
                let id = w.id();
                self.window = Some(w);
                Some(id)
            }
        };
        crate::log(&format!(
            "feed: {video_id} video itag {} ({:?}) audio itag {:?}",
            choice.video_itag(),
            choice.video_codec,
            choice.audio_itag()
        ));
        let user_agent = resolved.client.user_agent().to_string();
        let plan = Plan {
            video_fmt: choice.video.clone(),
            audio_fmt: choice.audio.clone(),
            transport: if resolved.is_live { Transport::Live } else { prefs.transport },
            resolved,
            user_agent,
            app_id: plx_base::paths::app_id().to_string(),
            window_id,
            start_ms: start_ms.filter(|ms| *ms > 2000).unwrap_or(0),
            duration_ms,
            refresh,
        };
        let shared = Shared::new(self.status.clone());
        let sh = shared.clone();
        let t = std::thread::Builder::new().name("feed".into()).spawn(move || {
            engine::run(sh, plan);
            plx_machine::present::wake_from_worker();
        })?;
        self.shared = Some(shared);
        self.thread = Some(t);
        Ok(())
    }

    fn fail(&mut self, why: &str) {
        crate::log(&format!("feed: {why}"));
        self.status.lock().unwrap().phase = Phase::Failed(why.to_string());
    }

    fn send(&self, c: Cmd) {
        if let Some(sh) = &self.shared {
            sh.cmds.lock().unwrap().push_back(c);
        }
    }

    /// Main thread, every frame: places the exported window for the decoded raster (webOS 5+),
    /// or drives the ACB plane binding (webOS 4).
    pub fn tick(&mut self) {
        let Some(sh) = self.shared.clone() else { return };
        if let Some((w, h)) = sh.place.lock().unwrap().take() {
            if let Some(win) = &self.window {
                let ok = win.place(w, h);
                crate::log(&format!("feed: window placed for {w}x{h} -> {ok}"));
            }
        }
        if crate::vplane::feed() {
            self.serve_release(&sh);
            self.acb_tick(&sh);
        }
    }

    /// webOS 4: plx-native's Feed order (`feed_pump.rs`): once Play was issued and the pipeline
    /// named its media id, bind (setSinkType MAIN + setMediaId + LOADED); after two frame ticks
    /// send its sourceInfo verbatim, then the full-screen window + PLAYING; from then on mirror
    /// pause / resume. A new pipeline (a reload per seek) is a new epoch and binds again.
    fn acb_tick(&mut self, sh: &Shared) {
        // Held across the bind: the feed thread withdraws `pipe` under it before its teardown.
        let mut link = sh.plane.lock().unwrap();
        let Some((epoch, ev)) = link.pipe.clone() else { return };
        if link.played != epoch {
            return;
        }
        if self.acb.as_ref().map_or(true, |a| a.epoch != epoch) {
            if let Some((e, t, given_up)) = &mut self.acb_refused {
                if *e == epoch && t.elapsed() > ACB_REFUSED_GIVE_UP {
                    if !*given_up {
                        *given_up = true;
                        crate::log("feed: acb bind still refused; giving up: no picture for this pipeline");
                    }
                    return;
                }
            }
            let Some(id) = ev.media_id.lock().unwrap().clone() else { return };
            match crate::vplane::bind_feed(&id) {
                plx_platform::tv::plane::BindVerdict::Granted => {
                    link.bound = epoch;
                    self.acb = Some(FeedAcb { epoch, streaming: false, refused: false, paused: false });
                    self.acb_refused = None;
                    crate::log(&format!("feed: acb bound mediaId={id}"));
                }
                // The previous release is in flight; the plane owner logs the wait once.
                plx_platform::tv::plane::BindVerdict::Deferred => {}
                plx_platform::tv::plane::BindVerdict::Refused(why) => {
                    if self.acb_refused.map_or(true, |(e, _, _)| e != epoch) {
                        crate::log(&format!("feed: acb bind refused ({why:?}); retrying for {} s", ACB_REFUSED_GIVE_UP.as_secs()));
                        self.acb_refused = Some((epoch, Instant::now(), false));
                    }
                }
            }
            return;
        }
        drop(link);
        let a = self.acb.as_mut().unwrap();
        let frames = ev.frames.load(Relaxed);
        if !a.streaming && !a.refused && frames >= ACB_FRAMES {
            let Some(si) = ev.source_info.lock().unwrap().clone() else { return };
            let rv = crate::vplane::video_data(&si);
            crate::log(&format!("feed: acb setMediaVideoData rv={rv} frames={frames}"));
            if rv == -1 {
                // -1: the client library refused the JSON itself; nothing left the process.
                a.refused = true;
                crate::log("feed: acb sourceInfo refused; no picture for this pipeline");
                return;
            }
            crate::vplane::start_full();
            a.streaming = true;
            crate::log("feed: acb window+PLAYING sent");
        }
        // Mirrored only from Streaming, and at once: ACB never stays PLAYING over a paused pipeline.
        if a.streaming {
            let paused = self.status.lock().unwrap().phase == Phase::Paused;
            if paused != a.paused {
                a.paused = paused;
                if paused {
                    crate::vplane::pause();
                } else {
                    crate::vplane::resume();
                }
            }
        }
    }

    /// webOS 4: the feed thread's teardown asks for the plane's UNLOADED between the callback
    /// gate's retirement and D1 (`engine::release_plane`); answered here, on the main thread.
    fn serve_release(&mut self, sh: &Shared) {
        let req = sh.release_req.load(Acquire);
        if req == 0 || sh.release_ack.load(Relaxed) == req {
            return;
        }
        if self.acb.as_ref().is_some_and(|a| a.epoch == req) {
            self.acb = None;
            let phase = crate::vplane::unload();
            crate::log(&format!("feed: acb unload -> {phase:?}"));
        }
        {
            let mut link = sh.plane.lock().unwrap();
            if link.bound == req {
                link.bound = 0;
            }
        }
        sh.release_ack.store(req, Release);
    }

    pub fn toggle_pause(&mut self) {
        let paused = self.status.lock().unwrap().phase == Phase::Paused;
        self.send(if paused { Cmd::Resume } else { Cmd::Pause });
    }

    pub fn pause(&mut self) {
        self.send(Cmd::Pause);
    }

    pub fn resume(&mut self) {
        self.send(Cmd::Resume);
    }

    pub fn seek_to(&mut self, to_ms: u64) {
        self.status.lock().unwrap().position_ms = to_ms; // shown at once; events correct it
        self.send(Cmd::Seek(to_ms));
    }

    pub fn seek_by(&mut self, delta_ms: i64) {
        let (pos, end) = {
            let s = self.status.lock().unwrap();
            // Live: anywhere in the DVR window up to just behind the newest segment.
            let end = if s.live { s.live_edge_ms as i64 - source::LIVE_DELAY_NS / 1_000_000 } else { s.duration_ms as i64 - 1000 };
            (s.position_ms as i64, end)
        };
        let to = (pos + delta_ms).clamp(0, end.max(0));
        self.seek_to(to as u64);
    }

    /// Playback speed, through StarfishMediaAPIs::SetPlayRate (re-applied by the engine after
    /// every reload).
    pub fn set_rate(&mut self, rate: f32) -> bool {
        if !self.active() {
            return false;
        }
        let rate = rate.clamp(0.25, 4.0);
        self.status.lock().unwrap().rate = rate;
        self.send(Cmd::Rate(rate));
        true
    }

    /// This pipeline's volume, 0..100.
    pub fn set_volume(&mut self, volume: i32) {
        self.send(Cmd::Volume(volume.clamp(0, 100)));
    }

    /// Main thread. Stops the session: the feed thread unloads the pipeline (and destructs it
    /// only with the shim's proofs, else quarantines it); only then is the window destroyed.
    pub fn stop(&mut self) {
        let sh = self.shared.take();
        if let Some(sh) = &sh {
            sh.stop.store(true, Relaxed);
        }
        if let Some(t) = self.thread.take() {
            // webOS 4: the teardown waits on this thread for the plane's UNLOADED; a blind join
            // would hold it to its timeout.
            if let Some(sh) = sh.as_ref().filter(|_| crate::vplane::feed()) {
                while !t.is_finished() {
                    self.serve_release(sh);
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            let _ = t.join();
        }
        if self.acb.take().is_some() {
            // The handshake did not run (it timed out, or the thread ended without a teardown):
            // UNLOADED after the pipeline, but the plane is still freed.
            let phase = crate::vplane::unload();
            crate::log(&format!("feed: acb unload (after the pipeline) -> {phase:?}"));
        }
        self.acb_refused = None;
        if let Some(w) = self.window.take() {
            w.destroy();
        }
        let mut s = self.status.lock().unwrap();
        if !matches!(s.phase, Phase::Failed(_)) {
            s.phase = Phase::Idle;
        }
    }
}

impl Drop for FeedPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

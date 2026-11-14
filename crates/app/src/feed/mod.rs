//! Playback engine B ("Feed"): the app demuxes YouTube's DASH formats itself
//! (`yt_core::demux`) and feeds elementary streams to the TV's in-process `StarfishMediaAPIs`
//! (libplayerAPIs.so.1, BUFFERSTREAM), with the SDL exported window as the video plane.
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
use std::sync::{Arc, Mutex};
use yt_core::demux::select;

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
}

impl Default for FeedPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl FeedPlayer {
    pub fn new() -> Self {
        FeedPlayer { status: Default::default(), shared: None, thread: None, window: None }
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
                allow_hdr: prefs.allow_hdr,
                need_url: matches!(prefs.transport, Transport::Direct),
            },
        )
    }

    /// A session (and possibly its window) exists.
    pub fn active(&self) -> bool {
        self.shared.is_some()
    }

    /// Main thread. Plays an already resolved video. `window_id`: an exported window the caller
    /// owns (must outlive [`stop`](Self::stop)), or `None` to create one here.
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
            Some(w) => w,
            None => {
                let Some(w) = window::Window::create() else {
                    self.fail("could not create the video window");
                    anyhow::bail!("could not create the video window");
                };
                let n = |k: &str| choice.video[k].as_u64().unwrap_or(0) as u32;
                w.place(n("width"), n("height"));
                let id = w.id();
                self.window = Some(w);
                id
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
            window_id: Some(window_id),
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

    /// Main thread, every frame: places the exported window for the decoded raster.
    pub fn tick(&mut self) {
        let Some(sh) = &self.shared else { return };
        if let Some((w, h)) = sh.place.lock().unwrap().take() {
            if let Some(win) = &self.window {
                let ok = win.place(w, h);
                crate::log(&format!("feed: window placed for {w}x{h} -> {ok}"));
            }
        }
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
        if let Some(sh) = self.shared.take() {
            sh.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
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

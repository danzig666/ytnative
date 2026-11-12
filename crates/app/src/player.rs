//! Playback engine A: a DASH manifest handed to the TV's media service (uMediaServer) with an SDL
//! exported window as the video plane. Proven on the C3 by the step-2 prototype (`tvplay`).
//!
//! Threading: the exported window is created/destroyed on the main thread (SDL); stream
//! resolution and every luna call run on a worker; media events update `Status`, which the
//! player screen reads every frame.
//!
//! The Windows simulator has no media pipeline: its `Player` (`sim/player.rs`) resolves the video
//! for real (title, duration, captions, extras) and then runs a fake clock.

#[cfg(windows)]
pub use crate::sim::player::Player;
#[cfg(not(windows))]
use crate::ls2::Luna;
#[cfg(not(windows))]
use std::ffi::{c_char, c_int, CStr, CString};
use std::sync::{Arc, Mutex};

#[cfg(not(windows))]
#[repr(C)]
struct SdlRect {
    x: c_int,
    y: c_int,
    w: c_int,
    h: c_int,
}

#[cfg(not(windows))]
type CreateFn = unsafe extern "C" fn(c_int) -> *const c_char;
#[cfg(not(windows))]
type SetFn = unsafe extern "C" fn(*const c_char, *mut SdlRect, *mut SdlRect) -> c_int;
#[cfg(not(windows))]
type DestroyFn = unsafe extern "C" fn(*const c_char);

#[cfg(not(windows))]
fn sdl_sym<T>(name: &str) -> Option<T> {
    let c = CString::new(name).ok()?;
    let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c.as_ptr()) };
    (!p.is_null()).then(|| unsafe { std::mem::transmute_copy(&p) })
}

/// How the picture fills the screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Zoom {
    /// Whole picture, letterboxed as needed.
    #[default]
    Fit,
    /// Crop to fill the screen (no bars).
    Fill,
    /// 125 % centre crop.
    Zoom,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Phase {
    #[default]
    Idle,
    Resolving,
    Loading,
    Playing,
    Paused,
    Ended,
    Failed(String),
}

/// What the player shows around the video, fetched after playback starts.
#[derive(Debug, Default, Clone)]
pub struct Extras {
    pub watch: Option<yt_core::watch::WatchInfo>,
    pub segments: Vec<yt_core::ext::sponsorblock::Segment>,
    pub votes: Option<yt_core::ext::ryd::Votes>,
    pub cues: Vec<yt_core::captions::Cue>,
    pub caption_label: Option<String>,
    pub levels: Vec<yt_core::storyboard::Level>,
}

/// Settings the extras fetch depends on (a snapshot, taken on the main thread).
#[derive(Clone, Debug)]
pub struct ExtrasPrefs {
    pub subtitles: bool,
    pub language: String,
    pub auto_translate: bool,
    pub sponsorblock: bool,
    pub categories: Vec<yt_core::ext::sponsorblock::Category>,
    pub dislikes: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub phase: Phase,
    pub extras: Arc<Extras>,
    pub video_id: String,
    pub channel_id: String,
    pub storyboard: Option<String>,
    pub captions: Vec<yt_core::streams::CaptionTrack>,
    /// (id, name, default) of the audio languages offered.
    pub audio_tracks: Vec<(String, String, bool)>,
    pub audio_track: Option<String>,
    pub zoom: Zoom,
    pub rate: f32,
    /// A live stream (engine B): `position_ms` is stream time, `live_edge_ms` the newest media.
    pub live: bool,
    pub live_edge_ms: u64,
    pub title: String,
    pub author: String,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    /// Best height the stream offers within the quality setting, shown until the decoder reports.
    pub quality_hint: u32,
    pub fps: u32,
}

#[cfg(not(windows))]
pub struct Player {
    pub status: Arc<Mutex<Status>>,
    window: Option<CString>,
    media_id: Arc<Mutex<Option<String>>>,
    luna: Option<&'static Luna>,
    /// Engine B settings; `Some` lets a video go to the Feed engine when it needs it (4K, VP9, AV1)
    /// and live streams (always).
    pub feed_prefs: Option<crate::feed::Prefs>,
    /// The "Playback engine" setting: VOD goes to engine B only when this is on.
    pub prefer_feed: bool,
    feed: crate::feed::FeedPlayer,
    /// A resolved video the worker hands to engine B (started on the main thread in `tick`).
    handoff: Arc<Mutex<Option<Handoff>>>,
}

#[cfg(not(windows))]
struct Handoff {
    video_id: String,
    resolved: yt_core::streams::Resolved,
    start_ms: Option<u64>,
}

/// Engine B takes live streams (LG's HLS player stalls on YouTube's split-audio live playlists)
/// and, when preferred, videos whose best format is above 1080p or not H.264.
#[cfg(not(windows))]
fn wants_feed(resolved: &yt_core::streams::Resolved, prefs: &crate::feed::Prefs, prefer: bool) -> bool {
    if !crate::feed::FeedPlayer::available() {
        return false;
    }
    match crate::feed::FeedPlayer::choose(resolved, prefs) {
        Some(_) if resolved.is_live => true,
        Some(c) => prefer && (c.video_codec != crate::feed::VideoCodec::H264 || c.video["height"].as_u64().unwrap_or(0) > 1080),
        None => false,
    }
}

#[cfg(not(windows))]
impl Player {
    pub fn new() -> Self {
        Player {
            status: Default::default(),
            window: None,
            media_id: Default::default(),
            luna: None,
            feed_prefs: None,
            prefer_feed: false,
            feed: crate::feed::FeedPlayer::new(),
            handoff: Default::default(),
        }
    }

    /// A playback session (and its video window) exists.
    pub fn active(&self) -> bool {
        self.window.is_some()
    }

    fn bus(&mut self) -> Option<&'static Luna> {
        if self.luna.is_none() {
            match Luna::shared(plx_base::paths::app_id()) {
                Ok(l) => self.luna = Some(l),
                Err(e) => crate::log(&format!("player: LS2 unavailable: {e}")),
            }
        }
        self.luna
    }

    /// Main thread. Starts playback of `video_id`; `session` resolves streams on the worker.
    pub fn start(
        &mut self,
        video_id: String,
        session: Arc<Mutex<Option<yt_core::session::Session>>>,
        prefs: yt_core::streams::Prefs,
        extras: ExtrasPrefs,
        start_ms: Option<u64>,
    ) {
        self.stop();
        *self.status.lock().unwrap() = Status { phase: Phase::Resolving, rate: 1.0, ..Default::default() };
        let Some(create) = sdl_sym::<CreateFn>("SDL_webOSCreateExportedWindow") else {
            self.fail("no SDL exported-window support on this TV");
            return;
        };
        let wid = unsafe { create(0) };
        if wid.is_null() {
            self.fail("could not create the video window");
            return;
        }
        let wid = unsafe { CStr::from_ptr(wid) }.to_owned();
        if let Some(set) = sdl_sym::<SetFn>("SDL_webOSSetExportedWindow") {
            let mut src = SdlRect { x: 0, y: 0, w: 1920, h: 1080 };
            let mut dst = SdlRect { x: 0, y: 0, w: 1920, h: 1080 };
            unsafe { set(wid.as_ptr(), &mut src, &mut dst) };
        }
        let window_id = wid.to_string_lossy().into_owned();
        self.window = Some(wid);
        let Some(luna) = self.bus() else {
            self.fail("media bus unavailable");
            return;
        };
        let (status, media_id) = (self.status.clone(), self.media_id.clone());
        let feed = self.feed_prefs.clone().map(|p| (p, self.prefer_feed, self.handoff.clone()));
        let spawned = plx_base::task::spawn("play", move || {
            match run(luna, &video_id, &window_id, &session, &status, &media_id, prefs, start_ms, feed) {
                Ok(()) => {
                    let (v, s2, st2) = (video_id.clone(), session.clone(), status.clone());
                    let _ = plx_base::task::spawn("track", move || track_history(&v, &s2, &st2));
                    load_extras(&video_id, &session, &status, &extras)
                }
                Err(e) => {
                    crate::log(&format!("player: {e:#}"));
                    status.lock().unwrap().phase = Phase::Failed(format!("{e:#}"));
                }
            }
            plx_machine::present::wake_from_worker();
        });
        if spawned.is_none() {
            self.fail("could not start the player thread");
        }
    }

    /// Main thread, every frame: starts a handed-off engine B session, drives engine B.
    pub fn tick(&mut self) {
        let h = self.handoff.lock().unwrap().take();
        if let Some(h) = h {
            let prefs = self.feed_prefs.clone().unwrap_or_else(|| crate::feed::Prefs::new(2160, crate::feed::VideoCodec::Vp9));
            self.feed.status = self.status.clone();
            let wid = self.window.as_ref().map(|w| w.to_string_lossy().into_owned());
            if let Err(e) = self.feed.start(h.video_id, h.resolved, prefs, wid, h.start_ms) {
                self.fail(&format!("engine B: {e:#}"));
            }
        }
        if self.feed.active() {
            self.feed.tick();
        }
    }

    fn fail(&mut self, why: &str) {
        crate::log(&format!("player: {why}"));
        self.status.lock().unwrap().phase = Phase::Failed(why.to_string());
    }

    pub fn toggle_pause(&mut self) {
        if self.feed.active() {
            return self.feed.toggle_pause();
        }
        let Some(luna) = self.luna else { return };
        let Some(mid) = self.media_id.lock().unwrap().clone() else { return };
        let paused = self.status.lock().unwrap().phase == Phase::Paused;
        let method = if paused { "play" } else { "pause" };
        let payload = format!(r#"{{"mediaId":{}}}"#, json_str(&mid));
        let _ = plx_base::task::spawn("media", move || {
            luna.call(&format!("luna://com.webos.media/{method}"), &payload);
        });
    }

    pub fn seek_to(&mut self, to_ms: u64) {
        if self.feed.active() {
            return self.feed.seek_to(to_ms);
        }
        let pos = self.status.lock().unwrap().position_ms as i64;
        self.seek_by(to_ms as i64 - pos);
    }

    /// Picture fit: re-places the exported window with a cropped source rectangle.
    pub fn set_zoom(&mut self, zoom: Zoom) {
        let (w, h) = {
            let mut s = self.status.lock().unwrap();
            s.zoom = zoom;
            (s.width.max(1) as f32, s.height.max(1) as f32)
        };
        let Some(win) = &self.window else { return };
        let Some(set) = sdl_sym::<SetFn>("SDL_webOSSetExportedWindow") else { return };
        let (sw, sh) = match zoom {
            Zoom::Fit => (w, h),
            Zoom::Fill => {
                // Crop the source to the screen's 16:9.
                let target = 16.0 / 9.0;
                if w / h > target { (h * target, h) } else { (w, w / target) }
            }
            Zoom::Zoom => (w / 1.25, h / 1.25),
        };
        let mut src = SdlRect { x: ((w - sw) / 2.0) as c_int, y: ((h - sh) / 2.0) as c_int, w: sw as c_int, h: sh as c_int };
        let mut dst = SdlRect { x: 0, y: 0, w: 1920, h: 1080 };
        let r = unsafe { set(win.as_ptr(), &mut src, &mut dst) };
        crate::log(&format!("player: zoom {zoom:?} src {}x{} -> {r}", src.w, src.h));
    }

    /// Playback speed (0.25..2.0) through uMediaServer's setPlayRate.
    pub fn set_rate(&mut self, rate: f32) {
        if self.feed.active() {
            // Starfish has no play-rate call in this mode.
            self.feed.set_rate(rate);
            return;
        }
        let Some(luna) = self.luna else { return };
        let Some(mid) = self.media_id.lock().unwrap().clone() else { return };
        self.status.lock().unwrap().rate = rate;
        let payload = format!(r#"{{"mediaId":{},"playRate":{rate},"audioOutput":true}}"#, json_str(&mid));
        let _ = plx_base::task::spawn("media", move || {
            let r = luna.call("luna://com.webos.media/setPlayRate", &payload);
            crate::log(&format!("player: setPlayRate -> {r}"));
        });
    }

    pub fn seek_by(&mut self, delta_ms: i64) {
        if self.feed.active() {
            return self.feed.seek_by(delta_ms);
        }
        let Some(luna) = self.luna else { return };
        let Some(mid) = self.media_id.lock().unwrap().clone() else { return };
        let (pos, dur) = {
            let mut s = self.status.lock().unwrap();
            let to = (s.position_ms as i64 + delta_ms).clamp(0, (s.duration_ms as i64 - 1000).max(0));
            let before = s.position_ms as i64;
            s.position_ms = to as u64; // show the target immediately; events correct it
            (before, s.duration_ms as i64)
        };
        let to = (pos + delta_ms).clamp(0, (dur - 1000).max(0));
        let payload = format!(r#"{{"mediaId":{},"position":{to}}}"#, json_str(&mid));
        let _ = plx_base::task::spawn("media", move || {
            luna.call("luna://com.webos.media/seek", &payload);
        });
    }

    /// Main thread. Unloads the pipeline (if any) and destroys the video window.
    pub fn stop(&mut self) {
        self.handoff.lock().unwrap().take();
        if self.feed.active() {
            // Engine B unloads first: the window it renders into is ours.
            self.feed.stop();
        }
        let mid = self.media_id.lock().unwrap().take();
        if let (Some(luna), Some(mid)) = (self.luna, mid) {
            // Synchronous on purpose: the window must outlive the pipeline that renders into it.
            let r = luna.call("luna://com.webos.media/unload", &format!(r#"{{"mediaId":{}}}"#, json_str(&mid)));
            crate::log(&format!("player: unload -> {r}"));
        }
        if let Some(w) = self.window.take() {
            if let Some(destroy) = sdl_sym::<DestroyFn>("SDL_webOSDestroyExportedWindow") {
                unsafe { destroy(w.as_ptr()) };
            }
        }
        self.status.lock().unwrap().phase = Phase::Idle;
    }
}

#[cfg(not(windows))]
fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// Worker: resolve, write the manifest, load, subscribe, foreground, play.
#[cfg(not(windows))]
fn run(
    luna: &'static Luna,
    video_id: &str,
    window_id: &str,
    session: &Mutex<Option<yt_core::session::Session>>,
    status: &Arc<Mutex<Status>>,
    media_id: &Arc<Mutex<Option<String>>>,
    prefs: yt_core::streams::Prefs,
    start_ms: Option<u64>,
    feed: Option<(crate::feed::Prefs, bool, Arc<Mutex<Option<Handoff>>>)>,
) -> anyhow::Result<()> {
    let resolved = {
        let mut g = session.lock().unwrap();
        if g.is_none() {
            *g = Some(yt_core::session::Session::new()?);
        }
        g.as_mut().unwrap().resolve(video_id)?
    };
    {
        let mut s = status.lock().unwrap();
        s.title = resolved.title.clone();
        s.author = resolved.author.clone();
        s.duration_ms = resolved.length_secs * 1000;
        s.phase = Phase::Loading;
    }
    {
        let mut s = status.lock().unwrap();
        s.video_id = video_id.to_string();
        s.channel_id = resolved.channel_id.clone();
        s.storyboard = resolved.storyboard_spec.clone();
        s.captions = resolved.captions.clone();
        s.audio_tracks = resolved.audio_tracks();
        s.audio_track = prefs.audio_track.clone();
        s.quality_hint = resolved
            .adaptive
            .iter()
            .filter(|f| f["mimeType"].as_str().is_some_and(|m| m.starts_with("video/")))
            .filter_map(|f| f["height"].as_u64())
            .filter(|h| *h <= prefs.max_height)
            .max()
            .unwrap_or(0) as u32;
    }
    if let Some((fp, prefer, slot)) = feed {
        if wants_feed(&resolved, &fp, prefer) {
            crate::log("player: handing the video to engine B (Feed)");
            *slot.lock().unwrap() = Some(Handoff { video_id: video_id.to_string(), resolved, start_ms });
            plx_machine::present::wake_from_worker();
            // Let the main thread start it before the extras land in the shared status.
            for _ in 0..60 {
                if slot.lock().unwrap().is_none() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            return Ok(());
        }
    }
    std::fs::create_dir_all("/tmp/ytnative")?;
    // Live: the HLS master reduced to H.264 renditions (DASH-from-formats only works for VOD).
    // Needs TV validation: LG's hlsdemux2 stalled on YouTube's VOD TS playlists in early tests.
    let path = if resolved.is_live || resolved.adaptive.iter().all(|f| f["initRange"].is_null()) {
        let http = yt_core::net::builder().timeout(std::time::Duration::from_secs(15)).build()?;
        let master = resolved.hls_master(&http, prefs.max_height)?;
        let path = "/tmp/ytnative/live.m3u8";
        std::fs::write(path, master)?;
        crate::log("player: live stream via HLS");
        path
    } else {
        let mpd = resolved.mpd(prefs).ok_or_else(|| anyhow::anyhow!("no H.264 streams for this video"))?;
        let path = "/tmp/ytnative/play.mpd";
        std::fs::write(path, mpd)?;
        path
    };

    let app_id = plx_base::paths::app_id();
    let load = format!(
        r#"{{"uri":"file://{path}","type":"media","payload":{{"option":{{"appId":{},"windowId":{}}}}}}}"#,
        json_str(app_id),
        json_str(window_id)
    );
    let r = luna.call("luna://com.webos.media/load", &load);
    crate::log(&format!("player: load -> {r}"));
    let v: serde_json::Value = serde_json::from_str(&r).unwrap_or_default();
    let mid = v["mediaId"].as_str().ok_or_else(|| anyhow::anyhow!("media service refused the load: {r}"))?.to_string();
    *media_id.lock().unwrap() = Some(mid.clone());

    let st = status.clone();
    luna.subscribe("luna://com.webos.media/subscribe", &format!(r#"{{"mediaId":{},"subscribe":true}}"#, json_str(&mid)), move |msg| {
        on_event(&st, msg);
    });
    luna.call("luna://com.webos.media/notifyForeground", &format!(r#"{{"connectionId":{}}}"#, json_str(&mid)));
    let r = luna.call("luna://com.webos.media/play", &format!(r#"{{"mediaId":{}}}"#, json_str(&mid)));
    crate::log(&format!("player: play -> {r}"));
    if let Some(ms) = start_ms.filter(|ms| *ms > 2000) {
        let r = luna.call("luna://com.webos.media/seek", &format!(r#"{{"mediaId":{},"position":{ms}}}"#, json_str(&mid)));
        crate::log(&format!("player: start at {ms} ms -> {r}"));
    }
    Ok(())
}

#[cfg(not(windows))]
fn on_event(status: &Mutex<Status>, msg: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(msg) else { return };
    let mut s = status.lock().unwrap();
    if let Some(t) = v["currentTime"]["currentTime"].as_u64() {
        s.position_ms = t;
    } else if v.get("playing").is_some() {
        s.phase = Phase::Playing;
    } else if v.get("paused").is_some() {
        s.phase = Phase::Paused;
    } else if v.get("endOfStream").is_some() {
        s.phase = Phase::Ended;
    } else if let Some(vi) = v.get("videoInfo") {
        s.width = vi["width"].as_u64().unwrap_or(0) as u32;
        s.height = vi["height"].as_u64().unwrap_or(0) as u32;
        s.fps = vi["frameRate"].as_f64().unwrap_or(0.0) as u32;
        crate::log(&format!("player: video {}x{}@{}", s.width, s.height, s.fps));
    } else if let Some(d) = v["sourceInfo"]["programInfo"][0]["duration"].as_u64() {
        s.duration_ms = d;
    } else if let Some(e) = v.get("error") {
        crate::log(&format!("player: media error {e}"));
        s.phase = Phase::Failed(e["errorText"].as_str().unwrap_or("playback error").to_string());
    } else {
        return;
    }
    drop(s);
    plx_machine::present::wake_from_worker();
}

/// Worker: reports playback to YouTube (watch history, resume position) on YouTube's own
/// schedule until this video stops playing.
#[cfg(not(windows))]
fn track_history(video_id: &str, session: &Mutex<Option<yt_core::session::Session>>, status: &Arc<Mutex<Status>>) {
    let mut t = match crate::host::with_session(session, |s| s.tracking_for(video_id)) {
        Ok(t) => t,
        Err(e) => {
            crate::log(&format!("history: no tracking for {video_id}: {e:#}"));
            return;
        }
    };
    let pos = |s: &Status| s.position_ms as f64 / 1000.0;
    let start = pos(&status.lock().unwrap());
    if let Err(e) = crate::host::with_session(session, |s| s.report_start(&mut t, start)) {
        crate::log(&format!("history: start ping failed: {e:#}"));
    }
    let mut last = start;
    loop {
        let wait = t.next_flush_secs().max(5);
        for _ in 0..wait {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let s = status.lock().unwrap();
            if s.video_id != video_id || matches!(s.phase, Phase::Idle | Phase::Failed(_) | Phase::Ended) {
                let end = if s.video_id == video_id { pos(&s) } else { last };
                drop(s);
                let _ = crate::host::with_session(session, |se| se.report_end(&mut t, end));
                return;
            }
            last = pos(&s);
        }
        let playing = status.lock().unwrap().phase == Phase::Playing;
        if let Err(e) = crate::host::with_session(session, |s| s.report_progress(&mut t, last, playing)) {
            crate::log(&format!("history: ping failed: {e:#}"));
        }
    }
}

static EXT: std::sync::OnceLock<yt_core::ext::Ext> = std::sync::OnceLock::new();

pub fn ext() -> &'static yt_core::ext::Ext {
    EXT.get_or_init(|| {
        let http = yt_core::net::builder().timeout(std::time::Duration::from_secs(10)).build().unwrap_or_default();
        yt_core::ext::Ext::new(http)
    })
}

/// Worker, after playback started: watch-page info, SponsorBlock, dislikes, captions,
/// storyboard. Each piece is published as soon as it arrives.
pub(crate) fn load_extras(video_id: &str, session: &Mutex<Option<yt_core::session::Session>>, status: &Arc<Mutex<Status>>, prefs: &ExtrasPrefs) {
    let mut ex = Extras::default();
    let publish = |ex: &Extras| {
        let mut s = status.lock().unwrap();
        if s.video_id == video_id {
            let mut ex = ex.clone();
            if ex.cues.is_empty() {
                ex.cues = s.extras.cues.clone();
                ex.caption_label = s.extras.caption_label.clone();
            }
            s.extras = Arc::new(ex);
        }
        drop(s);
        plx_machine::present::wake_from_worker();
    };
    let (spec, tracks) = {
        let s = status.lock().unwrap();
        (s.storyboard.clone(), s.captions.clone())
    };
    if let Some(spec) = spec {
        ex.levels = yt_core::storyboard::parse(&spec);
    }
    if prefs.sponsorblock && !prefs.categories.is_empty() {
        match ext().segments(video_id, &prefs.categories) {
            Ok(seg) => ex.segments = seg,
            Err(e) => crate::log(&format!("sponsorblock: {e:#}")),
        }
    }
    publish(&ex);
    match crate::host::with_session(session, |s| s.watch(video_id, None)) {
        Ok(w) => ex.watch = Some(w),
        Err(e) => crate::log(&format!("watch info: {e:#}")),
    }
    if prefs.dislikes {
        ex.votes = ext().votes(video_id).ok().flatten();
    }
    publish(&ex);
    let _ = tracks;
    if prefs.subtitles {
        load_captions(video_id, session, status, &prefs.language, prefs.auto_translate);
    }
}

/// Fetches the captions for `video_id` into the status's extras (worker). Used at start when
/// subtitles are on, and when they are switched on mid-video. A translation that fails (YouTube
/// answers 429 without a PO token) falls back to the video's own track.
pub(crate) fn load_captions(video_id: &str, session: &Mutex<Option<yt_core::session::Session>>, status: &Arc<Mutex<Status>>, lang: &str, translate: bool) {
    let tracks = status.lock().unwrap().captions.clone();
    if tracks.is_empty() {
        crate::log("captions: this video has none");
        return;
    }
    let mut picks = Vec::new();
    if let Some(p) = pick_track(&tracks, lang, translate) {
        picks.push(p);
    }
    // The video's own track (manual first) when the wanted language is missing or fails.
    if let Some(t) = tracks.iter().find(|t| !t.auto).or_else(|| tracks.first()) {
        if !picks.iter().any(|(p, tr)| p.language == t.language && tr.is_none()) {
            picks.push((t.clone(), None));
        }
    }
    for (track, tr) in picks {
        match crate::host::with_session(session, |s| s.captions(&track, tr.as_deref())) {
            Ok(cues) if !cues.is_empty() => {
                let label = match &tr {
                    Some(l) => format!("{} \u{2192} {l}", track.name),
                    None => track.name.clone(),
                };
                crate::log(&format!("captions: {label} ({} cues)", cues.len()));
                let mut s = status.lock().unwrap();
                if s.video_id == video_id {
                    let mut ex = (*s.extras).clone();
                    ex.caption_label = Some(label);
                    ex.cues = cues;
                    s.extras = Arc::new(ex);
                }
                drop(s);
                plx_machine::present::wake_from_worker();
                return;
            }
            Ok(_) => crate::log(&format!("captions: {} is empty", track.name)),
            Err(e) => crate::log(&format!("captions: {} {tr:?}: {e:#}", track.name)),
        }
    }
}

/// Manual track in the wanted language, else auto-generated in it, else (if allowed) a
/// translation of the best available track.
fn pick_track(tracks: &[yt_core::streams::CaptionTrack], lang: &str, translate: bool) -> Option<(yt_core::streams::CaptionTrack, Option<String>)> {
    let base = |l: &str| l.split('-').next().unwrap_or(l).to_ascii_lowercase();
    let want = base(lang);
    if let Some(t) = tracks.iter().find(|t| !t.auto && base(&t.language) == want) {
        return Some((t.clone(), None));
    }
    if let Some(t) = tracks.iter().find(|t| base(&t.language) == want) {
        return Some((t.clone(), None));
    }
    if translate {
        if let Some(t) = tracks.iter().find(|t| t.translatable && !t.auto).or_else(|| tracks.iter().find(|t| t.translatable)) {
            return Some((t.clone(), Some(lang.to_string())));
        }
    }
    None
}

//! Playback engine A: a DASH manifest handed to the TV's media service (uMediaServer) with an SDL
//! exported window as the video plane. Proven on the C3 by the step-2 prototype (`tvplay`).
//!
//! Threading: the exported window is created/destroyed on the main thread (SDL); stream
//! resolution and every luna call run on a worker; media events update `Status`, which the
//! player screen reads every frame.

use crate::ls2::Luna;
use std::ffi::{c_char, c_int, CStr, CString};
use std::sync::{Arc, Mutex};

#[repr(C)]
struct SdlRect {
    x: c_int,
    y: c_int,
    w: c_int,
    h: c_int,
}

type CreateFn = unsafe extern "C" fn(c_int) -> *const c_char;
type SetFn = unsafe extern "C" fn(*const c_char, *mut SdlRect, *mut SdlRect) -> c_int;
type DestroyFn = unsafe extern "C" fn(*const c_char);

fn sdl_sym<T>(name: &str) -> Option<T> {
    let c = CString::new(name).ok()?;
    let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c.as_ptr()) };
    (!p.is_null()).then(|| unsafe { std::mem::transmute_copy(&p) })
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
    pub rate: f32,
    pub title: String,
    pub author: String,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

pub struct Player {
    pub status: Arc<Mutex<Status>>,
    window: Option<CString>,
    media_id: Arc<Mutex<Option<String>>>,
    luna: Option<&'static Luna>,
}

impl Player {
    pub fn new() -> Self {
        Player { status: Default::default(), window: None, media_id: Default::default(), luna: None }
    }

    /// A playback session (and its video window) exists.
    pub fn active(&self) -> bool {
        self.window.is_some()
    }

    fn bus(&mut self) -> Option<&'static Luna> {
        if self.luna.is_none() {
            match Luna::connect(plx_base::paths::app_id()) {
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
        let spawned = plx_base::task::spawn("play", move || {
            match run(luna, &video_id, &window_id, &session, &status, &media_id, prefs, start_ms) {
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

    fn fail(&mut self, why: &str) {
        crate::log(&format!("player: {why}"));
        self.status.lock().unwrap().phase = Phase::Failed(why.to_string());
    }

    pub fn toggle_pause(&mut self) {
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
        let pos = self.status.lock().unwrap().position_ms as i64;
        self.seek_by(to_ms as i64 - pos);
    }

    /// Playback speed (0.25..2.0) through uMediaServer's setPlayRate.
    pub fn set_rate(&mut self, rate: f32) {
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

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// Worker: resolve, write the manifest, load, subscribe, foreground, play.
fn run(
    luna: &'static Luna,
    video_id: &str,
    window_id: &str,
    session: &Mutex<Option<yt_core::session::Session>>,
    status: &Arc<Mutex<Status>>,
    media_id: &Arc<Mutex<Option<String>>>,
    prefs: yt_core::streams::Prefs,
    start_ms: Option<u64>,
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
    }
    std::fs::create_dir_all("/tmp/ytnative")?;
    // Live: the HLS master reduced to H.264 renditions (DASH-from-formats only works for VOD).
    // Needs TV validation: LG's hlsdemux2 stalled on YouTube's VOD TS playlists in early tests.
    let path = if resolved.is_live || resolved.adaptive.iter().all(|f| f["initRange"].is_null()) {
        let http = reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(15)).build()?;
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
        let http = reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(10)).build().unwrap_or_default();
        yt_core::ext::Ext::new(http)
    })
}

/// Worker, after playback started: watch-page info, SponsorBlock, dislikes, captions,
/// storyboard. Each piece is published as soon as it arrives.
fn load_extras(video_id: &str, session: &Mutex<Option<yt_core::session::Session>>, status: &Arc<Mutex<Status>>, prefs: &ExtrasPrefs) {
    let mut ex = Extras::default();
    let publish = |ex: &Extras| {
        let mut s = status.lock().unwrap();
        if s.video_id == video_id {
            s.extras = Arc::new(ex.clone());
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
    if prefs.subtitles {
        if let Some((track, translate)) = pick_track(&tracks, &prefs.language, prefs.auto_translate) {
            let r = crate::host::with_session(session, |s| s.captions(&track, translate.as_deref()));
            match r {
                Ok(cues) => {
                    ex.caption_label = Some(match &translate {
                        Some(l) => format!("{} \u{2192} {l}", track.name),
                        None => track.name.clone(),
                    });
                    ex.cues = cues;
                }
                Err(e) => crate::log(&format!("captions: {e:#}")),
            }
            publish(&ex);
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

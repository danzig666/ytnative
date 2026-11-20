//! Video previews on focus (Settings > Interface, off by default): when the focus rests on a
//! video card of the browse page for a moment, the video plays muted inside the card's thumbnail.
//!
//! It is engine A's path (`player.rs`) in miniature: a DASH manifest handed to the TV's media
//! service, rendered into an SDL exported window placed over the thumbnail. The UI plane is drawn
//! over the video plane, so the thumbnail's rectangle is cleared to alpha 0 after the page is
//! drawn ([`punch`]): the compositor shows the video there, as it does under the player's overlays.
//!
//! Only one pipeline ever runs: a preview plays only on the browse page with nothing over it and
//! the player closed, and the player stops it ([`stop`]) before its own pipeline starts. The
//! manifest is video only (no audio set), so the preview is silent without touching the TV's
//! volume and takes no audio decoder; it is capped at 360p and cached per video, so a card focused
//! again plays without asking YouTube a second time.
//!
//! webOS 4 has no exported window (its plane is ACB, bound only by engine B): there the setting
//! has no effect. The Windows simulator has no media pipeline: the gate runs, nothing plays.
//!
//! Threading: everything here runs on the main thread except the stream resolution, the media
//! service calls of a starting preview and the media events, which only touch [`Play`].

use crate::store::Card;
use plx_ui::Rect;
use std::cell::RefCell;

/// How long the focus rests on a card before its preview starts.
pub const DWELL_MS: u32 = 1500;
/// A preview stops after this long on screen (the thumbnail comes back): it is a glimpse, not a
/// player left running on a page nobody is watching.
pub const MAX_SHOW_MS: u32 = 60_000;
/// The preview's quality: small cards need no more, and the download stays cheap.
#[cfg_attr(windows, allow(dead_code))] // used by the TV engine
pub const HEIGHT: u64 = 360;
/// The pages whose cards preview (by screen name).
const SCREENS: &[&str] = &["browse"];
/// Below the page title band (`browse.rs` draws it over scrolled rows): a card reaching under it
/// would show the video over the title.
const CONTENT_TOP: f32 = 118.0;

/// The focused card as a frame drew it: its video, its thumbnail on the 1920x1080 canvas and
/// where its preview starts.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub video_id: String,
    pub rect: [f32; 4],
    pub start_ms: u64,
}

impl Target {
    /// The same card at the same place (a scroll or a press moves the rectangle).
    fn same_place(&self, o: &Target) -> bool {
        self.video_id == o.video_id && self.rect.iter().zip(o.rect.iter()).all(|(a, b)| (a - b).abs() < 1.5)
    }
}

/// What the gate decides on a frame.
#[derive(Debug, PartialEq)]
pub enum Step {
    Nothing,
    Start(Target),
    Stop,
}

/// When a preview starts and stops: it starts once the same card has stayed in the same place for
/// [`DWELL_MS`], and stops as soon as the focus, the card's place or the page's state changes.
/// A card whose preview already ran (or failed) does not start again until the focus leaves it.
#[derive(Default)]
pub struct Gate {
    seen: Option<Target>,
    since: u32,
    started: bool,
}

impl Gate {
    /// `target`: the focused card when a preview is allowed now, else `None`.
    pub fn step(&mut self, target: Option<&Target>, now: u32) -> Step {
        let same = match (&self.seen, target) {
            (Some(a), Some(b)) => a.same_place(b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            let was = self.started;
            self.seen = target.cloned();
            self.since = now;
            self.started = false;
            return if was { Step::Stop } else { Step::Nothing };
        }
        match &self.seen {
            Some(t) if !self.started && now.wrapping_sub(self.since) >= DWELL_MS => {
                self.started = true;
                Step::Start(t.clone())
            }
            _ => Step::Nothing,
        }
    }
}

/// A card that can preview: a plain video (not a Short, a live or an upcoming stream, a members
/// only video, a playlist or a channel).
pub fn eligible(card: &Card) -> Option<&str> {
    use yt_core::model::ItemKind;
    if card.item.kind != ItemKind::Video || card.item.members_only {
        return None;
    }
    let badge = card.item.badge.as_deref().unwrap_or("").trim().to_ascii_uppercase();
    if matches!(badge.as_str(), "SHORTS" | "LIVE" | "UPCOMING") {
        return None;
    }
    card.video_id()
}

/// Where a preview starts: a tenth into the video (past the intro), from the start when the
/// length is unknown or the video is short.
pub fn start_at(duration_ms: Option<u64>) -> u64 {
    match duration_ms {
        Some(d) if d >= 20_000 => d / 10,
        _ => 0,
    }
}

/// `r` (`[x, y, w, h]` on the authored 1920x1080 canvas) on the video plane's 1920x1080 screen,
/// through the UI's viewport (`plx_base::surface::viewport`: the canvas scaled by "Screen size"
/// and centred in the drawable, which fills the screen).
pub fn plane_rect(r: [f32; 4], viewport: (i32, i32, i32, i32)) -> [i32; 4] {
    let (vx, vy, vw, vh) = viewport;
    let s = vw as f32 / 1920.0;
    let (dw, dh) = ((vw + 2 * vx).max(1) as f32, (vh + 2 * vy).max(1) as f32);
    let (kx, ky) = (1920.0 / dw, 1080.0 / dh);
    let x0 = ((vx as f32 + r[0] * s) * kx).round();
    let y0 = ((vy as f32 + r[1] * s) * ky).round();
    let x1 = ((vx as f32 + (r[0] + r[2]) * s) * kx).round();
    let y1 = ((vy as f32 + (r[1] + r[3]) * s) * ky).round();
    [x0 as i32, y0 as i32, (x1 - x0) as i32, (y1 - y0) as i32]
}

/// The DASH manifest without its audio set: the preview is silent and takes no audio decoder.
#[cfg_attr(windows, allow(dead_code))] // used by the TV engine
pub fn video_only(mpd: &str) -> Option<String> {
    let at = mpd.find("contentType=\"audio\"")?;
    let start = mpd[..at].rfind("<AdaptationSet")?;
    const END: &str = "</AdaptationSet>";
    let end = start + mpd[start..].find(END)? + END.len();
    let rest = mpd[end..].strip_prefix('\n').unwrap_or(&mpd[end..]);
    let out = format!("{}{}", &mpd[..start], rest);
    out.contains("contentType=\"video\"").then_some(out)
}

/// Manifests by video, so a card focused again (or the next time past it) asks YouTube nothing.
/// A video that cannot preview is remembered too, for a shorter while.
#[cfg_attr(windows, allow(dead_code))] // used by the TV engine
pub struct Cache {
    entries: Vec<(String, u64, Option<String>)>,
}

#[cfg_attr(windows, allow(dead_code))] // used by the TV engine
impl Cache {
    const CAP: usize = 16;
    /// Stream URLs live about six hours; half an hour keeps well inside that.
    const TTL_SECS: u64 = 1800;
    const MISS_SECS: u64 = 600;

    pub const fn new() -> Cache {
        Cache { entries: Vec::new() }
    }

    /// `Some(Some(mpd))`: cached; `Some(None)`: known not to preview; `None`: not known.
    pub fn get(&mut self, video_id: &str, now_secs: u64) -> Option<Option<String>> {
        self.entries.retain(|(_, at, m)| now_secs.saturating_sub(*at) < if m.is_some() { Self::TTL_SECS } else { Self::MISS_SECS });
        self.entries.iter().find(|e| e.0 == video_id).map(|e| e.2.clone())
    }

    pub fn put(&mut self, video_id: &str, now_secs: u64, mpd: Option<String>) {
        self.entries.retain(|e| e.0 != video_id);
        if self.entries.len() >= Self::CAP {
            self.entries.remove(0);
        }
        self.entries.push((video_id.to_string(), now_secs, mpd));
    }
}

/// What the main loop knows about the screen this frame.
pub struct Scene<'a> {
    /// The setting.
    pub enabled: bool,
    /// The top page's name.
    pub top: Option<&'a str>,
    /// Something lies over the cards: the sidebar, a menu, comments.
    pub covered: bool,
    /// The player has a playback session (its video window exists).
    pub player_active: bool,
    /// The app is in front and the screen is not blanked.
    pub visible: bool,
}

impl Scene<'_> {
    fn allowed(&self) -> bool {
        self.enabled && self.visible && !self.covered && !self.player_active && self.top.is_some_and(|t| SCREENS.contains(&t))
    }
}

#[derive(Default)]
struct State {
    gate: Gate,
    engine: engine::Engine,
    /// The preview on screen: its target and since when (frame clock).
    shown: Option<(Target, u32)>,
}

thread_local! {
    /// The focused previewable card the last page draw showed (see [`begin_draw`]).
    static DRAWN: RefCell<Option<Target>> = const { RefCell::new(None) };
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> T {
    STATE.with(|s| f(s.borrow_mut().get_or_insert_with(State::default)))
}

/// Main thread, before the page is drawn: forget the last frame's focused card.
pub fn begin_draw() {
    DRAWN.with(|d| *d.borrow_mut() = None);
}

/// Card drawing (`ui::CardsSrc::overlay`): the focused card at `screen` (its thumbnail on the
/// canvas, popped) inside `clip`. Only a whole thumbnail below the title band can preview.
pub fn note_focused(card: &Card, screen: Rect, clip: Rect) {
    let Some(video_id) = eligible(card) else { return };
    let inside = screen.x >= clip.x - 0.5
        && screen.y >= clip.y.max(CONTENT_TOP) - 0.5
        && screen.x + screen.w <= (clip.x + clip.w).min(1920.0) + 0.5
        && screen.y + screen.h <= (clip.y + clip.h).min(1080.0) + 0.5
        && screen.w > 0.0
        && screen.h > 0.0;
    if !inside {
        return;
    }
    let duration = card.item.badge.as_deref().and_then(crate::store::parse_duration);
    let t = Target { video_id: video_id.to_string(), rect: [screen.x, screen.y, screen.w, screen.h], start_ms: start_at(duration) };
    DRAWN.with(|d| *d.borrow_mut() = Some(t));
}

/// Main thread, every loop pass after the input was handled and before the frame is drawn:
/// starts, keeps or stops the preview.
pub fn frame(scene: &Scene, now: u32) {
    let allowed = scene.allowed();
    let target = if allowed { DRAWN.with(|d| d.borrow().clone()) } else { None };
    with_state(|st| {
        match st.gate.step(target.as_ref(), now) {
            Step::Start(t) => {
                let plane = plane_rect(t.rect, plx_base::surface::viewport());
                st.engine.start(&t.video_id, plane, t.start_ms);
            }
            Step::Stop => st.engine.stop(),
            Step::Nothing => {}
        }
        if st.engine.finished() {
            st.engine.stop();
        }
        let showing = allowed && st.engine.showing();
        match (&st.shown, showing) {
            (None, true) => {
                st.shown = st.gate.seen.clone().map(|t| (t, now));
                plx_machine::idle::invalidate();
            }
            (Some(_), false) => {
                st.shown = None;
                plx_machine::idle::invalidate();
            }
            (Some((_, since)), true) if now.wrapping_sub(*since) >= MAX_SHOW_MS => {
                st.engine.stop();
                st.shown = None;
                plx_machine::idle::invalidate();
            }
            _ => {}
        }
    });
}

/// Main thread, right after the page is drawn: clears the playing preview's thumbnail to alpha 0
/// so the video plane shows through it. Only where this very frame drew the card: a card that
/// moved meanwhile is stopped by the next [`frame`] and must not leave a hole behind.
pub fn punch() {
    let Some((t, _)) = with_state(|st| st.shown.clone()) else { return };
    let drawn = DRAWN.with(|d| d.borrow().clone());
    if !drawn.is_some_and(|d| d.same_place(&t)) {
        return;
    }
    let [x, y, w, h] = t.rect;
    plx_gfx::gfx::clip_set(x, y, w, h);
    plx_gfx::gfx::frame_clear_through();
    plx_gfx::gfx::clip_clear();
}

/// Main thread: stops a preview at once (the player is about to start its own pipeline).
pub fn stop() {
    with_state(|st| {
        st.engine.stop();
        if st.shown.take().is_some() {
            plx_machine::idle::invalidate();
        }
    });
}

/// The simulator: no media pipeline, nothing plays.
#[cfg(windows)]
mod engine {
    #[derive(Default)]
    pub struct Engine;

    impl Engine {
        pub fn start(&mut self, _video_id: &str, _plane: [i32; 4], _start_ms: u64) {}
        pub fn stop(&mut self) {}
        pub fn showing(&self) -> bool {
            false
        }
        pub fn finished(&self) -> bool {
            false
        }
    }
}

#[cfg(not(windows))]
mod engine {
    use super::Cache;
    use crate::ls2::Luna;
    use std::ffi::{c_char, c_int, CStr, CString};
    use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
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

    fn sdl_sym<T>(name: &CStr) -> Option<T> {
        let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
        (!p.is_null()).then(|| unsafe { std::mem::transmute_copy(&p) })
    }

    fn json_str(s: &str) -> String {
        serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
    }

    static CACHE: Mutex<Cache> = Mutex::new(Cache::new());
    /// The previews' own YouTube session: resolving a preview never holds the player's session
    /// lock while the viewer starts a video.
    static SESSION: Mutex<Option<yt_core::session::Session>> = Mutex::new(None);

    /// A starting or playing preview, as its worker and the media events see it.
    #[derive(Default)]
    struct Play {
        playing: bool,
        position_ms: u64,
        /// At the start position and playing: the hole may open.
        ready: bool,
        /// Ended, failed, or nothing to play: the pipeline is unloaded by the next frame.
        done: bool,
    }

    #[derive(Default)]
    pub struct Engine {
        window: Option<CString>,
        /// The loaded pipeline. Its lock also orders a worker's store against `stop` (see there).
        media_id: Arc<Mutex<Option<String>>>,
        /// Bumped by every start and stop: a worker of an older number drops what it made.
        gen: Arc<AtomicU64>,
        play: Arc<Mutex<Play>>,
        /// webOS 4 (no exported window) was found and said once.
        unavailable: bool,
    }

    impl Engine {
        pub fn start(&mut self, video_id: &str, plane: [i32; 4], start_ms: u64) {
            self.stop();
            let (Some(create), Some(set)) = (sdl_sym::<CreateFn>(c"SDL_webOSCreateExportedWindow"), sdl_sym::<SetFn>(c"SDL_webOSSetExportedWindow")) else {
                if !self.unavailable {
                    self.unavailable = true;
                    crate::log("preview: no SDL exported window on this TV (webOS 4); video previews are off");
                }
                return;
            };
            let luna = match Luna::shared(plx_base::paths::app_id()) {
                Ok(l) => l,
                Err(e) => {
                    crate::log(&format!("preview: LS2 unavailable: {e}"));
                    return;
                }
            };
            let wid = unsafe { create(0) };
            if wid.is_null() {
                crate::log("preview: could not create the video window");
                return;
            }
            let wid = unsafe { CStr::from_ptr(wid) }.to_owned();
            let [x, y, w, h] = plane;
            let mut src = SdlRect { x: 0, y: 0, w: 1920, h: 1080 };
            let mut dst = SdlRect { x, y, w, h };
            unsafe { set(wid.as_ptr(), &mut src, &mut dst) };
            let window_id = wid.to_string_lossy().into_owned();
            self.window = Some(wid);
            let my = self.gen.fetch_add(1, SeqCst) + 1;
            *self.play.lock().unwrap() = Play::default();
            let (gen, media_id, play, video_id) = (self.gen.clone(), self.media_id.clone(), self.play.clone(), video_id.to_string());
            let spawned = plx_base::task::spawn("preview", move || {
                let live = || gen.load(SeqCst) == my;
                if let Err(e) = run(luna, &video_id, &window_id, start_ms, &gen, my, &media_id, &play) {
                    if live() {
                        crate::log(&format!("preview: {e:#}"));
                    }
                    play.lock().unwrap().done = true;
                }
                plx_machine::present::wake_from_worker();
            });
            if spawned.is_none() {
                self.play.lock().unwrap().done = true;
            }
        }

        /// Unloads the pipeline (synchronously: the window must outlive it) and destroys the window.
        pub fn stop(&mut self) {
            let mid = {
                // Under the lock a starting worker stores its media id with: it either stored it
                // already (unloaded here) or sees the new number and unloads it itself.
                let mut g = self.media_id.lock().unwrap();
                self.gen.fetch_add(1, SeqCst);
                g.take()
            };
            if let Some(mid) = mid {
                if let Ok(luna) = Luna::shared(plx_base::paths::app_id()) {
                    luna.call("luna://com.webos.media/unload", &format!(r#"{{"mediaId":{}}}"#, json_str(&mid)));
                }
            }
            if let Some(w) = self.window.take() {
                if let Some(destroy) = sdl_sym::<DestroyFn>(c"SDL_webOSDestroyExportedWindow") {
                    unsafe { destroy(w.as_ptr()) };
                }
            }
            *self.play.lock().unwrap() = Play::default();
        }

        pub fn showing(&self) -> bool {
            self.window.is_some() && {
                let p = self.play.lock().unwrap();
                p.ready && !p.done
            }
        }

        pub fn finished(&self) -> bool {
            self.window.is_some() && self.play.lock().unwrap().done
        }
    }

    /// The manifest for `video_id`, from the cache or resolved now.
    fn manifest(video_id: &str) -> anyhow::Result<String> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        if let Some(m) = CACHE.lock().unwrap().get(video_id, now) {
            return m.ok_or_else(|| anyhow::anyhow!("this video has no preview"));
        }
        let resolved = {
            let mut g = SESSION.lock().unwrap();
            if g.is_none() {
                *g = Some(yt_core::session::Session::new()?);
            }
            match g.as_mut().unwrap().resolve(video_id) {
                Ok(r) => r,
                Err(e) => {
                    // A refused stream may be the session's: the next preview starts a fresh one.
                    *g = None;
                    return Err(e);
                }
            }
        };
        let prefs = yt_core::streams::Prefs { min_height: super::HEIGHT, max_height: super::HEIGHT, codec: "avc1", audio_track: None, audio_language: None };
        let mpd = (!resolved.is_live && !resolved.is_upcoming).then(|| resolved.mpd(prefs)).flatten().and_then(|m| super::video_only(&m));
        CACHE.lock().unwrap().put(video_id, now, mpd.clone());
        mpd.ok_or_else(|| anyhow::anyhow!("this video has no preview"))
    }

    /// Worker: manifest, load into the window, play, and seek to the start position before the
    /// hole opens (a seek sent right after play is ignored by the pipeline, see `player.rs`).
    #[allow(clippy::too_many_arguments)]
    fn run(
        luna: &'static Luna,
        video_id: &str,
        window_id: &str,
        start_ms: u64,
        gen: &AtomicU64,
        my: u64,
        media_id: &Arc<Mutex<Option<String>>>,
        play: &Arc<Mutex<Play>>,
    ) -> anyhow::Result<()> {
        let live = || gen.load(SeqCst) == my;
        let mpd = manifest(video_id)?;
        if !live() {
            return Ok(());
        }
        std::fs::create_dir_all("/tmp/ytnative")?;
        let path = "/tmp/ytnative/preview.mpd";
        std::fs::write(path, mpd)?;
        let load = format!(
            r#"{{"uri":"file://{path}","type":"media","payload":{{"option":{{"appId":{},"windowId":{}}}}}}}"#,
            json_str(plx_base::paths::app_id()),
            json_str(window_id)
        );
        let r = luna.call("luna://com.webos.media/load", &load);
        let v: serde_json::Value = serde_json::from_str(&r).unwrap_or_default();
        let mid = v["mediaId"].as_str().ok_or_else(|| anyhow::anyhow!("the media service refused the load"))?.to_string();
        {
            let mut g = media_id.lock().unwrap();
            if !live() {
                drop(g);
                luna.call("luna://com.webos.media/unload", &format!(r#"{{"mediaId":{}}}"#, json_str(&mid)));
                return Ok(());
            }
            *g = Some(mid.clone());
        }
        let (current, me, p) = (media_id.clone(), mid.clone(), play.clone());
        luna.subscribe("luna://com.webos.media/subscribe", &format!(r#"{{"mediaId":{},"subscribe":true}}"#, json_str(&mid)), move |msg| {
            if current.lock().unwrap().as_deref() == Some(me.as_str()) {
                on_event(&p, msg);
            }
        });
        luna.call("luna://com.webos.media/play", &format!(r#"{{"mediaId":{}}}"#, json_str(&mid)));
        let wait = |secs: u64, until: &dyn Fn(&Play) -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
            while live() && std::time::Instant::now() < deadline {
                let p = play.lock().unwrap();
                if p.done || until(&p) {
                    break;
                }
                drop(p);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        };
        wait(15, &|p| p.playing || p.position_ms > 0);
        if start_ms > 2000 && live() {
            luna.call("luna://com.webos.media/seek", &format!(r#"{{"mediaId":{},"position":{start_ms}}}"#, json_str(&mid)));
            wait(5, &|p| p.position_ms + 1500 >= start_ms);
        }
        let mut p = play.lock().unwrap();
        if live() && !p.done {
            if p.playing || p.position_ms > 0 {
                p.ready = true;
            } else {
                anyhow::bail!("the preview did not start");
            }
        }
        Ok(())
    }

    fn on_event(play: &Mutex<Play>, msg: &str) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(msg) else { return };
        let mut p = play.lock().unwrap();
        if let Some(t) = v["currentTime"]["currentTime"].as_u64() {
            p.position_ms = t;
            return; // no wake: the hole opens from the worker
        } else if v.get("playing").is_some() {
            p.playing = true;
        } else if v.get("endOfStream").is_some() || v.get("error").is_some() {
            p.done = true;
        } else {
            return;
        }
        drop(p);
        plx_machine::present::wake_from_worker();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(id: &str, x: f32) -> Target {
        Target { video_id: id.into(), rect: [x, 300.0, 400.0, 225.0], start_ms: 0 }
    }

    #[test]
    fn starts_after_the_focus_rests_and_stops_when_it_moves() {
        let mut g = Gate::default();
        let a = target("a", 100.0);
        assert_eq!(g.step(Some(&a), 1000), Step::Nothing);
        assert_eq!(g.step(Some(&a), 1000 + DWELL_MS - 1), Step::Nothing);
        assert_eq!(g.step(Some(&a), 1000 + DWELL_MS), Step::Start(a.clone()));
        assert_eq!(g.step(Some(&a), 5000), Step::Nothing, "starts once");
        let b = target("b", 540.0);
        assert_eq!(g.step(Some(&b), 5100), Step::Stop);
        assert_eq!(g.step(Some(&b), 5100 + DWELL_MS), Step::Start(b));
    }

    #[test]
    fn a_scroll_or_a_covered_page_stops_it_and_waits_again() {
        let mut g = Gate::default();
        let a = target("a", 100.0);
        g.step(Some(&a), 0);
        assert!(matches!(g.step(Some(&a), DWELL_MS), Step::Start(_)));
        // The page scrolled: same card, another place.
        let moved = target("a", 60.0);
        assert_eq!(g.step(Some(&moved), DWELL_MS + 16), Step::Stop);
        assert_eq!(g.step(Some(&moved), DWELL_MS + 32), Step::Nothing, "waits for the scroll to rest");
        assert!(matches!(g.step(Some(&moved), 2 * DWELL_MS + 16), Step::Start(_)));
        // A menu opened (no target while not allowed).
        assert_eq!(g.step(None, 5000), Step::Stop);
        assert_eq!(g.step(None, 9000), Step::Nothing);
        // A sub-pixel wobble is the same place.
        let mut g = Gate::default();
        g.step(Some(&a), 0);
        assert!(matches!(g.step(Some(&target("a", 100.4)), DWELL_MS), Step::Start(_)));
    }

    #[test]
    fn start_position_is_a_tenth_in() {
        assert_eq!(start_at(Some(600_000)), 60_000);
        assert_eq!(start_at(Some(10_000)), 0, "short videos from the start");
        assert_eq!(start_at(None), 0);
    }

    #[test]
    fn plane_rect_follows_the_screen_size_setting() {
        assert_eq!(plane_rect([100.0, 200.0, 400.0, 225.0], (0, 0, 1920, 1080)), [100, 200, 400, 225]);
        // 90 %: the canvas is 1728x972, centred.
        assert_eq!(plane_rect([0.0, 0.0, 1920.0, 1080.0], (96, 54, 1728, 972)), [96, 54, 1728, 972]);
        assert_eq!(plane_rect([100.0, 200.0, 400.0, 225.0], (96, 54, 1728, 972)), [186, 234, 360, 203]);
    }

    #[test]
    fn the_manifest_loses_its_audio_set() {
        let prefs = yt_core::streams::Prefs { min_height: HEIGHT, max_height: HEIGHT, codec: "avc1", audio_track: None, audio_language: None };
        let f = |itag: u64, mime: &str, h: u64| {
            serde_json::json!({"itag": itag, "mimeType": mime, "width": h * 16 / 9, "height": h, "bitrate": 1000, "url": "https://x/v?a=1",
                "initRange": {"start": "0", "end": "9"}, "indexRange": {"start": "10", "end": "19"}})
        };
        let formats = [f(134, "video/mp4; codecs=\"avc1.4d401e\"", 360), f(136, "video/mp4; codecs=\"avc1.4d401f\"", 720), f(140, "audio/mp4; codecs=\"mp4a.40.2\"", 0)];
        let (mpd, nv, na) = yt_core::mpd::build(&formats, 60_000, prefs.codec, prefs.min_height, prefs.max_height).unwrap();
        assert_eq!((nv, na), (1, 1));
        let v = video_only(&mpd).unwrap();
        assert!(!v.contains("audio") && v.contains("id=\"134\"") && !v.contains("id=\"136\""));
        assert!(v.contains("</AdaptationSet>\n</Period>"));
        assert_eq!(video_only("<MPD></MPD>"), None);
    }

    #[test]
    fn cache_keeps_manifests_and_misses_for_a_while() {
        let mut c = Cache::new();
        c.put("a", 100, Some("mpd".into()));
        c.put("b", 100, None);
        assert_eq!(c.get("a", 200), Some(Some("mpd".into())));
        assert_eq!(c.get("b", 200), Some(None));
        assert_eq!(c.get("b", 100 + Cache::MISS_SECS), None, "a miss is tried again later");
        assert_eq!(c.get("a", 100 + Cache::TTL_SECS), None, "stream addresses expire");
        for i in 0..Cache::CAP + 2 {
            c.put(&i.to_string(), 5000, None);
        }
        assert_eq!(c.entries.len(), Cache::CAP);
        assert_eq!(c.get("0", 5000), None, "the oldest goes first");
    }

    #[test]
    fn only_plain_videos_preview() {
        use yt_core::model::{Endpoint, Item, ItemKind};
        let card = |kind: ItemKind, badge: &str| Card {
            elem: 1,
            item: Item {
                kind,
                id: "vid".into(),
                badge: Some(badge.into()),
                endpoint: Some(Endpoint::Watch { video_id: "vid".into(), playlist_id: None, params: None }),
                ..Default::default()
            },
            title: String::new(),
            detail: String::new(),
            thumb: String::new(),
        };
        assert_eq!(eligible(&card(ItemKind::Video, "12:34")), Some("vid"));
        assert_eq!(eligible(&card(ItemKind::Short, "")), None);
        assert_eq!(eligible(&card(ItemKind::Video, "SHORTS")), None);
        assert_eq!(eligible(&card(ItemKind::Live, "LIVE")), None);
        assert_eq!(eligible(&card(ItemKind::Upcoming, "UPCOMING")), None);
        assert_eq!(eligible(&card(ItemKind::Playlist, "")), None);
        let mut members = card(ItemKind::Video, "1:00");
        members.item.members_only = true;
        assert_eq!(eligible(&members), None);
    }
}

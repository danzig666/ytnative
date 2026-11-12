//! Full-screen playback with an auto-hiding overlay (HUD).
//!
//! Keys: OK shows the HUD, or toggles pause when it is up; Left/Right seek (HUD shows the target);
//! Up opens the player options; Play/Pause/FF/RW remote keys work directly; BACK hides the HUD
//! first, then closes the player (which stops playback).

use super::common;
use crate::host::{YtArg, YtFx, YtHost};
use crate::player::Phase;
use crate::settings::Repeat;
use crate::ui;
use plx_machine::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, Key, LogicalState, Machine, NavOp, ScreenEvent,
    TimerId,
};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use std::borrow::Cow;

const T_HIDE: TimerId = TimerId(1);
const T_TICK: TimerId = TimerId(2);

const WCODE_PLAY: u32 = 450;
const WCODE_PAUSE: u32 = 72;
const WCODE_PLAYPAUSE: u32 = 261;
const WCODE_FF: u32 = 451;
const WCODE_RW: u32 = 452;
const WCODE_STOP: [u32; 3] = [120, 260, 413];
const WCODE_CH_UP: u32 = 300;
const WCODE_CH_DOWN: u32 = 301;

struct State(String);
impl LogicalState for State {
    fn write(&self, c: &mut Canon) {
        c.str(&self.0);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&self.0);
    }
}

pub struct PlayerScreen {
    entry: EntryId,
    video_id: String,
    playlist: Option<String>,
    hud: bool,
    ended_handled: bool,
    /// Show the storyboard preview until this time (ms, views.now_ms clock).
    preview_until: u32,
    state: State,
}

impl PlayerScreen {
    pub fn new(entry: EntryId, video_id: String, playlist: Option<String>) -> Self {
        PlayerScreen { entry, state: State(video_id.clone()), video_id, playlist, hud: true, ended_handled: false, preview_until: 0 }
    }

    fn show_hud(&mut self, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        self.hud = true;
        let secs = cx.views.store.settings.hud_timeout_secs.max(2);
        fx.push(Fx::CancelTimer(T_HIDE));
        fx.push(Fx::Timer { id: T_HIDE, after_ms: secs * 1000 });
        fx.push(Fx::Timer { id: T_TICK, after_ms: 500 });
        fx.invalidate(plx_machine::present::Provenance::Input);
    }

    /// Previous / next video of the list this one was opened from.
    fn step_queue(&mut self, dir: i32, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        let q = &cx.views.store.queue;
        let Some(i) = q.iter().position(|v| *v == self.video_id) else {
            fx.push(Fx::App(YtFx::Toast("No other videos in this list".into())));
            return;
        };
        let j = i as i32 + dir;
        if j < 0 || j as usize >= q.len() {
            fx.push(Fx::App(YtFx::Toast(if dir < 0 { "This is the first video".into() } else { "This is the last video".into() })));
            return;
        }
        fx.push(Fx::Nav(NavOp::Replace(YtArg::Player { video_id: q[j as usize].clone(), playlist: self.playlist.clone() })));
    }

    fn seek(&mut self, dir: i64, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        let step = cx.views.store.settings.seek_step_secs.max(1) as i64 * 1000;
        fx.push(Fx::App(YtFx::SeekBy(dir * step)));
        self.preview_until = cx.views.now_ms + 1500;
        self.show_hud(cx, fx);
    }
}

impl Machine<YtHost> for PlayerScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                fx.push(Fx::App(YtFx::Play { video_id: self.video_id.clone(), playlist: self.playlist.clone() }));
                self.show_hud(cx, fx);
                Handled::Yes
            }
            ScreenEvent::Unmount => {
                fx.push(Fx::App(YtFx::Stop));
                Handled::Yes
            }
            ScreenEvent::Uncover => {
                self.show_hud(cx, fx);
                Handled::Yes
            }
            ScreenEvent::Timer(T_HIDE) => {
                if cx.views.player.phase == Phase::Playing {
                    self.hud = false;
                    fx.invalidate(plx_machine::present::Provenance::Timer(T_HIDE));
                }
                Handled::Yes
            }
            ScreenEvent::Timer(T_TICK) => {
                let p = cx.views.player;
                // End of video: repeat or autoplay according to settings.
                if p.phase == Phase::Ended && !self.ended_handled {
                    self.ended_handled = true;
                    match cx.views.store.settings.repeat {
                        Repeat::One => {
                            self.ended_handled = false;
                            fx.push(Fx::App(YtFx::Play { video_id: self.video_id.clone(), playlist: self.playlist.clone() }));
                        }
                        Repeat::Autoplay | Repeat::List => {
                            let repeat_list = cx.views.store.settings.repeat == Repeat::List;
                            match next_video(cx, &self.video_id, repeat_list) {
                                Some((v, pl)) => fx.push(Fx::Nav(NavOp::Replace(YtArg::Player {
                                    video_id: v,
                                    playlist: pl.or(self.playlist.clone()),
                                }))),
                                None => self.show_hud(cx, fx),
                            }
                        }
                        Repeat::Off => self.show_hud(cx, fx),
                    }
                }
                if self.hud || p.phase != Phase::Playing {
                    fx.push(Fx::Timer { id: T_TICK, after_ms: 500 });
                }
                fx.invalidate(plx_machine::present::Provenance::Timer(T_TICK));
                Handled::Yes
            }
            // Magic Remote pointer: moving shows the controls; a click on the seek bar seeks there,
            // on "Options" opens the menu, anywhere else shows the controls or toggles pause.
            ScreenEvent::Input(InputEvent { kind: InputKind::Pointer { .. } | InputKind::Drag { .. }, .. }) => {
                self.show_hud(cx, fx); // up, and kept up while the pointer moves
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Click { x, y, .. }, .. }) => {
                let st = cx.views.player;
                let (x, y) = (*x, *y);
                if self.hud && (890.0..=960.0).contains(&y) && (96.0..=1824.0).contains(&x) && st.duration_ms > 0 && !st.live {
                    let to = ((x - 96.0) / 1728.0).clamp(0.0, 1.0) as f64 * st.duration_ms as f64;
                    fx.push(Fx::App(YtFx::SeekTo(to as u64)));
                    self.preview_until = cx.views.now_ms + 1500;
                    self.show_hud(cx, fx);
                } else if self.hud && x > 1600.0 && y > 990.0 {
                    fx.push(Fx::Nav(NavOp::Present(YtArg::PlayerMenu)));
                } else if !self.hud {
                    self.show_hud(cx, fx);
                } else {
                    fx.push(Fx::App(YtFx::TogglePause));
                    self.show_hud(cx, fx);
                }
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key, wcode, edge, .. }, .. }) => {
                if *edge == Edge::Up {
                    return if *key == Key::Back && !self.hud { Handled::No } else { Handled::Yes };
                }
                match (key, *wcode) {
                    (Key::Back, _) => {
                        if self.hud && cx.views.player.phase == Phase::Playing {
                            self.hud = false;
                            fx.invalidate(plx_machine::present::Provenance::Input);
                            Handled::Yes
                        } else {
                            Handled::No
                        }
                    }
                    (_, w) if WCODE_STOP.contains(&w) => {
                        fx.push(Fx::Nav(NavOp::Pop));
                        Handled::Yes
                    }
                    (Key::Ok, _) if !self.hud => {
                        self.show_hud(cx, fx);
                        Handled::Yes
                    }
                    (Key::Ok, _) => {
                        fx.push(Fx::App(YtFx::OkInPlayer));
                        self.show_hud(cx, fx);
                        Handled::Yes
                    }
                    (_, WCODE_PLAY) | (_, WCODE_PAUSE) | (_, WCODE_PLAYPAUSE) => {
                        fx.push(Fx::App(YtFx::TogglePause));
                        self.show_hud(cx, fx);
                        Handled::Yes
                    }
                    (Key::Left, _) | (_, WCODE_RW) => {
                        self.seek(-1, cx, fx);
                        Handled::Yes
                    }
                    (_, WCODE_FF) if cx.views.player.live => {
                        // Back to the live edge (the seek is clamped to it).
                        fx.push(Fx::App(YtFx::SeekBy(i64::MAX / 4)));
                        self.show_hud(cx, fx);
                        Handled::Yes
                    }
                    (Key::Right, _) | (_, WCODE_FF) => {
                        self.seek(1, cx, fx);
                        Handled::Yes
                    }
                    (_, WCODE_CH_UP) => {
                        self.step_queue(-1, cx, fx);
                        Handled::Yes
                    }
                    (_, WCODE_CH_DOWN) => {
                        self.step_queue(1, cx, fx);
                        Handled::Yes
                    }
                    (Key::Up, _) | (Key::Down, _) if cx.views.store.queue_shorts => {
                        self.step_queue(if *key == Key::Up { -1 } else { 1 }, cx, fx);
                        Handled::Yes
                    }
                    (Key::Up, _) => {
                        // Up shows / hides the on-screen controls.
                        if self.hud {
                            self.hud = false;
                            fx.push(Fx::CancelTimer(T_HIDE));
                            fx.invalidate(plx_machine::present::Provenance::Input);
                        } else {
                            self.show_hud(cx, fx);
                        }
                        Handled::Yes
                    }
                    (Key::Down, _) => {
                        fx.push(Fx::Nav(NavOp::Present(YtArg::PlayerMenu)));
                        Handled::Yes
                    }
                    _ => {
                        self.show_hud(cx, fx);
                        Handled::Yes
                    }
                }
            }
            _ => Handled::Yes,
        }
    }
}

impl Focusable<YtHost> for PlayerScreen {
    fn groups(&self, _cx: &Cx<'_, YtHost>, _out: &mut Vec<GroupSpec>) {}
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        None
    }
    fn neighbour(&self, _key: FocusKey<u32>, _dir: Dir, _cx: &Cx<'_, YtHost>) -> Step<u32> {
        Step::Edge
    }
    fn place(&self, _key: &u32, _cx: &Cx<'_, YtHost>, _at: At) -> Option<Placed> {
        None
    }
    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        want
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: 0 }
    }
}

impl Screen<YtHost> for PlayerScreen {
    fn name(&self) -> &'static str {
        "player"
    }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, YtHost>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, YtHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, YtHost>) {
        let p = f.painter;
        let cx = f.cx;
        let st = cx.views.player;
        // Status in the middle while there is no picture yet / on failure.
        let centre = match &st.phase {
            Phase::Idle | Phase::Resolving => Some("Loading\u{2026}".to_string()),
            Phase::Loading => Some("Starting playback\u{2026}".to_string()),
            Phase::Failed(e) => Some(format!("Can't play this video: {}", ui::ellipsize(e, 80))),
            _ => None,
        };
        // Windows simulator: no media pipeline and no video plane; mark where the picture would be.
        #[cfg(windows)]
        if centre.is_none() {
            ui::fill(p, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.06, 0.06, 0.07, 1.0]);
            ui::text(p, "Simulator: no video", 960.0, 470.0, 36, ui::DIM, ui::ALIGN_CENTER, false);
            let res = if st.height > 0 { format!("{}p  \u{00B7}  ", st.height) } else { String::new() };
            ui::text(p, &format!("{res}fake clock, HUD and menus only"), 960.0, 530.0, 26, ui::DIM, ui::ALIGN_CENTER, false);
        }
        if let Some(msg) = centre {
            ui::fill(p, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.0, 0.0, 0.0, 1.0]);
            ui::text(p, &msg, 960.0, 510.0, 36, ui::DIM, ui::ALIGN_CENTER, false);
        }
        let ex = st.extras.clone();
        if cx.views.store.settings.subtitles {
            if let Some(cue) = yt_core::captions::cue_at(&ex.cues, st.position_ms) {
                let lift = if self.hud { 330.0 } else { 0.0 };
                let s = &cx.views.store.settings;
                let ink = match s.subtitle_color.as_str() {
                    "yellow" => [1.0, 0.92, 0.25, 1.0],
                    "cyan" => [0.45, 0.95, 1.0, 1.0],
                    _ => ui::WHITE,
                };
                draw_caption(p, &cue.text, s.subtitle_size as i32, s.subtitle_bottom_offset as f32 + lift, ink, s.subtitle_background as f32 / 100.0);
            }
        }
        if !self.hud && st.phase == Phase::Playing {
            common::toast(p, cx);
            return;
        }
        // Bottom scrim + title + progress
        ui::fill(p, Rect::new(0.0, 755.0, 1920.0, 325.0), 0.0, [0.0, 0.0, 0.0, 0.55]);
        if cx.views.store.settings.show_clock {
            ui::fill(p, Rect::new(0.0, 0.0, 1920.0, 96.0), 0.0, [0.0, 0.0, 0.0, 0.40]);
        }
        ui::text(p, &ui::ellipsize(&st.title, 80), 96.0, 780.0, 44, ui::WHITE, ui::ALIGN_LEFT, true);
        let mut byline = st.author.clone();
        if let Some(w) = &ex.watch {
            if let Some(subs) = &w.subscriber_count_text {
                byline.push_str(&format!("  \u{2022}  {subs}"));
            }
            if let Some(v) = &w.short_view_count_text {
                byline.push_str(&format!("  \u{2022}  {v}"));
            }
        }
        match (&ex.votes, ex.watch.as_ref().and_then(|w| w.like_count_text.clone())) {
            (Some(v), _) => byline.push_str(&format!("  \u{2022}  Likes {}  Dislikes {}", short_num(v.likes), short_num(v.dislikes))),
            (None, Some(l)) => byline.push_str(&format!("  \u{2022}  Likes {l}")),
            _ => {}
        }
        ui::text(p, &byline, 96.0, 842.0, 30, ui::DIM, ui::ALIGN_LEFT, false);
        if let Some(w) = &ex.watch {
            if let Some(i) = yt_core::watch::chapter_at(&w.chapters, st.position_ms) {
                let t = format!("\u{25B6} {}", ui::ellipsize(&w.chapters[i].title, 60));
                ui::text(p, &t, 1824.0, 842.0, 28, ui::WHITE, ui::ALIGN_RIGHT, false);
            }
        }
        let bar = Rect::new(96.0, 920.0, 1728.0, 10.0);
        ui::fill(p, bar, 5.0, [1.0, 1.0, 1.0, 0.25]);
        let frac = if st.duration_ms > 0 { (st.position_ms as f32 / st.duration_ms as f32).clamp(0.0, 1.0) } else { 0.0 };
        ui::fill(p, Rect::new(bar.x, bar.y, bar.w * frac, bar.h), 5.0, ui::accent());
        if st.duration_ms > 0 {
            let x_of = |ms: u64| bar.x + bar.w * (ms as f32 / st.duration_ms as f32).clamp(0.0, 1.0);
            if cx.views.store.settings.sb_markers {
                for seg in ex.segments.iter().filter(|s| s.is_ranged()) {
                    let c = yt_core::ext::sponsorblock::colour(&seg.category);
                    let col = [((c >> 16) & 0xff) as f32 / 255.0, ((c >> 8) & 0xff) as f32 / 255.0, (c & 0xff) as f32 / 255.0, 0.95];
                    let (x0, x1) = (x_of(seg.start_ms), x_of(seg.end_ms));
                    ui::fill(p, Rect::new(x0, bar.y, (x1 - x0).max(3.0), bar.h), 2.0, col);
                }
            }
            if let Some(w) = &ex.watch {
                for ch in w.chapters.iter().skip(1) {
                    ui::fill(p, Rect::new(x_of(ch.start_ms) - 2.0, bar.y - 4.0, 4.0, bar.h + 8.0), 1.0, [0.0, 0.0, 0.0, 0.8]);
                }
            }
            // Storyboard preview above the playhead while seeking.
            if cx.views.now_ms < self.preview_until {
                if let Some(level) = yt_core::storyboard::best_level(&ex.levels, 320) {
                    let fr = yt_core::storyboard::frame_at(level, st.position_ms, st.duration_ms);
                    let (tex, tw, th) = plx_ui::tex::resolve_wh_on(1, &fr.sheet_url, 0, 0, false);
                    let (pw, ph) = (384.0, 216.0);
                    let px = (bar.x + bar.w * frac - pw / 2.0).clamp(bar.x, bar.x + bar.w - pw);
                    let r = Rect::new(px, bar.y - ph - 40.0, pw, ph);
                    ui::fill(p, Rect::new(r.x - 4.0, r.y - 4.0, r.w + 8.0, r.h + 8.0), 10.0, ui::WHITE);
                    if tex != 0 && tw > 0.0 && th > 0.0 {
                        // Frame coordinates are in the sheet's own pixels; the texture size may differ.
                        // The sheet's real size: the last sheet of a video holds fewer rows than
                        // `rows`, so a full-grid size maps its frames to the wrong place.
                        let (sw, sh) = (tw, th);
                        // plx's uv is [u, v, width, height] (`Rect::cover_uv`), not two corners.
                        let uv = [fr.x as f32 / sw, fr.y as f32 / sh, fr.w as f32 / sw, fr.h as f32 / sh];
                        p.tex_uv(tex, uv, r, 8.0, ui::WHITE);
                    } else {
                        ui::fill(p, r, 8.0, [0.1, 0.1, 0.12, 1.0]);
                    }
                    ui::text(p, &ui::fmt_time(st.position_ms), r.x + r.w / 2.0, r.y + r.h - 44.0, 28, ui::WHITE, ui::ALIGN_CENTER, true);
                }
            }
        }
        ui::fill(p, Rect::new(bar.x + bar.w * frac - 12.0, bar.y - 7.0, 24.0, 24.0), 12.0, ui::WHITE);
        if st.live {
            let behind = st.live_edge_ms.saturating_sub(st.position_ms);
            if behind < 20_000 {
                ui::text(p, "\u{25CF} LIVE", 96.0, 950.0, 30, ui::accent(), ui::ALIGN_LEFT, true);
            } else {
                ui::text(p, &format!("\u{2212}{} behind live", ui::fmt_time(behind)), 96.0, 950.0, 30, ui::WHITE, ui::ALIGN_LEFT, false);
            }
        } else {
            ui::text(p, &ui::fmt_time(st.position_ms), 96.0, 950.0, 30, ui::WHITE, ui::ALIGN_LEFT, false);
        }
        let mut right = if st.live { String::new() } else { ui::fmt_time(st.duration_ms) };
        if cx.views.store.settings.show_ending_time && st.duration_ms > st.position_ms {
            right = format!("{right}   \u{2022}   ends in {}", ui::fmt_time(st.duration_ms - st.position_ms));
        }
        ui::text(p, &right, 1824.0, 950.0, 30, ui::WHITE, ui::ALIGN_RIGHT, false);
        // State + quality line
        let state = match st.phase {
            Phase::Paused => "\u{23F8}  Paused",
            Phase::Ended => "Ended",
            _ => "\u{25B6}",
        };
        let mut info = state.to_string();
        // Quality, always: the decoder's resolution, else the best the stream offers.
        let h = if st.height > 0 { st.height } else { st.quality_hint };
        if h > 0 {
            info.push_str(&format!("   {}p{}", h, if st.fps > 30 { st.fps.to_string() } else { String::new() }));
        } else if !matches!(st.phase, Phase::Failed(_)) {
            info.push_str("   \u{2014}p");
        }
        if (st.rate - 1.0).abs() > 0.01 && st.rate > 0.0 {
            info.push_str(&format!("   {}x", st.rate));
        }
        ui::text(p, &info, 96.0, 1010.0, 26, ui::DIM, ui::ALIGN_LEFT, false);
        ui::text(p, "\u{25BC} Options", 1824.0, 1010.0, 26, ui::FAINT, ui::ALIGN_RIGHT, false);
        if cx.views.store.settings.show_clock {
            ui::text(p, &common::clock(&cx.views), 1824.0, 28.0, 34, ui::WHITE, ui::ALIGN_RIGHT, false);
        }
        common::toast(p, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::VideoPlane
    }
}

/// Next video when one ends: the next playlist item (or the first when repeating the list),
/// else the watch page's autoplay pick.
fn next_video(cx: &Cx<'_, YtHost>, current: &str, repeat_list: bool) -> Option<(String, Option<String>)> {
    let ex = cx.views.player.extras.clone();
    let w = ex.watch.as_ref()?;
    if let Some(pl) = &w.playlist {
        let ids: Vec<&str> = pl.items.iter().map(|i| i.id.as_str()).collect();
        if let Some(i) = ids.iter().position(|v| *v == current) {
            if let Some(n) = ids.get(i + 1) {
                return Some((n.to_string(), Some(pl.id.clone())));
            }
            if repeat_list {
                return ids.first().map(|v| (v.to_string(), Some(pl.id.clone())));
            }
        }
    }
    let n = w.autoplay_next.as_ref()?;
    match &n.endpoint {
        Some(yt_core::model::Endpoint::Watch { video_id, playlist_id, .. }) if !video_id.is_empty() => {
            Some((video_id.clone(), playlist_id.clone()))
        }
        _ => Some((n.id.clone(), None)),
    }
}

fn short_num(n: u64) -> String {
    let f = |v: f64, unit: &str| {
        let s = format!("{v:.1}");
        format!("{}{unit}", s.trim_end_matches(".0"))
    };
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => f(n as f64 / 1e3, "K"),
        1_000_000..=999_999_999 => f(n as f64 / 1e6, "M"),
        _ => f(n as f64 / 1e9, "B"),
    }
}

/// Subtitles: centred lines on a dark backing, `bottom` px above the screen's bottom edge.
fn draw_caption(p: plx_ui::Painter, text: &str, size: i32, bottom: f32, ink: [f32; 4], bg: f32) {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let lh = size as f32 * 1.3;
    let top = 1080.0 - bottom - lh * lines.len() as f32;
    for (i, line) in lines.iter().enumerate() {
        let w = line.chars().count() as f32 * size as f32 * 0.52 + 32.0;
        let y = top + i as f32 * lh;
        if bg > 0.0 {
            ui::fill(p, Rect::new(960.0 - w / 2.0, y - 4.0, w, lh), 6.0, [0.0, 0.0, 0.0, bg]);
        }
        ui::text(p, line, 960.0, y, size, ink, ui::ALIGN_CENTER, bg < 0.3);
    }
}

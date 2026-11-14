//! Full-screen playback with an auto-hiding overlay (HUD).
//!
//! Keys: OK shows the HUD, or toggles pause when it is up; Left/Right seek (HUD shows the target);
//! Up opens the player options; Play/Pause/FF/RW remote keys work directly; BACK hides the HUD
//! first, then closes the player (which stops playback).

use super::common;
use crate::host::{YtArg, YtFx, YtHost};
use crate::player::Phase;
use crate::settings::{OkAction, PlaybackMode, PlayerStyle};
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
    /// Last Magic Remote pointer position (OSD button hover).
    pointer: Option<(f32, f32)>,
    /// Keyboard selection on the OSD buttons (see [`osd_button`] for the order).
    osd_sel: Option<usize>,
    /// Keyboard selection in the suggestions row under the controls (Down from the lower row).
    sugg_sel: Option<usize>,
    /// Screen dimming: the picture is dimmed now, and the last key press (views.now_ms clock).
    dimmed: bool,
    last_key_ms: u32,
    /// Pointer jitter guard (see the Pointer event).
    ptr_quiet: bool,
    ptr_anchor: Option<(f32, f32)>,
    /// "Leave with a double BACK": when the first BACK was pressed (views.now_ms clock).
    back_armed: Option<u32>,
    /// The release of that first BACK is swallowed too.
    back_swallow_up: bool,
    /// YouTube style: where each control was drawn last frame (pointer hits), and the seek bar.
    yt_rects: Vec<(usize, Rect)>,
    yt_bar: Rect,
    state: State,
}

impl PlayerScreen {
    pub fn new(entry: EntryId, video_id: String, playlist: Option<String>) -> Self {
        PlayerScreen {
            entry,
            state: State(video_id.clone()),
            video_id,
            playlist,
            hud: true,
            ended_handled: false,
            preview_until: 0,
            pointer: None,
            osd_sel: None,
            sugg_sel: None,
            dimmed: false,
            last_key_ms: 0,
            ptr_quiet: false,
            ptr_anchor: None,
            back_armed: None,
            back_swallow_up: false,
            yt_rects: Vec::new(),
            yt_bar: Rect::new(0.0, 0.0, 0.0, 0.0),
        }
    }

    fn show_hud(&mut self, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        self.hud = true;
        // 0 = never hide.
        let secs = cx.views.store.settings.hud_timeout_secs;
        fx.push(Fx::CancelTimer(T_HIDE));
        if secs > 0 {
            fx.push(Fx::Timer { id: T_HIDE, after_ms: secs * 1000 });
        }
        // One tick chain only: timers are not deduplicated, and every key / pointer move lands here.
        fx.push(Fx::CancelTimer(T_TICK));
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
        go_to(q[j as usize].clone(), self.playlist.clone(), fx);
    }

    fn seek(&mut self, dir: i64, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        let step = cx.views.store.settings.seek_step_secs.max(1) as i64 * 1000;
        fx.push(Fx::App(YtFx::SeekBy(dir * step)));
        self.preview_until = cx.views.now_ms + 1500;
        self.show_hud(cx, fx);
    }
}

/// OSD buttons in two rows, as SmartTube lays them out.
/// Above the seek bar: 0 play/pause, 1 previous, 2 next, 3 playback mode, 4 speed, 5 screen
/// dimming, 6 chapters, 7 comments, 8 search. Below it: 9 options, 10 channel, 11 like,
/// 12 dislike, 13 subtitles, 14 save to playlist, 15 subscribe, 16 video info, 17 SponsorBlock.
const OSD_N: usize = 18;
const OSD_ROW_B: usize = 9;
const OSD_PLAY: usize = 0;
const OSD_PREV: usize = 1;
const OSD_NEXT: usize = 2;
const OSD_REPEAT: usize = 3;
const OSD_SPEED: usize = 4;
const OSD_DIM: usize = 5;
const OSD_CHAPTERS: usize = 6;
const OSD_COMMENTS: usize = 7;
const OSD_SEARCH: usize = 8;
const OSD_OPTIONS: usize = 9;
const OSD_CHANNEL: usize = 10;
const OSD_LIKE: usize = 11;
const OSD_DISLIKE: usize = 12;
const OSD_CC: usize = 13;
const OSD_SAVE: usize = 14;
const OSD_SUBSCRIBE: usize = 15;
const OSD_INFO: usize = 16;
const OSD_SB: usize = 17;
/// Vertical layout of the controls.
const ROW_A_Y: f32 = 806.0;
const BAR_Y: f32 = 900.0;
const ROW_B_Y: f32 = 930.0;
const BTN: f32 = 64.0;
const BTN_PITCH: f32 = 84.0;

fn osd_button(i: usize) -> Rect {
    let (col, y) = if i < OSD_ROW_B { (i, ROW_A_Y) } else { (i - OSD_ROW_B, ROW_B_Y) };
    Rect::new(96.0 + col as f32 * BTN_PITCH, y, BTN, BTN)
}

/// The YouTube player style is chosen.
fn yt_style(cx: &Cx<'_, YtHost>) -> bool {
    cx.views.store.settings.player_style == PlayerStyle::YouTube
}

/// YouTube style: its one row of controls, left to right (the OSD ids they act as).
fn yt_order(cx: &Cx<'_, YtHost>) -> Vec<usize> {
    let child = cx.views.store.settings.child_mode;
    [OSD_CHANNEL, OSD_INFO, OSD_SUBSCRIBE, OSD_PREV, OSD_PLAY, OSD_NEXT, OSD_LIKE, OSD_DISLIKE, OSD_COMMENTS, OSD_SAVE, OSD_CC, OSD_OPTIONS]
        .into_iter()
        .filter(|&i| !(child && i == OSD_COMMENTS))
        .collect()
}

/// The button in the other row nearest to button `i` (Up / Down between the rows).
fn osd_other_row(i: usize) -> usize {
    if i < OSD_ROW_B {
        (i + OSD_ROW_B).min(OSD_N - 1)
    } else {
        (i - OSD_ROW_B).min(OSD_ROW_B - 1)
    }
}

/// The video's suggestions, flattened into one row (videos only).
fn suggestions(cx: &Cx<'_, YtHost>) -> Vec<yt_core::model::Item> {
    let Some(w) = cx.views.player.extras.watch.as_ref() else { return Vec::new() };
    let mut out: Vec<yt_core::model::Item> = Vec::new();
    for it in w.suggestions.iter().flat_map(|s| s.items.iter()) {
        if matches!(it.endpoint, Some(yt_core::model::Endpoint::Watch { .. })) && !out.iter().any(|o| o.id == it.id) && it.id != w.video_id {
            out.push(it.clone());
        }
    }
    out
}



thread_local! {
    /// Videos played in this run, oldest first (the "previous" button walks back through it).
    static PLAYED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    /// The next player was started by this one (end of video, next / previous), not opened.
    static MOVED_ON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The player moves on to another video (replacing itself).
fn go_to(video_id: String, playlist: Option<String>, fx: &mut Effects<'_, YtHost>) {
    MOVED_ON.with(|m| m.set(true));
    fx.push(Fx::Nav(NavOp::Replace(YtArg::Player { video_id, playlist })));
}

impl PlayerScreen {
    /// Next: the playlist's / YouTube's up-next video, else the next one in the opened list.
    fn next(&mut self, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        if let Some(v) = queued_next(cx, &self.video_id) {
            go_to(v, None, fx);
            return;
        }
        match next_video(cx, &self.video_id, false) {
            Some((v, pl)) => go_to(v, pl.or(self.playlist.clone()), fx),
            None => self.step_queue(1, cx, fx),
        }
    }

    /// Previous: the playlist's previous video, else the one played before this, else the
    /// previous one in the opened list.
    fn prev(&mut self, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        if let Some(pl) = cx.views.player.extras.watch.as_ref().and_then(|w| w.playlist.as_ref()) {
            if let Some(i) = pl.items.iter().position(|it| it.id == self.video_id).filter(|i| *i > 0) {
                let video_id = pl.items[i - 1].id.clone();
                PLAYED.with(|h| {
                    let mut h = h.borrow_mut();
                    if h.last() == Some(&self.video_id) {
                        h.pop();
                    }
                });
                go_to(video_id, Some(pl.id.clone()), fx);
                return;
            }
        }
        let before = PLAYED.with(|h| {
            let mut h = h.borrow_mut();
            if h.len() >= 2 && h.last() == Some(&self.video_id) {
                h.pop();
                h.last().cloned()
            } else {
                None
            }
        });
        match before {
            Some(video_id) => go_to(video_id, None, fx),
            None => self.step_queue(-1, cx, fx),
        }
    }

    /// The video ended: what comes next, per the playback mode.
    fn on_ended(&mut self, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        let store = cx.views.store;
        let s = &store.settings;
        let replay = YtFx::Replay { video_id: self.video_id.clone(), playlist: self.playlist.clone() };
        // A Short in a Shorts list loops (as on YouTube).
        if store.queue_shorts && s.loop_shorts && store.queue.contains(&self.video_id) {
            fx.push(Fx::App(replay));
            return;
        }
        // "Play next" goes first, unless this video repeats or the mode stops after each video.
        if !matches!(s.playback_mode, PlaybackMode::RepeatOne | PlaybackMode::PauseEach | PlaybackMode::CloseEach) {
            if let Some(v) = queued_next(cx, &self.video_id) {
                go_to(v, None, fx);
                return;
            }
        }
        let target = match s.playback_mode {
            PlaybackMode::RepeatOne => {
                fx.push(Fx::App(replay));
                return;
            }
            PlaybackMode::PauseEach => None,
            PlaybackMode::CloseEach => {
                fx.push(Fx::Nav(NavOp::Pop));
                return;
            }
            PlaybackMode::PlayNext => next_video(cx, &self.video_id, false),
            PlaybackMode::ListOnce => list_step(cx, &self.video_id, 1, false),
            PlaybackMode::ListRepeat => list_step(cx, &self.video_id, 1, true).or_else(|| next_video(cx, &self.video_id, true)),
            PlaybackMode::Shuffle => list_random(cx, &self.video_id).or_else(|| next_video(cx, &self.video_id, false)),
            PlaybackMode::Reverse => list_step(cx, &self.video_id, -1, false),
        };
        match target {
            Some((v, pl)) => go_to(v, pl.or(self.playlist.clone()), fx),
            None => self.show_hud(cx, fx),
        }
    }

    /// OSD button `i` pressed (pointer or OK); see [`osd_button`] for the order.
    fn press_osd(&mut self, i: usize, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        let st = cx.views.player;
        let pause_if_playing = |fx: &mut Effects<'_, YtHost>| {
            if st.phase == Phase::Playing {
                fx.push(Fx::App(YtFx::TogglePause));
            }
        };
        match i {
            OSD_PREV => self.prev(cx, fx),
            OSD_PLAY => fx.push(Fx::App(YtFx::TogglePause)),
            OSD_NEXT => self.next(cx, fx),
            OSD_REPEAT => {
                // Steps through every playback mode, naming the new one.
                let all = PlaybackMode::ALL;
                let cur = cx.views.store.settings.playback_mode;
                let i = all.iter().position(|r| *r == cur).unwrap_or(0);
                let next = all[(i + 1) % all.len()];
                fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| s.playback_mode = next))));
                fx.push(Fx::App(YtFx::Toast(next.label().into())));
            }
            // The list of speeds, as SmartTube's speed button opens.
            OSD_SPEED => {
                self.osd_sel = None;
                fx.push(Fx::Nav(NavOp::Present(YtArg::SpeedMenu)));
            }
            OSD_SEARCH if !cx.views.store.settings.child_mode => {
                pause_if_playing(fx);
                self.osd_sel = None;
                fx.push(Fx::Nav(NavOp::Push(YtArg::Search)));
            }
            OSD_SEARCH => {}
            OSD_CHANNEL if !st.channel_id.is_empty() => {
                pause_if_playing(fx);
                self.osd_sel = None;
                fx.push(Fx::Nav(NavOp::Push(YtArg::Feed(crate::store::FeedKey::Browse { id: st.channel_id.clone(), params: None }))));
            }
            OSD_CHANNEL => {}
            OSD_SAVE if !st.video_id.is_empty() => {
                self.osd_sel = None;
                fx.push(Fx::App(YtFx::LoadPlaylists(st.video_id.clone())));
                fx.push(Fx::Nav(NavOp::Present(YtArg::PlaylistPicker(st.video_id.clone(), st.title.clone()))));
            }
            OSD_SAVE => {}
            OSD_SUBSCRIBE if !st.channel_id.is_empty() => {
                let subscribed = st.extras.watch.as_ref().and_then(|w| w.subscribed).unwrap_or(false);
                fx.push(Fx::App(YtFx::Act(crate::host::UserAction::Subscribe { channel_id: st.channel_id.clone(), subscribe: !subscribed })));
            }
            OSD_SUBSCRIBE => {}
            OSD_INFO => {
                self.osd_sel = None;
                fx.push(Fx::Nav(NavOp::Push(YtArg::Info)));
            }
            OSD_DIM => {
                let s = &cx.views.store.settings;
                if s.screen_off_timeout_secs == 0 {
                    // At once: dim now (or lift a partial dim that is on). A partial dim stays on
                    // while the controls are used and carries over to the next video.
                    let on = !(self.dimmed || s.screen_off_armed);
                    let partial = s.screen_off_dimming < 100;
                    self.dimmed = on;
                    fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| s.screen_off_armed = on && partial))));
                    if on {
                        self.hud = false;
                        self.osd_sel = None;
                    }
                } else {
                    // A timeout: arm or disarm it; the dimming starts after that long without a key.
                    let on = !s.screen_off_armed;
                    self.dimmed = false;
                    self.last_key_ms = cx.views.now_ms;
                    fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| s.screen_off_armed = on))));
                    fx.push(Fx::App(YtFx::Toast(if on { "Screen dimming on".into() } else { "Screen dimming off".into() })));
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
            }
            OSD_SB => {
                let on = !cx.views.store.settings.sponsorblock;
                fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| s.sponsorblock = on))));
                fx.push(Fx::App(YtFx::Toast(if on { "SponsorBlock on".into() } else { "SponsorBlock off".into() })));
            }
            OSD_LIKE | OSD_DISLIKE if !st.video_id.is_empty() => {
                use yt_core::actions::Rating;
                let cur = st.extras.watch.as_ref().map(|w| w.like_status);
                let liked = matches!(cur, Some(yt_core::watch::LikeStatus::Like));
                let disliked = matches!(cur, Some(yt_core::watch::LikeStatus::Dislike));
                let r = match (i, liked, disliked) {
                    (OSD_LIKE, true, _) | (OSD_DISLIKE, _, true) => Rating::None,
                    (OSD_LIKE, ..) => Rating::Like,
                    _ => Rating::Dislike,
                };
                fx.push(Fx::App(YtFx::Act(crate::host::UserAction::Rate(st.video_id.clone(), r))));
            }
            OSD_LIKE | OSD_DISLIKE => {}
            OSD_CC => fx.push(Fx::App(YtFx::EditSettings(Box::new(|s| s.subtitles = !s.subtitles)))),
            OSD_CHAPTERS => {
                if has_chapters(cx) {
                    self.osd_sel = None;
                    fx.push(Fx::Nav(NavOp::Present(YtArg::Chapters)));
                } else {
                    fx.push(Fx::App(YtFx::Toast("No chapters".into())));
                }
            }
            OSD_COMMENTS => {
                if !st.video_id.is_empty() && !cx.views.store.settings.child_mode {
                    self.osd_sel = None;
                    fx.push(Fx::Nav(NavOp::Present(YtArg::Comments(st.video_id.clone()))));
                }
            }
            _ => {
                self.osd_sel = None;
                fx.push(Fx::Nav(NavOp::Present(YtArg::PlayerMenu)));
            }
        }
    }
}

impl Machine<YtHost> for PlayerScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                PLAYED.with(|h| {
                    let mut h = h.borrow_mut();
                    if h.last() != Some(&self.video_id) {
                        h.push(self.video_id.clone());
                        if h.len() > 100 {
                            h.remove(0);
                        }
                    }
                });
                fx.push(Fx::App(YtFx::Play { video_id: self.video_id.clone(), playlist: self.playlist.clone() }));
                self.show_hud(cx, fx);
                if yt_style(cx) {
                    self.osd_sel = Some(OSD_PLAY);
                }
                self.last_key_ms = cx.views.now_ms;
                let s = &cx.views.store.settings;
                self.dimmed = s.screen_off_armed && s.screen_off_timeout_secs == 0 && s.screen_off_dimming < 100;
                // A video that follows another starts without the controls unless asked for.
                if MOVED_ON.with(|m| m.replace(false)) && !cx.views.store.settings.hud_on_next_video {
                    self.hud = false;
                    // No button stays selected under hidden controls: the keys would go to it
                    // (OK toggling pause, Left / Right not seeking) instead of the player.
                    self.osd_sel = None;
                    fx.push(Fx::CancelTimer(T_HIDE));
                }
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
                if cx.views.player.phase == Phase::Playing && self.sugg_sel.is_none() {
                    self.hud = false;
                    self.osd_sel = None;
                    self.pointer = None;
                    fx.invalidate(plx_machine::present::Provenance::Timer(T_HIDE));
                }
                Handled::Yes
            }
            ScreenEvent::Timer(T_TICK) => {
                let p = cx.views.player;
                // Screen dimming after the timeout without a key press (while playing).
                {
                    let s = &cx.views.store.settings;
                    let quiet = cx.views.now_ms.wrapping_sub(self.last_key_ms) / 1000;
                    if s.screen_off_armed && s.screen_off_timeout_secs > 0 && !self.dimmed && p.phase == Phase::Playing && !self.hud
                        && quiet >= s.screen_off_timeout_secs
                    {
                        self.dimmed = true;
                        fx.invalidate(plx_machine::present::Provenance::Timer(T_TICK));
                    }
                }
                // End of video: repeat or autoplay according to settings.
                if p.phase != Phase::Ended {
                    self.ended_handled = false; // seeking back from the end ends it again later
                }
                if p.phase == Phase::Ended && !self.ended_handled {
                    self.ended_handled = true;
                    self.on_ended(cx, fx);
                }
                // Always keep ticking: the end of the video must be noticed with the controls
                // hidden too (it used to wait for a key press). Redraw only when something shows.
                fx.push(Fx::Timer { id: T_TICK, after_ms: 500 });
                if self.hud || p.phase != Phase::Playing {
                    fx.invalidate(plx_machine::present::Provenance::Timer(T_TICK));
                }
                Handled::Yes
            }
            // Magic Remote pointer: moving shows the controls; a click on the seek bar seeks there,
            // on "Options" opens the menu, anywhere else shows the controls or toggles pause.
            ScreenEvent::Input(InputEvent { kind: InputKind::Pointer { x, y, .. } | InputKind::Drag { x, y, .. }, .. }) => {
                // Jitter guard: after a key (BACK hiding the controls, say) the Magic Remote's
                // pointer shakes; it brings the controls back only once it really moves.
                if self.ptr_quiet {
                    match self.ptr_anchor {
                        None => {
                            self.ptr_anchor = Some((*x, *y));
                            return Handled::Yes;
                        }
                        Some((ax, ay)) if (x - ax).hypot(y - ay) < 80.0 => return Handled::Yes,
                        _ => {
                            self.ptr_quiet = false;
                            self.ptr_anchor = None;
                        }
                    }
                }
                self.pointer = Some((*x, *y));
                self.show_hud(cx, fx); // up, and kept up while the pointer moves
                Handled::Yes
            }
            // The Magic Remote's wheel: just bring up the controls (never the options menu).
            ScreenEvent::Input(InputEvent { kind: InputKind::Wheel { .. }, .. }) => {
                self.show_hud(cx, fx);
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Click { x, y, .. }, .. }) => {
                let st = cx.views.player;
                let (x, y) = (*x, *y);
                let yt = yt_style(cx);
                let hit = if yt {
                    self.yt_rects.iter().find(|(_, r)| r.contains(x, y)).map(|(i, _)| *i)
                } else {
                    (0..OSD_N).find(|i| osd_button(*i).contains(x, y))
                };
                let bar = if yt { self.yt_bar } else { Rect::new(96.0, 900.0, 1728.0, 8.0) };
                let on_bar = (bar.y - 24.0..=bar.y + 40.0).contains(&y) && (bar.x..=bar.x + bar.w).contains(&x);
                if let Some(i) = hit.filter(|_| self.hud) {
                    self.press_osd(i, cx, fx);
                    self.show_hud(cx, fx);
                } else if self.hud && on_bar && st.duration_ms > 0 && !st.live {
                    let to = ((x - bar.x) / bar.w).clamp(0.0, 1.0) as f64 * st.duration_ms as f64;
                    fx.push(Fx::App(YtFx::SeekTo(to as u64)));
                    self.preview_until = cx.views.now_ms + 1500;
                    self.show_hud(cx, fx);
                } else if !self.hud {
                    self.show_hud(cx, fx);
                } else {
                    fx.push(Fx::App(YtFx::TogglePause));
                    self.show_hud(cx, fx);
                }
                Handled::Yes
            }
            // The pointer went away: nothing stays highlighted where it last was.
            ScreenEvent::Input(InputEvent { kind: InputKind::PointerHidden, .. }) => {
                self.pointer = None;
                fx.invalidate(plx_machine::present::Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key, sym, wcode, edge, .. }, .. }) => {
                self.ptr_quiet = true;
                self.ptr_anchor = None;
                self.last_key_ms = cx.views.now_ms;
                // A full dim (or a timed one) is lifted by any key, which does nothing else; a
                // partial dim at once stays until its button is pressed again.
                if self.dimmed && *edge == Edge::Down {
                    let s = &cx.views.store.settings;
                    if s.screen_off_dimming >= 100 || s.screen_off_timeout_secs > 0 {
                        self.dimmed = false;
                        fx.invalidate(plx_machine::present::Provenance::Input);
                        return Handled::Yes;
                    }
                }
                // Keys take over from the pointer: its last hover no longer highlights a button.
                self.pointer = None;
                if *edge == Edge::Up {
                    if *key == Key::Back && std::mem::take(&mut self.back_swallow_up) {
                        return Handled::Yes;
                    }
                    return if *key == Key::Back && !self.hud { Handled::No } else { Handled::Yes };
                }
                if *edge == Edge::Repeat && *key == Key::Back && cx.views.store.settings.double_back_exit {
                    return Handled::Yes; // a held BACK is not a second press
                }
                // OSD buttons selected with the keys: Left/Right between them, OK presses, Down again
                // opens Options, Up / BACK leave the buttons.
                let transport = [WCODE_PLAY, WCODE_PAUSE, WCODE_PLAYPAUSE, WCODE_RW, WCODE_FF, WCODE_CH_UP, WCODE_CH_DOWN].contains(wcode)
                    || WCODE_STOP.contains(wcode);
                // The suggestions row: Left / Right through it, OK plays, Up / BACK go back.
                if let Some(s) = self.sugg_sel.filter(|_| !transport) {
                    let n = suggestions(cx).len();
                    match key {
                        Key::Left => self.sugg_sel = Some(s.saturating_sub(1)),
                        Key::Right => self.sugg_sel = Some((s + 1).min(n.saturating_sub(1))),
                        Key::Ok => {
                            if let Some(it) = suggestions(cx).get(s) {
                                fx.push(Fx::Nav(NavOp::Replace(YtArg::Player { video_id: it.id.clone(), playlist: None })));
                            }
                        }
                        Key::Up | Key::Back => {
                            self.sugg_sel = None;
                            self.osd_sel = Some(if yt_style(cx) { OSD_PLAY } else { OSD_ROW_B });
                        }
                        _ => {}
                    }
                    self.show_hud(cx, fx);
                    return Handled::Yes;
                }
                if let Some(sel) = self.osd_sel.filter(|_| !transport && self.hud && yt_style(cx)) {
                    // YouTube style: one row; Up to the seek bar, Down to the suggestions, BACK hides.
                    let order = yt_order(cx);
                    let at = order.iter().position(|&o| o == sel).unwrap_or(0);
                    match key {
                        Key::Left => self.osd_sel = Some(order[at.saturating_sub(1)]),
                        Key::Right => self.osd_sel = Some(order[(at + 1).min(order.len() - 1)]),
                        Key::Up => self.osd_sel = None,
                        Key::Down => {
                            if !suggestions(cx).is_empty() {
                                self.osd_sel = None;
                                self.sugg_sel = Some(0);
                            }
                        }
                        Key::Ok => self.press_osd(sel, cx, fx),
                        Key::Back => {
                            self.hud = false;
                            self.osd_sel = None;
                            fx.push(Fx::CancelTimer(T_HIDE));
                            fx.invalidate(plx_machine::present::Provenance::Input);
                            return Handled::Yes;
                        }
                        _ => {}
                    }
                    self.show_hud(cx, fx);
                    return Handled::Yes;
                }
                if let Some(sel) = self.osd_sel.filter(|_| !transport) {
                    let row = if sel < OSD_ROW_B { 0..OSD_ROW_B } else { OSD_ROW_B..OSD_N };
                    match key {
                        Key::Left => self.osd_sel = Some(sel.saturating_sub(1).max(row.start)),
                        Key::Right => self.osd_sel = Some((sel + 1).min(row.end - 1)),
                        Key::Down if sel < OSD_ROW_B => self.osd_sel = Some(osd_other_row(sel)),
                        Key::Down => {
                            // Below the lower row: the suggestions.
                            if !suggestions(cx).is_empty() {
                                self.osd_sel = None;
                                self.sugg_sel = Some(0);
                            }
                        }
                        Key::Ok => self.press_osd(sel, cx, fx),
                        Key::Up if sel >= OSD_ROW_B => self.osd_sel = Some(osd_other_row(sel)),
                        Key::Up | Key::Back => self.osd_sel = None,
                        _ => {}
                    }
                    self.show_hud(cx, fx);
                    return Handled::Yes;
                }
                match (key, *wcode) {
                    (Key::Back, _) => {
                        // BACK hides the controls while playing or paused (a paused video too).
                        let watching = matches!(cx.views.player.phase, Phase::Playing | Phase::Paused);
                        if self.hud && watching {
                            self.hud = false;
                            self.osd_sel = None;
                            fx.invalidate(plx_machine::present::Provenance::Input);
                            Handled::Yes
                        } else if watching && cx.views.store.settings.double_back_exit {
                            // Leave only with a second BACK within two seconds.
                            let now = cx.views.now_ms;
                            if self.back_armed.take().is_some_and(|t| now.wrapping_sub(t) < 2000) {
                                Handled::No
                            } else {
                                self.back_armed = Some(now);
                                self.back_swallow_up = true;
                                fx.push(Fx::App(YtFx::Toast(crate::i18n::tr("Press BACK again to leave").to_string())));
                                fx.invalidate(plx_machine::present::Provenance::Input);
                                Handled::Yes
                            }
                        } else {
                            Handled::No
                        }
                    }
                    (_, w) if WCODE_STOP.contains(&w) => {
                        fx.push(Fx::Nav(NavOp::Pop));
                        Handled::Yes
                    }
                    (Key::Ok, _) if !self.hud => {
                        // An offered SponsorBlock segment is skipped first.
                        if cx.views.store.sb_offered {
                            fx.push(Fx::App(YtFx::OkInPlayer));
                            fx.invalidate(plx_machine::present::Provenance::Input);
                            return Handled::Yes;
                        }
                        match cx.views.store.settings.ok_action {
                            OkAction::Controls => {
                                self.show_hud(cx, fx);
                                if yt_style(cx) {
                                    self.osd_sel = Some(OSD_PLAY);
                                }
                            }
                            OkAction::ControlsAndPause => {
                                if cx.views.player.phase == Phase::Playing {
                                    fx.push(Fx::App(YtFx::TogglePause));
                                }
                                self.show_hud(cx, fx);
                                if yt_style(cx) {
                                    self.osd_sel = Some(OSD_PLAY);
                                }
                            }
                            OkAction::Pause => {
                                fx.push(Fx::App(YtFx::TogglePause));
                                fx.invalidate(plx_machine::present::Provenance::Input);
                            }
                            OkAction::ToggleSpeed => {
                                fx.push(Fx::App(YtFx::ToggleSpeed));
                                fx.invalidate(plx_machine::present::Provenance::Input);
                            }
                        }
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
                            self.osd_sel = None;
                            fx.push(Fx::CancelTimer(T_HIDE));
                            fx.invalidate(plx_machine::present::Provenance::Input);
                        } else {
                            self.show_hud(cx, fx);
                        }
                        Handled::Yes
                    }
                    (_, _) if digit_key(*sym, *wcode).is_some() && cx.views.store.settings.seek_number_keys => {
                        // 0..9: jump to 0..90 % of the video.
                        let st = cx.views.player;
                        if st.duration_ms > 0 && !st.live {
                            let to = st.duration_ms * digit_key(*sym, *wcode).unwrap_or(0) / 10;
                            fx.push(Fx::App(YtFx::SeekTo(to)));
                            fx.push(Fx::App(YtFx::Toast(ui::fmt_time(to))));
                            self.preview_until = cx.views.now_ms + 1500;
                        }
                        self.show_hud(cx, fx);
                        Handled::Yes
                    }
                    (Key::Down, _) => {
                        // Down: the OSD buttons, starting on play/pause.
                        self.osd_sel = Some(OSD_PLAY);
                        self.show_hud(cx, fx);
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
            Phase::Idle | Phase::Resolving | Phase::Loading => Some(String::new()),
            Phase::Failed(e) => Some(e.clone()),
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
            if msg.is_empty() {
                // Loading / starting: a spinner, no words.
                ui::spinner(p, cx.views.now_ms, 960.0, 540.0);
            } else {
                // The failure: a headline, YouTube's (or our) reason under it in full; a members-only
                // video just says so.
                ui::text(p, "Can't play this video", 960.0, 440.0, 40, ui::WHITE, ui::ALIGN_CENTER, true);
                // YouTube's common refusals in the interface's language; anything else as it came.
                let msg = refusal_text(&msg).map_or(msg, |t| crate::i18n::tr(t).to_string());
                for (i, line) in ui::wrap_lines(f.measure, &msg, 1300.0, 28, false, 4).iter().enumerate() {
                    ui::text(p, line, 960.0, 510.0 + i as f32 * 42.0, 28, ui::DIM, ui::ALIGN_CENTER, false);
                }
            }
        }
        // Everything below (captions, OSD, toasts) is dimmed over HDR video.
        let _dim = ui::HdrDim::begin(ui::hdr_playing(st));
        let ex = st.extras.clone();
        // Screen dimming: black over the picture (and its subtitles) at the chosen amount.
        let dim_alpha = if self.dimmed { cx.views.store.settings.screen_off_dimming.min(100) as f32 / 100.0 } else { 0.0 };
        if dim_alpha >= 1.0 {
            ui::fill(p, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.0, 0.0, 0.0, 1.0]);
            return; // a switched-off screen shows nothing, not even subtitles
        }
        // (Not under the YouTube style's suggestions, which come up over the middle of the picture.)
        if cx.views.store.settings.subtitles && !(self.sugg_sel.is_some() && yt_style(cx)) {
            if let Some(cue) = yt_core::captions::cue_at(&ex.cues, st.position_ms) {
                // Above the title and both button rows (the YouTube style's single row is lower).
                let lift = match (self.hud, yt_style(cx)) {
                    (false, _) => 0.0,
                    (true, true) => 300.0,
                    (true, false) => 480.0,
                };
                let s = &cx.views.store.settings;
                SUB_STYLE.with(|c| c.set(match s.subtitle_style.as_str() { "shadow" => 1, "box" => 2, _ => 0 }));
                let face = match s.subtitle_font.as_str() { "sans" => 1, "serif" => 2, "mono" => 3, "roboto" => 4, _ => 0 };
                // Bold faces are 1..4, regular ones 5..8; -1 is the app font's regular weight.
                SUB_FACE.with(|c| c.set(match (face, s.subtitle_bold) { (0, true) => 0, (0, false) => -1, (f, true) => f, (f, false) => f + 4 }));
                let ink = match s.subtitle_color.as_str() {
                    "light gray" => [0.85, 0.85, 0.85, 1.0],
                    "gray" => [0.70, 0.70, 0.70, 1.0],
                    "dark gray" => [0.52, 0.52, 0.52, 1.0],
                    "yellow" => [1.0, 0.92, 0.25, 1.0],
                    "light yellow" => [1.0, 0.97, 0.70, 1.0],
                    "cyan" => [0.45, 0.95, 1.0, 1.0],
                    "green" => [0.55, 1.0, 0.55, 1.0],
                    "orange" => [1.0, 0.72, 0.35, 1.0],
                    "pink" => [1.0, 0.65, 0.85, 1.0],
                    _ => ui::WHITE,
                };
                if ui::comments_open() {
                    // On the shrunk video beside the comments.
                    draw_caption_at(p, &cue.text, (s.subtitle_size as f32 * 0.7) as i32, 1080.0 - 882.0 + s.subtitle_bottom_offset as f32 * 0.6, ink, 618.0);
                } else {
                    draw_caption(p, &cue.text, s.subtitle_size as i32, s.subtitle_bottom_offset as f32 + lift, ink, s.subtitle_background as f32 / 100.0);
                }
            }
        }
        if dim_alpha > 0.0 {
            ui::fill(p, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.0, 0.0, 0.0, dim_alpha]);
        }
        if ui::comments_open() {
            return;
        }
        if !self.hud && matches!(st.phase, Phase::Playing | Phase::Paused) {
            common::toast(p, cx);
            return;
        }
        if yt_style(cx) {
            self.draw_youtube(p, f.measure, cx);
            return;
        }
        let s = &cx.views.store.settings;
        // Browsing the suggestions: they take the screen (SmartTube scrolls them up under a dark
        // veil), the title stays on top.
        if let Some(sel) = self.sugg_sel {
            ui::fill(p, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.0, 0.0, 0.0, 0.82]);
            ui::text_raw(p, &f.measure.fit_line(&st.title, 1728.0, 48, true).to_string_lossy(), 96.0, 90.0, 48, ui::WHITE, ui::ALIGN_LEFT, true);
            ui::text_raw(p, &st.author, 96.0, 160.0, 28, ui::DIM, ui::ALIGN_LEFT, false);
            draw_suggestions(p, f.measure, &suggestions(cx), sel, 560.0);
            common::toast(p, cx);
            return;
        }
        // A gradient from the picture into the controls (SmartTube's player background).
        p.rect(Rect::new(0.0, 520.0, 1920.0, 260.0), 0.0, [0.0, 0.0, 0.0, 0.0], ui::dimmed([0.0, 0.0, 0.0, 0.82]), 0.0);
        ui::fill(p, Rect::new(0.0, 780.0, 1920.0, 300.0), 0.0, [0.0, 0.0, 0.0, 0.82]);
        // Title and the line under it: channel, views, age, likes, dislikes, subscribers.
        ui::text_raw(p, &f.measure.fit_line(&st.title, 1728.0, 50, true).to_string_lossy(), 96.0, 640.0, 50, ui::WHITE, ui::ALIGN_LEFT, true);
        let mut byline = st.author.clone();
        if let Some(w) = &ex.watch {
            if let Some(v) = &w.short_view_count_text {
                byline.push_str(&format!("  \u{2022}  {v}"));
            }
            if let Some(d) = w.published_text.as_deref().filter(|d| !d.is_empty()) {
                byline.push_str(&format!("  \u{2022}  {d}"));
            }
        }
        let votes: Option<(String, Option<String>)> = match (&ex.votes, ex.watch.as_ref().and_then(|w| w.like_count_text.clone())) {
            (Some(v), _) => Some((ui::full_count(v.likes), Some(ui::full_count(v.dislikes)))),
            (None, Some(l)) => Some((l, None)),
            _ => None,
        };
        let my = 712.0;
        let mut bx = 96.0 + ui::text_raw(p, &byline, 96.0, my, 28, ui::DIM, ui::ALIGN_LEFT, false);
        if let Some((likes, dislikes)) = votes {
            bx += ui::text(p, "  \u{2022}  ", bx, my, 28, ui::DIM, ui::ALIGN_LEFT, false);
            ui::icon::draw(p, ui::icon::Kind::ThumbUp, Rect::new(bx, my + 3.0, 26.0, 26.0), ui::DIM, false);
            bx += 34.0;
            bx += ui::text_raw(p, &likes, bx, my, 28, ui::DIM, ui::ALIGN_LEFT, false);
            if let Some(d) = dislikes {
                bx += ui::text(p, "  \u{2022}  ", bx, my, 28, ui::DIM, ui::ALIGN_LEFT, false);
                ui::icon::draw(p, ui::icon::Kind::ThumbDown, Rect::new(bx, my + 6.0, 26.0, 26.0), ui::DIM, false);
                bx += 34.0;
                bx += ui::text_raw(p, &d, bx, my, 28, ui::DIM, ui::ALIGN_LEFT, false);
            }
        }
        if let Some(subs) = ex.watch.as_ref().and_then(|w| w.subscriber_count_text.as_ref()) {
            ui::text_raw(p, &format!("  \u{2022}  {subs}"), bx, my, 28, ui::DIM, ui::ALIGN_LEFT, false);
        }
        // The chapter being played, over the right end of the bar.
        if let Some(w) = &ex.watch {
            if let Some(i) = yt_core::watch::chapter_at(&w.chapters, st.position_ms) {
                let t = format!("\u{25B6} {}", ui::ellipsize(&w.chapters[i].title, 60));
                ui::text_raw(p, &t, 1824.0, 712.0, 28, ui::WHITE, ui::ALIGN_RIGHT, false);
            }
        }
        // Seek bar: one piece per chapter with a small clear gap between.
        let bar = Rect::new(96.0, BAR_Y, 1728.0, 8.0);
        let frac = if st.duration_ms > 0 { (st.position_ms as f32 / st.duration_ms as f32).clamp(0.0, 1.0) } else { 0.0 };
        {
            let mut cuts: Vec<f32> = vec![0.0];
            if st.duration_ms > 0 {
                if let Some(w) = &ex.watch {
                    cuts.extend(w.chapters.iter().skip(1).map(|c| (c.start_ms as f32 / st.duration_ms as f32).clamp(0.0, 1.0)));
                }
            }
            cuts.push(1.0);
            let px = bar.x + bar.w * frac;
            for w in cuts.windows(2) {
                let gap = if w[1] < 1.0 { 4.0 } else { 0.0 };
                let (x0, x1) = (bar.x + bar.w * w[0], bar.x + bar.w * w[1] - gap);
                if x1 <= x0 {
                    continue;
                }
                ui::fill(p, Rect::new(x0, bar.y, x1 - x0, bar.h), 4.0, [1.0, 1.0, 1.0, 0.28]);
                let f1 = x1.min(px);
                if f1 > x0 {
                    ui::fill(p, Rect::new(x0, bar.y, f1 - x0, bar.h), 4.0, ui::accent());
                }
            }
        }
        if st.duration_ms > 0 {
            let x_of = |ms: u64| bar.x + bar.w * (ms as f32 / st.duration_ms as f32).clamp(0.0, 1.0);
            if s.sb_markers {
                for seg in ex.segments.iter().filter(|s| s.is_ranged()) {
                    let c = yt_core::ext::sponsorblock::colour(&seg.category);
                    let col = [((c >> 16) & 0xff) as f32 / 255.0, ((c >> 8) & 0xff) as f32 / 255.0, (c & 0xff) as f32 / 255.0, 0.95];
                    let (x0, x1) = (x_of(seg.start_ms), x_of(seg.end_ms));
                    ui::fill(p, Rect::new(x0, bar.y, (x1 - x0).max(3.0), bar.h), 2.0, col);
                }
            }
            // Storyboard preview above the playhead while seeking.
            if cx.views.now_ms < self.preview_until {
                storyboard_preview(p, st, bar, frac);
            }
        }
        ui::fill(p, Rect::new(bar.x + bar.w * frac - 11.0, bar.y - 7.0, 22.0, 22.0), 11.0, ui::WHITE);
        // Right of the upper row: quality / codec / HDR, and the date and time.
        let mut quality = String::new();
        let h = if st.height > 0 { st.height } else { st.quality_hint };
        if h > 0 {
            quality = format!("{}p{}", h, if st.fps > 30 { st.fps.to_string() } else { String::new() });
            if !st.hdr.is_empty() && !st.hdr.eq_ignore_ascii_case("none") {
                quality.push_str(&format!("  \u{2022}  {}", st.hdr));
            }
        }
        if (st.rate - 1.0).abs() > 0.01 && st.rate > 0.0 {
            quality.push_str(&format!("  \u{2022}  {}x", st.rate));
        }
        if st.phase == Phase::Ended {
            quality = format!("{}  {quality}", crate::i18n::tr("Ended"));
        }
        ui::text_raw(p, &quality, 1824.0, ROW_A_Y + 2.0, 26, ui::DIM, ui::ALIGN_RIGHT, false);
        if s.show_clock {
            // The date where it always was, beside the controls; the time in the top right corner
            // like on the video pages. Both leave with the controls, so nothing stays still on the
            // panel. A soft shade keeps the time legible over a bright picture.
            ui::text(p, &common::date_text(), 1824.0, ROW_A_Y + 34.0, 26, ui::DIM, ui::ALIGN_RIGHT, false);
            let time = common::clock(&cx.views);
            let w = 25.0 * time.chars().count() as f32 + 60.0;
            ui::fill(p, Rect::new(1852.0 - w, 22.0, w, 76.0), 24.0, [0.0, 0.0, 0.0, 0.45]);
            ui::text(p, &time, 1824.0, 34.0, 40, ui::WHITE, ui::ALIGN_RIGHT, true);
        }
        // Right of the lower row: remaining time, position / duration (or live).
        if st.live {
            let behind = st.live_edge_ms.saturating_sub(st.position_ms);
            if behind < 20_000 {
                ui::text_raw(p, &format!("\u{25CF} {}", crate::i18n::tr("LIVE")), 1824.0, ROW_B_Y + 16.0, 30, ui::accent(), ui::ALIGN_RIGHT, true);
            } else {
                ui::text_raw(p, &crate::i18n::tr("{} behind live").replace("{}", &ui::fmt_time(behind)), 1824.0, ROW_B_Y + 16.0, 28, ui::WHITE, ui::ALIGN_RIGHT, false);
            }
        } else {
            if let Some(t) = ending_text(st, s) {
                ui::text(p, &t, 1824.0, ROW_B_Y + 2.0, 26, ui::DIM, ui::ALIGN_RIGHT, false);
            }
            let pos = format!("{}  /  {}", ui::fmt_time(st.position_ms), ui::fmt_time(st.duration_ms));
            ui::text(p, &pos, 1824.0, ROW_B_Y + 34.0, 28, ui::WHITE, ui::ALIGN_RIGHT, false);
        }
        // The two rows of round buttons.
        let w = st.extras.watch.as_ref();
        let rating = w.map(|w| w.like_status);
        let liked = matches!(rating, Some(yt_core::watch::LikeStatus::Like));
        let disliked = matches!(rating, Some(yt_core::watch::LikeStatus::Dislike));
        let subscribed = w.and_then(|w| w.subscribed).unwrap_or(false);
        use ui::icon::Kind;
        let play = if matches!(st.phase, Phase::Paused | Phase::Ended) { Kind::Play } else { Kind::Pause };
        let repeat_icon = match s.playback_mode {
            PlaybackMode::RepeatOne => Kind::Repeat1,
            PlaybackMode::ListOnce | PlaybackMode::ListRepeat | PlaybackMode::Shuffle | PlaybackMode::Reverse => Kind::Repeat,
            PlaybackMode::PauseEach | PlaybackMode::CloseEach => Kind::PauseEnd,
            PlaybackMode::PlayNext => Kind::Autoplay,
        };
        let kinds = [
            play,
            Kind::Prev,
            Kind::Next,
            repeat_icon,
            Kind::Gauge,
            Kind::ScreenOff,
            Kind::List,
            Kind::Chat,
            Kind::Search,
            Kind::Gear,
            Kind::User,
            Kind::ThumbUp,
            Kind::ThumbDown,
            Kind::Cc,
            Kind::ListPlus,
            Kind::Bell,
            Kind::Info,
            Kind::Shield,
        ];
        let chapters = has_chapters(cx);
        for (i, k) in kinds.iter().enumerate() {
            let b = osd_button(i);
            let hot = self.pointer.is_some_and(|(x, y)| b.contains(x, y)) || self.osd_sel == Some(i);
            let off = (i == OSD_CHAPTERS && !chapters) || (i == OSD_SEARCH && s.child_mode) || (i == OSD_COMMENTS && s.child_mode);
            let on = match i {
                OSD_CC => s.subtitles,
                OSD_LIKE => liked,
                OSD_DISLIKE => disliked,
                OSD_SUBSCRIBE => subscribed,
                OSD_SB => s.sponsorblock,
                OSD_SPEED => (st.rate - 1.0).abs() > 0.01 && st.rate > 0.0,
                OSD_REPEAT => s.playback_mode != PlaybackMode::PlayNext,
                OSD_DIM => self.dimmed || s.screen_off_armed,
                _ => false,
            };
            if hot {
                ui::fill(p, b, b.h / 2.0, [1.0, 1.0, 1.0, 0.92]);
            }
            let ink = match (hot, off, on) {
                (true, _, _) => [0.05, 0.05, 0.07, 1.0],
                (false, true, _) => [1.0, 1.0, 1.0, 0.3],
                (false, false, true) => ui::accent(),
                _ => ui::WHITE,
            };
            let sz = if i <= OSD_NEXT { 30.0 } else { 34.0 };
            let ir = Rect::new(b.x + (b.w - sz) / 2.0, b.y + (b.h - sz) / 2.0, sz, sz);
            // The channel button shows the channel's picture when there is one.
            let avatar = (i == OSD_CHANNEL).then(|| w.and_then(|w| w.channel_avatar.clone())).flatten();
            let drawn = avatar.is_some_and(|url| {
                let (tex, _, _) = plx_ui::tex::resolve_wh_on(0, &url, 0, 0, false);
                if tex != 0 {
                    let a = Rect::new(b.x + 10.0, b.y + 10.0, b.w - 20.0, b.h - 20.0);
                    p.tex_uv(tex, [0.0, 0.0, 1.0, 1.0], a, a.w / 2.0, ui::dimmed(ui::WHITE));
                }
                tex != 0
            });
            if !drawn {
                let filled = i <= OSD_NEXT || (on && (i == OSD_LIKE || i == OSD_DISLIKE));
                ui::icon::draw(p, *k, ir, ink, filled);
            }
            // The focused button's name under it (a tooltip, as SmartTube shows).
            if self.osd_sel == Some(i) && i >= OSD_ROW_B {
                ui::text(p, osd_name(i), b.x + b.w / 2.0, b.y + b.h + 6.0, 22, ui::WHITE, ui::ALIGN_CENTER, false);
            } else if self.osd_sel == Some(i) {
                ui::text(p, osd_name(i), b.x + b.w / 2.0, b.y - 32.0, 22, ui::WHITE, ui::ALIGN_CENTER, false);
            }
        }
        // There is more below: the suggestions.
        if !suggestions(cx).is_empty() && self.osd_sel.is_some_and(|i| i >= OSD_ROW_B) {
            ui::text(p, &format!("\u{25BC}  {}", crate::i18n::tr("Suggestions")), 960.0, 1030.0, 24, ui::DIM, ui::ALIGN_CENTER, false);
        }
        common::toast_at(p, cx, 540.0); // above the title and the controls
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::VideoPlane
    }
}


/// The storyboard frame at the playhead, above the seek bar (while seeking).
fn storyboard_preview(p: plx_ui::Painter, st: &crate::host::PlayerView, bar: Rect, frac: f32) {
    let ex = &st.extras;
    if let Some(level) = yt_core::storyboard::best_level(&ex.levels, 320) {
        let fr = yt_core::storyboard::frame_at(level, st.position_ms, st.duration_ms);
        let (tex, tw, th) = plx_ui::tex::resolve_wh_on(1, &fr.sheet_url, 0, 0, false);
        let (pw, ph) = (384.0, 216.0);
        let px = (bar.x + bar.w * frac - pw / 2.0).clamp(bar.x, bar.x + bar.w - pw);
        let r = Rect::new(px, bar.y - ph - 40.0, pw, ph);
        ui::fill(p, Rect::new(r.x - 4.0, r.y - 4.0, r.w + 8.0, r.h + 8.0), 10.0, ui::WHITE);
        if tex != 0 && tw > 0.0 && th > 0.0 {
            // plx's uv is [u, v, width, height] (`Rect::cover_uv`), in the sheet's pixels.
            let uv = [fr.x as f32 / tw, fr.y as f32 / th, fr.w as f32 / tw, fr.h as f32 / th];
            p.tex_uv(tex, uv, r, 8.0, ui::dimmed(ui::WHITE));
        } else {
            ui::fill(p, r, 8.0, [0.1, 0.1, 0.12, 1.0]);
        }
        ui::text(p, &ui::fmt_time(st.position_ms), r.x + r.w / 2.0, r.y + r.h - 44.0, 28, ui::WHITE, ui::ALIGN_CENTER, true);
    }
}

// ---- The YouTube player style -------------------------------------------------------------------
// The official YouTube TV app's player (its 2025 design): the title and its line at the top left,
// the time and chapter over a thin white seek bar, one row of controls under it (channel,
// Description, Subscribe | previous, play, next | like, dislike, comments, save | captions,
// settings) and the suggestions peeking in at the bottom; Down brings them up.

/// Colours of the YouTube style: a control at rest and focused.
const YT_REST: [f32; 4] = [0.22, 0.22, 0.24, 0.62];
const YT_FOCUS: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const YT_FOCUS_INK: [f32; 4] = [0.06, 0.06, 0.07, 1.0];


impl PlayerScreen {
    fn draw_youtube(&mut self, p: plx_ui::Painter, m: &dyn plx_machine::machine::Measure, cx: &Cx<'_, YtHost>) {
        use ui::icon::Kind;
        let st = cx.views.player;
        let s = &cx.views.store.settings;
        let ex = &st.extras;
        let w = ex.watch.as_ref();
        let items = suggestions(cx);
        // Browsing the suggestions lifts the controls; the shelf comes up under them.
        let dy = if self.sugg_sel.is_some() { -330.0 } else { 0.0 };
        self.yt_rects.clear();

        // Shades: from the top into the picture, and from the picture into the controls.
        p.rect(Rect::new(0.0, 0.0, 1920.0, 360.0), 0.0, ui::dimmed([0.0, 0.0, 0.0, 0.72]), [0.0, 0.0, 0.0, 0.0], 0.0);
        if self.sugg_sel.is_some() {
            ui::fill(p, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.0, 0.0, 0.0, 0.78]);
        } else {
            p.rect(Rect::new(0.0, 560.0, 1920.0, 300.0), 0.0, [0.0, 0.0, 0.0, 0.0], ui::dimmed([0.0, 0.0, 0.0, 0.8]), 0.0);
            ui::fill(p, Rect::new(0.0, 860.0, 1920.0, 220.0), 0.0, [0.0, 0.0, 0.0, 0.8]);
        }

        // Title (up to two lines) and channel, views, age under it.
        let mut y = 64.0;
        for line in ui::wrap_lines(m, &st.title, 1300.0, 46, true, 2) {
            ui::text_raw(p, &line, 96.0, y, 46, ui::WHITE, ui::ALIGN_LEFT, true);
            y += 58.0;
        }
        let mut byline = st.author.clone();
        if let Some(w) = w {
            if let Some(v) = &w.short_view_count_text {
                byline.push_str(&format!(" \u{2022} {v}"));
            }
            if let Some(d) = w.published_text.as_deref().filter(|d| !d.is_empty()) {
                byline.push_str(&format!(" \u{2022} {d}"));
            }
        }
        ui::text_raw(p, &byline, 96.0, y + 8.0, 26, ui::DIM, ui::ALIGN_LEFT, false);
        if s.show_clock {
            ui::text(p, &common::clock(&cx.views), 1824.0, 64.0, 40, ui::WHITE, ui::ALIGN_RIGHT, true);
        }

        // Time and chapter over the bar, the length at its right end.
        let time_y = 788.0 + dy;
        let mut left = ui::fmt_time(st.position_ms);
        if let Some(w) = w {
            if let Some(i) = yt_core::watch::chapter_at(&w.chapters, st.position_ms) {
                left.push_str(&format!(" \u{2022} {}", ui::ellipsize(&w.chapters[i].title, 60)));
            }
        }
        if st.live {
            let behind = st.live_edge_ms.saturating_sub(st.position_ms);
            let r = Rect::new(1824.0 - 92.0, time_y - 2.0, 92.0, 36.0);
            ui::fill(p, r, 6.0, if behind < 20_000 { [0.8, 0.0, 0.0, 1.0] } else { YT_REST });
            ui::text(p, "LIVE", r.x + r.w / 2.0, r.y + 5.0, 22, ui::WHITE, ui::ALIGN_CENTER, true);
        } else {
            ui::text_raw(p, &left, 96.0, time_y, 26, ui::WHITE, ui::ALIGN_LEFT, false);
            ui::text(p, &ui::fmt_time(st.duration_ms), 1824.0, time_y, 26, ui::WHITE, ui::ALIGN_RIGHT, false);
        }

        // The seek bar: white, one piece per chapter; thicker with a knob while it has the focus.
        let bar_focus = self.osd_sel.is_none() && self.sugg_sel.is_none();
        let bh = if bar_focus { 8.0 } else { 5.0 };
        let bar = Rect::new(96.0, 836.0 + dy - bh / 2.0, 1728.0, bh);
        self.yt_bar = bar;
        let frac = if st.duration_ms > 0 { (st.position_ms as f32 / st.duration_ms as f32).clamp(0.0, 1.0) } else { 0.0 };
        let mut cuts: Vec<f32> = vec![0.0];
        if st.duration_ms > 0 {
            if let Some(w) = w {
                cuts.extend(w.chapters.iter().skip(1).map(|c| (c.start_ms as f32 / st.duration_ms as f32).clamp(0.0, 1.0)));
            }
        }
        cuts.push(1.0);
        let px = bar.x + bar.w * frac;
        for c in cuts.windows(2) {
            let gap = if c[1] < 1.0 { 4.0 } else { 0.0 };
            let (x0, x1) = (bar.x + bar.w * c[0], bar.x + bar.w * c[1] - gap);
            if x1 <= x0 {
                continue;
            }
            ui::fill(p, Rect::new(x0, bar.y, x1 - x0, bar.h), bar.h / 2.0, [1.0, 1.0, 1.0, 0.32]);
            let f1 = x1.min(px);
            if f1 > x0 {
                ui::fill(p, Rect::new(x0, bar.y, f1 - x0, bar.h), bar.h / 2.0, [1.0, 1.0, 1.0, 0.95]);
            }
        }
        if st.duration_ms > 0 {
            if s.sb_markers {
                let x_of = |ms: u64| bar.x + bar.w * (ms as f32 / st.duration_ms as f32).clamp(0.0, 1.0);
                for seg in ex.segments.iter().filter(|s| s.is_ranged()) {
                    let c = yt_core::ext::sponsorblock::colour(&seg.category);
                    let col = [((c >> 16) & 0xff) as f32 / 255.0, ((c >> 8) & 0xff) as f32 / 255.0, (c & 0xff) as f32 / 255.0, 0.95];
                    let (x0, x1) = (x_of(seg.start_ms), x_of(seg.end_ms));
                    ui::fill(p, Rect::new(x0, bar.y, (x1 - x0).max(3.0), bar.h), 2.0, col);
                }
            }
            if cx.views.now_ms < self.preview_until {
                storyboard_preview(p, st, bar, frac);
            }
        }
        if bar_focus {
            ui::fill(p, Rect::new(px - 12.0, bar.y + bar.h / 2.0 - 12.0, 24.0, 24.0), 12.0, ui::WHITE);
        }

        // The row of controls.
        let cy = 906.0 + dy; // its middle
        const B: f32 = 60.0;
        let hot = |i: usize, r: Rect, sel: Option<usize>, ptr: Option<(f32, f32)>| sel == Some(i) || ptr.is_some_and(|(x, y)| r.contains(x, y));
        let (sel, ptr) = (if self.sugg_sel.is_some() { None } else { self.osd_sel }, self.pointer);
        let rating = w.map(|w| w.like_status);
        let liked = matches!(rating, Some(yt_core::watch::LikeStatus::Like));
        let disliked = matches!(rating, Some(yt_core::watch::LikeStatus::Dislike));
        let subscribed = w.and_then(|w| w.subscribed).unwrap_or(false);
        let mut label: Option<(usize, Rect)> = None;

        // A round button on its own: grey at rest, white with a dark icon when focused.
        let mut round = |me: &mut Self, i: usize, r: Rect, kind: Kind, filled: bool, standalone: bool| {
            let h = hot(i, r, sel, ptr);
            if h {
                ui::fill(p, r, r.h / 2.0, YT_FOCUS);
                label = Some((i, r));
            } else if standalone {
                ui::fill(p, r, r.h / 2.0, YT_REST);
            }
            let sz = (r.h * 0.48).round();
            let ink = if h { YT_FOCUS_INK } else { ui::WHITE };
            ui::icon::draw(p, kind, Rect::new(r.x + (r.w - sz) / 2.0, r.y + (r.h - sz) / 2.0, sz, sz), ink, filled);
            me.yt_rects.push((i, r));
            h
        };

        // Left: the channel's picture, Description, Subscribe (a bell once subscribed).
        let mut x = 96.0;
        {
            let r = Rect::new(x, cy - B / 2.0, B, B);
            let h = hot(OSD_CHANNEL, r, sel, ptr);
            if h {
                ui::fill(p, Rect::new(r.x - 5.0, r.y - 5.0, r.w + 10.0, r.h + 10.0), (r.h + 10.0) / 2.0, YT_FOCUS);
            }
            let avatar = w.and_then(|w| w.channel_avatar.clone());
            let tex = avatar.map_or(0, |url| plx_ui::tex::resolve_wh_on(0, &url, 0, 0, false).0);
            if tex != 0 {
                p.tex_uv(tex, [0.0, 0.0, 1.0, 1.0], r, r.w / 2.0, ui::dimmed(ui::WHITE));
            } else {
                ui::fill(p, r, r.h / 2.0, YT_REST);
                let sz = 30.0;
                ui::icon::draw(p, Kind::User, Rect::new(r.x + (r.w - sz) / 2.0, r.y + (r.h - sz) / 2.0, sz, sz), ui::WHITE, false);
            }
            self.yt_rects.push((OSD_CHANNEL, r));
            x += B + 18.0;
        }
        let mut pill = |me: &mut Self, i: usize, x: f32, text: &str| -> f32 {
            let tw = std::ffi::CString::new(text).map_or(0.0, |c| m.width(&c, 24, true));
            let r = Rect::new(x, cy - B / 2.0, tw + 56.0, B);
            let h = hot(i, r, sel, ptr);
            ui::fill(p, r, r.h / 2.0, if h { YT_FOCUS } else { YT_REST });
            ui::text(p, text, r.x + r.w / 2.0, cy - 16.0, 24, if h { YT_FOCUS_INK } else { ui::WHITE }, ui::ALIGN_CENTER, true);
            me.yt_rects.push((i, r));
            r.w
        };
        x += pill(self, OSD_INFO, x, crate::i18n::tr("Description")) + 14.0;
        if subscribed {
            round(self, OSD_SUBSCRIBE, Rect::new(x, cy - B / 2.0, B, B), Kind::Bell, false, true);
        } else {
            pill(self, OSD_SUBSCRIBE, x, crate::i18n::tr("Subscribe"));
        }

        // Middle: previous, play / pause (large), next.
        const PLAY: f32 = 80.0;
        let play_kind = if matches!(st.phase, Phase::Paused | Phase::Ended) { Kind::Play } else { Kind::Pause };
        round(self, OSD_PREV, Rect::new(960.0 - PLAY / 2.0 - 18.0 - B, cy - B / 2.0, B, B), Kind::Prev, true, true);
        round(self, OSD_PLAY, Rect::new(960.0 - PLAY / 2.0, cy - PLAY / 2.0, PLAY, PLAY), play_kind, true, true);
        round(self, OSD_NEXT, Rect::new(960.0 + PLAY / 2.0 + 18.0, cy - B / 2.0, B, B), Kind::Next, true, true);

        // Right: two grouped pills, rating / comments / save and captions / settings.
        let child = s.child_mode;
        let group_a: Vec<(usize, Kind, bool)> = [
            (OSD_LIKE, Kind::ThumbUp, liked),
            (OSD_DISLIKE, Kind::ThumbDown, disliked),
            (OSD_COMMENTS, Kind::Chat, false),
            (OSD_SAVE, Kind::ListPlus, false),
        ]
        .into_iter()
        .filter(|(i, _, _)| !(child && *i == OSD_COMMENTS))
        .collect();
        let group_b = [(OSD_CC, Kind::Cc, false), (OSD_OPTIONS, Kind::Gear, false)];
        const PAD: f32 = 4.0;
        let gw = |n: usize| n as f32 * B + 2.0 * PAD;
        let bx = 1824.0 - gw(group_b.len());
        let ax = bx - 16.0 - gw(group_a.len());
        for (gx, group) in [(ax, &group_a[..]), (bx, &group_b[..])] {
            ui::fill(p, Rect::new(gx, cy - B / 2.0 - PAD, gw(group.len()), B + 2.0 * PAD), (B + 2.0 * PAD) / 2.0, YT_REST);
            for (k, (i, kind, on)) in group.iter().enumerate() {
                let r = Rect::new(gx + PAD + k as f32 * B, cy - B / 2.0, B, B);
                let h = round(self, *i, r, *kind, *on, false);
                // Captions on: a short red line under the icon, as YouTube marks it.
                if *i == OSD_CC && s.subtitles {
                    ui::fill(p, Rect::new(r.x + 18.0, r.y + r.h - 14.0, r.w - 36.0, 4.0), 2.0, if h { [0.8, 0.0, 0.0, 1.0] } else { [1.0, 0.2, 0.2, 1.0] });
                }
            }
        }
        // The focused icon button's name under it (the play button and the worded pills need none).
        if let Some((i, r)) = label.filter(|(i, _)| *i != OSD_PLAY && (![OSD_INFO, OSD_SUBSCRIBE].contains(i) || subscribed)) {
            ui::text(p, crate::i18n::tr(osd_name(i)), r.x + r.w / 2.0, cy + B / 2.0 + 12.0, 22, ui::WHITE, ui::ALIGN_CENTER, false);
        }

        // The suggestions: peeking in at the bottom, or brought up to browse.
        if !items.is_empty() {
            if let Some(sel) = self.sugg_sel {
                draw_suggestions_at(p, m, &items, sel, 1000.0 + dy, false);
            } else {
                const TW: f32 = 400.0;
                for (k, it) in items.iter().take(5).enumerate() {
                    let r = Rect::new(96.0 + k as f32 * (TW + 32.0), 1000.0, TW, TW * 9.0 / 16.0);
                    let url = it.thumbnail.clone().unwrap_or_default();
                    let (tex, _, _) = plx_ui::tex::resolve_wh_on(0, &url, 0, 0, false);
                    if tex != 0 {
                        p.tex_uv(tex, [0.0, 0.0, 1.0, 1.0], r, 14.0, ui::dimmed([1.0, 1.0, 1.0, 0.85]));
                    } else {
                        ui::fill(p, r, 14.0, [1.0, 1.0, 1.0, 0.08]);
                    }
                }
            }
        }
        common::toast_at(p, cx, 560.0 + dy);
    }
}

/// The remote's number keys: 0..9 (SDL symbols / LG key codes 48..57).
fn digit_key(sym: u32, wcode: u32) -> Option<u64> {
    [sym, wcode].into_iter().find(|k| (48..=57).contains(k)).map(|k| (k - 48) as u64)
}

/// The text after the duration under the seek bar ("Time in the controls"): the time left, or the
/// wall-clock time the video ends at the current speed; `None` when off (or live / finished).
pub fn ending_text(st: &crate::host::PlayerView, s: &crate::settings::Settings) -> Option<String> {
    use crate::settings::ControlsTime;
    if st.live || st.duration_ms <= st.position_ms {
        return None;
    }
    let left = st.duration_ms - st.position_ms;
    match s.controls_time {
        ControlsTime::Off => None,
        ControlsTime::Remaining => Some(crate::i18n::tr("{} left").replace("{}", &ui::fmt_time(left))),
        ControlsTime::Ending => {
            let rate = if st.rate > 0.0 { st.rate as f64 } else { 1.0 };
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
            let end = now + (left as f64 / 1000.0 / rate).round() as i64 + local_offset_secs();
            let (h, m) = ((end / 3600).rem_euclid(24), (end / 60).rem_euclid(60));
            let t = common::clock_text(h, m, s.time_24h);
            Some(crate::i18n::tr("Ends at {}").replace("{}", &t))
        }
    }
}

#[cfg(windows)]
fn local_offset_secs() -> i64 {
    crate::sim::local_offset_secs()
}

#[cfg(not(windows))]
fn local_offset_secs() -> i64 {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            0
        } else {
            tm.tm_gmtoff as i64
        }
    }
}

/// The list the current video is in: the playlist's items, else the list it was opened from.
fn current_list(cx: &Cx<'_, YtHost>, current: &str) -> Option<(Vec<String>, Option<String>)> {
    if let Some(pl) = cx.views.player.extras.watch.as_ref().and_then(|w| w.playlist.as_ref()) {
        if pl.items.iter().any(|i| i.id == current) {
            return Some((pl.items.iter().map(|i| i.id.clone()).collect(), Some(pl.id.clone())));
        }
    }
    let q = &cx.views.store.queue;
    q.iter().any(|v| v == current).then(|| (q.clone(), None))
}

/// The item `dir` places from `current` in its list; at an end, wraps (`wrap`) or stops.
fn list_step(cx: &Cx<'_, YtHost>, current: &str, dir: i32, wrap: bool) -> Option<(String, Option<String>)> {
    let (ids, pl) = current_list(cx, current)?;
    let i = ids.iter().position(|v| v == current)? as i32 + dir;
    let n = ids.len() as i32;
    let j = if (0..n).contains(&i) {
        i
    } else if wrap {
        i.rem_euclid(n)
    } else {
        return None;
    };
    Some((ids[j as usize].clone(), pl))
}

/// A random other item of the current list.
fn list_random(cx: &Cx<'_, YtHost>, current: &str) -> Option<(String, Option<String>)> {
    let (ids, pl) = current_list(cx, current)?;
    let others: Vec<&String> = ids.iter().filter(|v| *v != current).collect();
    if others.is_empty() {
        return None;
    }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let seed = (nanos as u64 ^ cx.views.now_ms as u64).wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    Some((others[(seed >> 33) as usize % others.len()].clone(), pl))
}

/// Next video when one ends: the next playlist item (or the first when repeating the list),
/// else the watch page's autoplay pick.
/// What each OSD button does, in words (its tooltip).
fn osd_name(i: usize) -> &'static str {
    crate::i18n::tr(match i {
        OSD_PLAY => "Play / pause",
        OSD_PREV => "Previous video",
        OSD_NEXT => "Next video",
        OSD_REPEAT => "Playback mode",
        OSD_SPEED => "Speed",
        OSD_DIM => "Screen dimming",
        OSD_CHAPTERS => "Chapters",
        OSD_COMMENTS => "Comments",
        OSD_SEARCH => "Search",
        OSD_OPTIONS => "Options",
        OSD_CHANNEL => "Channel",
        OSD_LIKE => "Like",
        OSD_DISLIKE => "Dislike",
        OSD_CC => "Subtitles",
        OSD_SAVE => "Save to playlist",
        OSD_SUBSCRIBE => "Subscribe",
        OSD_INFO => "Video info",
        _ => "SponsorBlock",
    })
}


/// The suggestions as a row of cards at `y`, the selected one lifted and lit.
fn draw_suggestions(p: plx_ui::Painter, m: &dyn plx_machine::machine::Measure, items: &[yt_core::model::Item], sel: usize, y: f32) {
    draw_suggestions_at(p, m, items, sel, y, true);
}

/// The suggestions row at `y`, with its heading above it when `heading`.
fn draw_suggestions_at(p: plx_ui::Painter, m: &dyn plx_machine::machine::Measure, items: &[yt_core::model::Item], sel: usize, y: f32, heading: bool) {
    const W: f32 = 400.0;
    const H: f32 = 225.0;
    const GAP: f32 = 32.0;
    if heading {
        ui::text(p, "Suggestions", 96.0, y - 70.0, 36, ui::WHITE, ui::ALIGN_LEFT, true);
    }
    let first = sel.saturating_sub(1);
    for (k, it) in items.iter().enumerate().skip(first).take(5) {
        let x = 96.0 + (k - first) as f32 * (W + GAP);
        let focused = k == sel;
        let grow = if focused { 12.0 } else { 0.0 };
        let r = Rect::new(x - grow, y - grow, W + 2.0 * grow, H + 2.0 * grow);
        let url = it.thumbnail.clone().unwrap_or_default();
        let (tex, _, _) = plx_ui::tex::resolve_wh_on(0, &url, 0, 0, false);
        if tex != 0 {
            p.tex_uv(tex, [0.0, 0.0, 1.0, 1.0], r, 14.0, ui::dimmed(ui::WHITE));
        } else {
            ui::fill(p, r, 14.0, [1.0, 1.0, 1.0, 0.08]);
        }
        if let Some(b) = &it.badge {
            let bw = 14.0 * b.chars().count() as f32 + 20.0;
            let br = Rect::new(r.x + r.w - bw - 10.0, r.y + r.h - 44.0, bw, 34.0);
            ui::fill(p, br, 6.0, [0.0, 0.0, 0.0, 0.78]);
            ui::text(p, b, br.x + bw / 2.0, br.y + 4.0, 22, ui::WHITE, ui::ALIGN_CENTER, true);
        }
        let panel = Rect::new(r.x, r.y + r.h + 6.0, r.w, 104.0);
        let (bg, ink, dim) = if focused {
            ([0.94, 0.94, 0.96, 1.0], [0.09, 0.09, 0.11, 1.0], [0.09, 0.09, 0.11, 0.66])
        } else {
            ([1.0, 1.0, 1.0, 0.08], [1.0, 1.0, 1.0, 0.92], ui::DIM)
        };
        ui::fill(p, panel, 14.0, bg);
        let tw = panel.w - 32.0;
        let t1 = m.fit_line(&it.title, tw, 24, true).to_string_lossy().into_owned();
        ui::text_raw(p, &t1, panel.x + 16.0, panel.y + 12.0, 24, ink, ui::ALIGN_LEFT, true);
        let detail = it.lines.join(" \u{2022} ");
        let d = m.fit_line(&detail, tw, 20, false).to_string_lossy().into_owned();
        ui::text_raw(p, &d, panel.x + 16.0, panel.y + 52.0, 20, dim, ui::ALIGN_LEFT, false);
    }
}

/// The playing video has chapters.
fn has_chapters(cx: &Cx<'_, YtHost>) -> bool {
    cx.views.player.extras.watch.as_ref().is_some_and(|w| !w.chapters.is_empty())
}

/// YouTube's reason for refusing a video (always English: requests are sent with hl=en) as one of
/// this app's translated sentences, when it is a common one.
fn refusal_text(reason: &str) -> Option<&'static str> {
    let r = reason.to_ascii_lowercase();
    let has = |k: &str| r.contains(k);
    Some(if has("available to this channel's members") || has("members-only") {
        "Only this channel's members can watch it."
    } else if has("not a bot") {
        "YouTube asks to confirm you're not a bot: try again later, or sign in."
    } else if has("confirm your age") || has("age-restricted") || has("inappropriate for some users") {
        "YouTube asks to confirm your age: sign in with an adult account."
    } else if has("in your country") {
        "This video is not available in your country."
    } else if has("private") {
        "This video is private."
    } else if has("removed") || has("terminated") || has("no longer available") {
        "This video has been removed."
    } else if has("live event will begin") || has("premieres in") || has("premiere will begin") {
        "This live stream or premiere has not started yet."
    } else if r.trim_end_matches('.') == "video unavailable" || r.trim_end_matches('.') == "this video is unavailable" {
        "This video is not available."
    } else {
        return None;
    })
}

/// The first video in "Play next" other than the one playing.
fn queued_next(cx: &Cx<'_, YtHost>, current: &str) -> Option<String> {
    cx.views.store.play_next.iter().map(|(v, _)| v).find(|v| v.as_str() != current).cloned()
}

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
    let n = w.autoplay_next.as_ref().filter(|n| n.id != current)?;
    match &n.endpoint {
        Some(yt_core::model::Endpoint::Watch { video_id, playlist_id, .. }) if !video_id.is_empty() => {
            Some((video_id.clone(), playlist_id.clone()))
        }
        _ => Some((n.id.clone(), None)),
    }
}

/// Subtitles: centred lines on a dark backing, `bottom` px above the screen's bottom edge.
fn draw_caption(p: plx_ui::Painter, text: &str, size: i32, bottom: f32, ink: [f32; 4], bg: f32) {
    let _ = bg;
    draw_caption_at(p, text, size, bottom, ink, 960.0);
}

/// Caption lines centred on `cx`, `bottom` px above the screen's bottom edge, outlined.
fn draw_caption_at(p: plx_ui::Painter, text: &str, size: i32, bottom: f32, ink: [f32; 4], cx: f32) {
    draw_caption_styled(p, text, size, bottom, ink, cx, SUB_STYLE.with(|s| s.get()));
}

thread_local! {
    /// The subtitle style for this frame's captions (0 outline, 1 shadow, 2 box).
    static SUB_STYLE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    /// The subtitle face (0 app font, 1..=4 the subtitle fonts).
    static SUB_FACE: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
}

fn draw_caption_styled(p: plx_ui::Painter, text: &str, size: i32, bottom: f32, ink: [f32; 4], cx: f32, style: u8) {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let lh = size as f32 * 1.3;
    let top = 1080.0 - bottom - lh * lines.len() as f32;
    let o = (size as f32 / 16.0).max(2.0);
    let outline = [0.0, 0.0, 0.0, 0.92];
    for (i, line) in lines.iter().enumerate() {
        let y = top + i as f32 * lh;
        match style {
            1 => {
                cap_text(p, line, cx + o * 1.2, y + o * 1.2, size, [0.0, 0.0, 0.0, 0.85]);
            }
            2 => {
                let w = line.chars().count() as f32 * size as f32 * 0.52 + 32.0;
                ui::fill(p, Rect::new(cx - w / 2.0, y - 4.0, w, lh), 6.0, [0.0, 0.0, 0.0, 0.72]);
            }
            _ => {
                for (dx, dy) in [(-o, 0.0), (o, 0.0), (0.0, -o), (0.0, o), (-o, -o), (o, -o), (-o, o), (o, o)] {
                    cap_text(p, line, cx + dx, y + dy, size, outline);
                }
            }
        }
        cap_text(p, line, cx, y, size, ink);
    }
}

/// One caption run in the chosen subtitle font.
fn cap_text(p: plx_ui::Painter, s: &str, x: f32, y: f32, size: i32, ink: [f32; 4]) {
    match SUB_FACE.with(|c| c.get()) {
        0 => {
            ui::text_raw(p, s, x, y, size, ink, ui::ALIGN_CENTER, true);
        }
        -1 => {
            ui::text_raw(p, s, x, y, size, ink, ui::ALIGN_CENTER, false);
        }
        face => {
            ui::text_face(p, s, x, y, size, ink, ui::ALIGN_CENTER, face);
        }
    }
}

#[allow(dead_code)]
fn draw_caption_old(p: plx_ui::Painter, text: &str, size: i32, bottom: f32, ink: [f32; 4], bg: f32) {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let lh = size as f32 * 1.3;
    let top = 1080.0 - bottom - lh * lines.len() as f32;
    for (i, line) in lines.iter().enumerate() {
        let w = line.chars().count() as f32 * size as f32 * 0.52 + 32.0;
        let y = top + i as f32 * lh;
        let _ = (w, bg);
        // A dark outline around the glyphs (no box): the text drawn offset in black all around.
        let o = (size as f32 / 16.0).max(2.0);
        let outline = [0.0, 0.0, 0.0, 0.92];
        for (dx, dy) in [(-o, 0.0), (o, 0.0), (0.0, -o), (0.0, o), (-o, -o), (o, -o), (-o, o), (o, o)] {
            ui::text_raw(p, line, 960.0 + dx, y + dy, size, outline, ui::ALIGN_CENTER, true);
        }
        ui::text_raw(p, line, 960.0, y, size, ink, ui::ALIGN_CENTER, true);
    }
}

#[cfg(test)]
mod refusal_tests {
    use super::refusal_text;

    #[test]
    fn common_refusals_are_translated_sentences() {
        assert_eq!(refusal_text("This video is available to this channel's members on level: X. Join this channel."), Some("Only this channel's members can watch it."));
        assert_eq!(refusal_text("Sign in to confirm your age"), Some("YouTube asks to confirm your age: sign in with an adult account."));
        assert_eq!(refusal_text("Video unavailable"), Some("This video is not available."));
        assert_eq!(refusal_text("This video is private"), Some("This video is private."));
        assert_eq!(refusal_text("no client could play abc: Profile(tv): ERROR boom"), None);
    }
}

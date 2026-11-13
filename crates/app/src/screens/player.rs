//! Full-screen playback with an auto-hiding overlay (HUD).
//!
//! Keys: OK shows the HUD, or toggles pause when it is up; Left/Right seek (HUD shows the target);
//! Up opens the player options; Play/Pause/FF/RW remote keys work directly; BACK hides the HUD
//! first, then closes the player (which stops playback).

use super::common;
use crate::host::{YtArg, YtFx, YtHost};
use crate::player::Phase;
use crate::settings::{OkAction, PlaybackMode};
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
    /// The speed the speed button returns to (it toggles between 1x and this).
    last_rate: f32,
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
            last_rate: 1.5,
            dimmed: false,
            last_key_ms: 0,
            ptr_quiet: false,
            ptr_anchor: None,
            back_armed: None,
            back_swallow_up: false,
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
            OSD_SPEED => {
                let rate = if st.rate > 0.0 { st.rate } else { 1.0 };
                let to = if (rate - 1.0).abs() > 0.01 {
                    self.last_rate = rate;
                    1.0
                } else {
                    self.last_rate
                };
                fx.push(Fx::App(YtFx::SetSpeed(to)));
                fx.push(Fx::App(YtFx::Toast(format!("{to}x"))));
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
                self.last_key_ms = cx.views.now_ms;
                let s = &cx.views.store.settings;
                self.dimmed = s.screen_off_armed && s.screen_off_timeout_secs == 0 && s.screen_off_dimming < 100;
                // A video that follows another starts without the controls unless asked for.
                if MOVED_ON.with(|m| m.replace(false)) && !cx.views.store.settings.hud_on_next_video {
                    self.hud = false;
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
                if let Some(i) = (0..OSD_N).find(|i| self.hud && osd_button(*i).contains(x, y)) {
                    self.press_osd(i, cx, fx);
                    self.show_hud(cx, fx);
                } else if self.hud && (890.0..=960.0).contains(&y) && (96.0..=1824.0).contains(&x) && st.duration_ms > 0 && !st.live {
                    let to = ((x - 96.0) / 1728.0).clamp(0.0, 1.0) as f64 * st.duration_ms as f64;
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
                            self.osd_sel = Some(OSD_ROW_B);
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
                            OkAction::Controls => self.show_hud(cx, fx),
                            OkAction::ControlsAndPause => {
                                if cx.views.player.phase == Phase::Playing {
                                    fx.push(Fx::App(YtFx::TogglePause));
                                }
                                self.show_hud(cx, fx);
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
            if msg.is_empty() {
                // Loading / starting: a spinner, no words.
                let n = 10;
                let head = (cx.views.now_ms / 80) as usize % n;
                for i in 0..n {
                    let a = i as f32 / n as f32 * std::f32::consts::TAU;
                    let (x, y) = (960.0 + 40.0 * a.sin(), 540.0 - 40.0 * a.cos());
                    let age = (head + n - i) % n;
                    let alpha = 0.15 + 0.8 * (1.0 - age as f32 / n as f32);
                    ui::fill(p, Rect::new(x - 6.0, y - 6.0, 12.0, 12.0), 6.0, [1.0, 1.0, 1.0, alpha]);
                }
                ui::animating();
            } else {
                ui::text(p, &msg, 960.0, 510.0, 36, ui::DIM, ui::ALIGN_CENTER, false);
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
        if cx.views.store.settings.subtitles {
            if let Some(cue) = yt_core::captions::cue_at(&ex.cues, st.position_ms) {
                let lift = if self.hud { 480.0 } else { 0.0 }; // above the title and both button rows
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
        let s = &cx.views.store.settings;
        // Browsing the suggestions: they take the screen (SmartTube scrolls them up under a dark
        // veil), the title stays on top.
        if let Some(sel) = self.sugg_sel {
            ui::fill(p, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.0, 0.0, 0.0, 0.82]);
            ui::text(p, &ui::ellipsize(&st.title, 70), 96.0, 90.0, 48, ui::WHITE, ui::ALIGN_LEFT, true);
            ui::text(p, &st.author, 96.0, 160.0, 28, ui::DIM, ui::ALIGN_LEFT, false);
            draw_suggestions(p, f.measure, &suggestions(cx), sel, 560.0);
            common::toast(p, cx);
            return;
        }
        // A gradient from the picture into the controls (SmartTube's player background).
        p.rect(Rect::new(0.0, 520.0, 1920.0, 260.0), 0.0, [0.0, 0.0, 0.0, 0.0], ui::dimmed([0.0, 0.0, 0.0, 0.82]), 0.0);
        ui::fill(p, Rect::new(0.0, 780.0, 1920.0, 300.0), 0.0, [0.0, 0.0, 0.0, 0.82]);
        // Title and the line under it: channel, views, age, likes, dislikes, subscribers.
        ui::text(p, &ui::ellipsize(&st.title, 72), 96.0, 640.0, 50, ui::WHITE, ui::ALIGN_LEFT, true);
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
            (Some(v), _) => Some((short_num(v.likes), Some(short_num(v.dislikes)))),
            (None, Some(l)) => Some((l, None)),
            _ => None,
        };
        let my = 712.0;
        let mut bx = 96.0 + ui::text(p, &byline, 96.0, my, 28, ui::DIM, ui::ALIGN_LEFT, false);
        if let Some((likes, dislikes)) = votes {
            bx += ui::text(p, "  \u{2022}  ", bx, my, 28, ui::DIM, ui::ALIGN_LEFT, false);
            ui::icon::draw(p, ui::icon::Kind::ThumbUp, Rect::new(bx, my + 3.0, 26.0, 26.0), ui::DIM, false);
            bx += 34.0;
            bx += ui::text(p, &likes, bx, my, 28, ui::DIM, ui::ALIGN_LEFT, false);
            if let Some(d) = dislikes {
                bx += ui::text(p, "  \u{2022}  ", bx, my, 28, ui::DIM, ui::ALIGN_LEFT, false);
                ui::icon::draw(p, ui::icon::Kind::ThumbDown, Rect::new(bx, my + 6.0, 26.0, 26.0), ui::DIM, false);
                bx += 34.0;
                bx += ui::text(p, &d, bx, my, 28, ui::DIM, ui::ALIGN_LEFT, false);
            }
        }
        if let Some(subs) = ex.watch.as_ref().and_then(|w| w.subscriber_count_text.as_ref()) {
            ui::text(p, &format!("  \u{2022}  {subs}"), bx, my, 28, ui::DIM, ui::ALIGN_LEFT, false);
        }
        // The chapter being played, over the right end of the bar.
        if let Some(w) = &ex.watch {
            if let Some(i) = yt_core::watch::chapter_at(&w.chapters, st.position_ms) {
                let t = format!("\u{25B6} {}", ui::ellipsize(&w.chapters[i].title, 60));
                ui::text(p, &t, 1824.0, 712.0, 28, ui::WHITE, ui::ALIGN_RIGHT, false);
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
        ui::text(p, &quality, 1824.0, ROW_A_Y + 2.0, 26, ui::DIM, ui::ALIGN_RIGHT, false);
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
                ui::text(p, "\u{25CF} LIVE", 1824.0, ROW_B_Y + 16.0, 30, ui::accent(), ui::ALIGN_RIGHT, true);
            } else {
                ui::text(p, &format!("\u{2212}{} behind live", ui::fmt_time(behind)), 1824.0, ROW_B_Y + 16.0, 28, ui::WHITE, ui::ALIGN_RIGHT, false);
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
            let t = if s.time_24h {
                format!("{h:02}:{m:02}")
            } else {
                format!("{}:{m:02} {}", if h % 12 == 0 { 12 } else { h % 12 }, if h < 12 { "AM" } else { "PM" })
            };
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
    const W: f32 = 400.0;
    const H: f32 = 225.0;
    const GAP: f32 = 32.0;
    ui::text(p, "Suggestions", 96.0, y - 70.0, 36, ui::WHITE, ui::ALIGN_LEFT, true);
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
        ui::text(p, &t1, panel.x + 16.0, panel.y + 12.0, 24, ink, ui::ALIGN_LEFT, true);
        let detail = it.lines.join(" \u{2022} ");
        let d = m.fit_line(&detail, tw, 20, false).to_string_lossy().into_owned();
        ui::text(p, &d, panel.x + 16.0, panel.y + 52.0, 20, dim, ui::ALIGN_LEFT, false);
    }
}

/// The playing video has chapters.
fn has_chapters(cx: &Cx<'_, YtHost>) -> bool {
    cx.views.player.extras.watch.as_ref().is_some_and(|w| !w.chapters.is_empty())
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
            ui::text(p, s, x, y, size, ink, ui::ALIGN_CENTER, true);
        }
        -1 => {
            ui::text(p, s, x, y, size, ink, ui::ALIGN_CENTER, false);
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
            ui::text(p, line, 960.0 + dx, y + dy, size, outline, ui::ALIGN_CENTER, true);
        }
        ui::text(p, line, 960.0, y, size, ink, ui::ALIGN_CENTER, true);
    }
}

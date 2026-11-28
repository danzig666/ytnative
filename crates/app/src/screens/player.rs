//! Full-screen playback with an auto-hiding overlay (HUD).
//!
//! Keys: OK shows the HUD, or toggles pause when it is up; Left/Right seek (HUD shows the target);
//! Up opens the player options; Play/Pause/FF/RW remote keys work directly; BACK hides the HUD
//! first, then closes the player (which stops playback).

use super::common;
use crate::host::{YtArg, YtFx, YtHost};
use crate::play_queue::PlayQueue;
use crate::player::Phase;
use crate::settings::{KeyPair, OkAction, PlaybackMode, PlayerStyle};
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
/// OK held this long on a suggestion opens its menu (as on a card) instead of playing it.
const T_SUGG_HOLD: TimerId = TimerId(3);
const SUGG_HOLD_MS: u32 = 500;

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
    /// "Confirm seeking with OK": where OK will jump (ms).
    seek_target: Option<u64>,
    /// Last Magic Remote pointer position (OSD button hover).
    pointer: Option<(f32, f32)>,
    /// Keyboard selection on the OSD buttons (see [`osd_button`] for the order).
    osd_sel: Option<usize>,
    /// Keyboard selection in the rows under the controls (Down from the lower row): the column,
    /// and the row by its kind ([`rows`]: the playing playlist first, then the suggestions), so
    /// a row that appears or goes (the playlist loading) does not move the selection to another.
    sugg_sel: Option<usize>,
    sugg_kind: RowKind,
    /// Where the rows' cards were drawn last frame (pointer clicks): ((row, column), rect).
    row_rects: Vec<((RowKind, usize), Rect)>,
    /// OK went down on this suggestion: it plays on release, or its menu opens when held. `true`
    /// once the menu opened (the release then does nothing).
    sugg_press: Option<(usize, bool)>,
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
    /// SmartTube style: the lower row's button the focus last went up from to the seek bar, where
    /// Down from the bar goes back to. Up on the bar hides the controls, as it always did (Up
    /// shows them, Up again hides them).
    osd_mem: usize,
    /// SmartTube style: seeking from the bar shows only the bar and the times (and the storyboard
    /// frame), as SmartTube's compact mode does, until another key than the seek keys is pressed.
    compact: bool,
    /// SmartTube style: a seek key held down speeds up 1.5 times every second, as in SmartTube:
    /// (direction, when the step last grew, the step in ms). `seek_repeat`: this key is a repeat.
    seek_run: Option<(i64, u32, i64)>,
    seek_repeat: bool,
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
            seek_target: None,
            pointer: None,
            osd_sel: None,
            sugg_sel: None,
            sugg_kind: RowKind::Suggestions,
            row_rects: Vec::new(),
            sugg_press: None,
            dimmed: false,
            last_key_ms: 0,
            ptr_quiet: false,
            ptr_anchor: None,
            back_armed: None,
            back_swallow_up: false,
            yt_rects: Vec::new(),
            yt_bar: Rect::new(0.0, 0.0, 0.0, 0.0),
            osd_mem: OSD_ROW_B,
            compact: false,
            seek_run: None,
            seek_repeat: false,
        }
    }

    /// The "Video details" panel is shown: switched on, and not over the audio-only black
    /// screen. It stays up while it is on; it moves a little every minute
    /// (`video_details::drift`) so it does not stay on the same OLED pixels.
    fn details_shown(&self, cx: &Cx<'_, YtHost>) -> bool {
        cx.views.store.settings.video_details && !cx.views.player.audio_only
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

    /// Previous / next video of the list this one was started from, in play order.
    fn step_queue(&mut self, dir: i32, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        self.drop_seek_target();
        let Some(q) = cx.views.store.play_queue.as_ref().filter(|q| q.contains(&self.video_id)) else {
            fx.push(Fx::App(YtFx::Toast("No other videos in this list".into())));
            return;
        };
        match q.step(&self.video_id, dir, false) {
            Some(v) => go_to(v.clone(), q.playlist_id.clone(), fx),
            None => fx.push(Fx::App(YtFx::Toast(if dir < 0 { "This is the first video".into() } else { "This is the last video".into() }))),
        }
    }

    fn seek(&mut self, dir: i64, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        let mut step = cx.views.store.settings.seek_step_secs.max(1) as i64 * 1000;
        if !yt_style(cx) {
            step = self.held_step(dir, step, cx.views.now_ms);
            self.compact = true;
        }
        let st = cx.views.player;
        if cx.views.store.settings.seek_confirm && !st.live && st.duration_ms > 0 {
            // "Confirm seeking with OK": the target moves on the bar; OK jumps there.
            let from = self.seek_target.unwrap_or(st.position_ms) as i64;
            self.seek_target = Some((from + dir * step).clamp(0, st.duration_ms as i64) as u64);
            self.preview_until = u32::MAX;
            self.show_hud(cx, fx);
            return;
        }
        fx.push(Fx::App(YtFx::SeekBy(dir * step)));
        self.preview_until = cx.views.now_ms + 1500;
        self.show_hud(cx, fx);
    }

    /// The step of this seek: `base` for a new press; while the key is held (repeats in the same
    /// direction) it grows 1.5 times every second, as SmartTube's `calculateSeekIncrement` does.
    fn held_step(&mut self, dir: i64, base: i64, now: u32) -> i64 {
        match self.seek_run {
            Some((d, since, s)) if self.seek_repeat && d == dir => {
                if now.wrapping_sub(since) >= 1000 {
                    let s = s * 3 / 2;
                    self.seek_run = Some((dir, now, s));
                    s
                } else {
                    s
                }
            }
            _ => {
                self.seek_run = Some((dir, now, base));
                base
            }
        }
    }

    /// The seek target waiting for OK, put down (BACK, the controls hidden).
    fn drop_seek_target(&mut self) -> bool {
        self.preview_until = 0;
        self.seek_target.take().is_some()
    }

    /// CH+ / CH- or fast-forward / rewind, as the settings have them (`dir`: -1 back, 1 on).
    fn key_pair(&mut self, what: KeyPair, dir: i32, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        match what {
            KeyPair::Video => self.step_queue(dir, cx, fx),
            KeyPair::Seek => self.seek(dir as i64, cx, fx),
            KeyPair::Speed => {
                fx.push(Fx::App(YtFx::SetSpeed(super::menu::step_speed(cx.views.player.rate, dir))));
                self.show_hud(cx, fx);
            }
            KeyPair::Chapter => {
                let st = cx.views.player;
                let Some(w) = st.extras.watch.as_ref().filter(|w| !w.chapters.is_empty()) else {
                    return self.seek(dir as i64, cx, fx);
                };
                let pos = st.position_ms;
                let at = yt_core::watch::chapter_at(&w.chapters, pos).unwrap_or(0);
                let to = if dir > 0 {
                    w.chapters.get(at + 1).map(|c| c.start_ms)
                } else if pos.saturating_sub(w.chapters[at].start_ms) > 3000 {
                    // Back within a chapter: its start first, as players do.
                    Some(w.chapters[at].start_ms)
                } else {
                    Some(w.chapters[at.saturating_sub(1)].start_ms)
                };
                if let Some(to) = to {
                    fx.push(Fx::App(YtFx::SeekTo(to)));
                    self.preview_until = cx.views.now_ms + 1500;
                    self.show_hud(cx, fx);
                }
            }
        }
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
    /// Next: in a playlist, its next video as the playback mode orders it; else YouTube's up-next
    /// video, else the next one in the opened list.
    fn next(&mut self, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        if let Some(v) = queued_next(cx, &self.video_id) {
            fx.push(Fx::App(YtFx::QueueResume(self.video_id.clone())));
            go_to(v, None, fx);
            return;
        }
        if playing_list(cx, &self.video_id).is_some() || resuming(cx, &self.video_id) {
            // The modes that stop or repeat after a video still move on with Next.
            let mode = match cx.views.store.settings.playback_mode {
                PlaybackMode::RepeatOne | PlaybackMode::PauseEach | PlaybackMode::CloseEach => PlaybackMode::PlayNext,
                m => m,
            };
            match self.decide(cx, mode, false) {
                crate::next_up::EndAction::Stay => fx.push(Fx::App(YtFx::Toast("This is the last video".into()))),
                a => self.carry_out(a, fx),
            }
            return;
        }
        match next_video(cx, &self.video_id, false) {
            Some((v, pl)) => go_to(v, pl, fx),
            None => self.step_queue(1, cx, fx),
        }
    }

    /// Previous: in a playlist, the video before this one in play order (shuffled, the shuffled
    /// order; Reverse, the next one in the list); else the watch page's playlist's previous video,
    /// else the one played before this, else the previous one in the opened list.
    fn prev(&mut self, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        if let Some(q) = playing_list(cx, &self.video_id) {
            let mode = cx.views.store.settings.playback_mode;
            let dir = if mode == PlaybackMode::Reverse { 1 } else { -1 };
            match q.step(&self.video_id, dir, mode == PlaybackMode::ListRepeat && q.complete()) {
                Some(v) => go_to(v.clone(), q.playlist_id.clone(), fx),
                None => fx.push(Fx::App(YtFx::Toast("This is the first video".into()))),
            }
            return;
        }
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

    /// What follows this video in playback mode `mode` (`next_up`).
    fn decide(&self, cx: &Cx<'_, YtHost>, mode: PlaybackMode, loop_short: bool) -> crate::next_up::EndAction {
        let store = cx.views.store;
        let play_next: Vec<String> = store.play_next.iter().map(|v| v.video_id.clone()).collect();
        let w = cx.views.player.extras.watch.as_ref();
        let yt_items: Option<(String, Vec<String>)> = w.and_then(|w| w.playlist.as_ref()).map(|pl| (pl.id.clone(), pl.items.iter().map(|i| i.id.clone()).collect()));
        let c = crate::next_up::EndContext {
            current: &self.video_id,
            mode,
            loop_short,
            play_next: &play_next,
            queue: store.play_queue.as_ref(),
            yt_playlist: yt_items.as_ref().map(|(id, v)| (id.as_str(), v.as_slice())),
            autoplay: w.and_then(autoplay_pick),
        };
        crate::next_up::on_ended(&c, crate::play_queue::seed(cx.views.now_ms))
    }

    /// Carries out a decision of [`PlayerScreen::decide`] other than staying.
    fn carry_out(&mut self, a: crate::next_up::EndAction, fx: &mut Effects<'_, YtHost>) {
        use crate::next_up::EndAction;
        match a {
            EndAction::Replay => fx.push(Fx::App(YtFx::Replay { video_id: self.video_id.clone(), playlist: self.playlist.clone() })),
            EndAction::Queued(v) => {
                // The playlist goes on from this video once the queued ones are done.
                fx.push(Fx::App(YtFx::QueueResume(self.video_id.clone())));
                go_to(v, None, fx);
            }
            EndAction::Go(v, pl) => go_to(v, pl, fx),
            EndAction::Shuffled(order, v, pl) => {
                fx.push(Fx::App(YtFx::SetQueueOrder(order)));
                go_to(v, pl, fx);
            }
            EndAction::Close => fx.push(Fx::Nav(NavOp::Pop)),
            EndAction::Stay => {}
        }
    }

    /// The video ended: what comes next, per the playback mode.
    fn on_ended(&mut self, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        let store = cx.views.store;
        let s = &store.settings;
        let loop_short = s.loop_shorts && store.play_queue.as_ref().is_some_and(|q| q.shorts && q.contains(&self.video_id));
        match self.decide(cx, s.playback_mode, loop_short) {
            crate::next_up::EndAction::Stay => self.show_hud(cx, fx),
            a => self.carry_out(a, fx),
        }
    }

    /// OK on the card in column `col` of row `row` ([`rows`]): the playlist's Shuffle switches;
    /// one of its videos plays within it (the playlist goes on from there); a suggestion plays,
    /// a mix or playlist suggestion as that playlist.
    fn open_tile(&mut self, kind: RowKind, col: usize, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        let kind = rows(cx, &self.video_id).contains(&kind).then_some(kind);
        match (kind, kind.and_then(|k| tile(k, cx, &self.video_id, col))) {
            (_, Some(Tile::Shuffle(on))) => fx.push(Fx::App(YtFx::ShuffleQueue(!on))),
            (Some(RowKind::Playlist), Some(Tile::Video(it))) => {
                let pl = playing_list(cx, &self.video_id).and_then(|q| q.playlist_id.clone());
                if it.id != self.video_id {
                    fx.push(Fx::Nav(NavOp::Replace(YtArg::Player { video_id: it.id, playlist: pl })));
                }
            }
            (_, Some(Tile::Video(it))) => match &it.endpoint {
                Some(yt_core::model::Endpoint::Watch { video_id, playlist_id: Some(pl), .. }) if !video_id.is_empty() => {
                    let title = if matches!(it.kind, yt_core::model::ItemKind::Mix | yt_core::model::ItemKind::Playlist) { it.title.clone() } else { String::new() };
                    fx.push(Fx::App(YtFx::SetQueue(PlayQueue::playlist(Some(pl.clone()), title, vec![video_id.clone()]))));
                    fx.push(Fx::Nav(NavOp::Replace(YtArg::Player { video_id: video_id.clone(), playlist: Some(pl.clone()) })));
                }
                // A video of its own: the playlist playing ends here (a later video of it would
                // otherwise pull the viewer back into the playlist they left).
                Some(yt_core::model::Endpoint::Watch { video_id, .. }) if !video_id.is_empty() => {
                    fx.push(Fx::App(YtFx::SetQueue(PlayQueue::list(vec![video_id.clone()], false))));
                    fx.push(Fx::Nav(NavOp::Replace(YtArg::Player { video_id: video_id.clone(), playlist: None })));
                }
                _ => {
                    fx.push(Fx::App(YtFx::SetQueue(PlayQueue::list(vec![it.id.clone()], false))));
                    fx.push(Fx::Nav(NavOp::Replace(YtArg::Player { video_id: it.id, playlist: None })));
                }
            },
            _ => {}
        }
    }

    /// The column a row is entered at: the playing video in the playlist, else the first card.
    fn entry_col(&self, kind: RowKind, cx: &Cx<'_, YtHost>) -> usize {
        match kind {
            RowKind::Playlist => playing_col(cx, &self.video_id).unwrap_or(0),
            RowKind::Suggestions => 0,
        }
    }

    /// Down from the controls: into the first row.
    fn enter_rows(&mut self, cx: &Cx<'_, YtHost>) -> bool {
        let Some(&first) = rows(cx, &self.video_id).first() else { return false };
        self.osd_sel = None;
        self.sugg_kind = first;
        self.sugg_sel = Some(self.entry_col(first, cx));
        true
    }

    /// Where the browsed row is in [`rows`] now (None: it went, the playlist ended).
    fn sugg_row(&self, cx: &Cx<'_, YtHost>) -> Option<usize> {
        rows(cx, &self.video_id).iter().position(|k| *k == self.sugg_kind)
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
                fx.push(Fx::Nav(NavOp::Present(YtArg::PlaylistPicker(playing_video(st)))));
            }
            OSD_SAVE => {}
            OSD_SUBSCRIBE if !st.channel_id.is_empty() => {
                // Subscribed: the bell's choices and Unsubscribe, as YouTube's button offers them
                // (only Unsubscribe when the page told no choices). Not subscribed: subscribe.
                if subscribed_now(cx) {
                    self.osd_sel = None;
                    fx.push(Fx::Nav(NavOp::Present(YtArg::BellMenu)));
                } else {
                    fx.push(Fx::App(YtFx::Act(crate::host::UserAction::Subscribe { channel_id: st.channel_id.clone(), subscribe: true })));
                }
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
                let cur = rating(cx);
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
            // The video's subtitle languages to pick from, as SmartTube's button opens; a video
            // without subtitles (or not loaded yet) keeps the on / off switch.
            OSD_CC if !st.captions.is_empty() => {
                self.osd_sel = None;
                fx.push(Fx::Nav(NavOp::Present(YtArg::SubtitleMenu)));
            }
            OSD_CC => fx.push(Fx::App(YtFx::EditSettings(Box::new(|s| s.subtitles = !s.subtitles)))),
            OSD_CHAPTERS => {
                if has_chapters(cx) {
                    self.osd_sel = None;
                    fx.push(Fx::Nav(NavOp::Present(YtArg::Chapters)));
                } else {
                    fx.push(Fx::App(YtFx::Toast("No chapters".into())));
                }
            }
            // A live stream with its chat offered: the comments button opens the live chat (its
            // comments stay in the Options menu).
            OSD_COMMENTS if super::live_chat::available(cx).is_some() => {
                self.osd_sel = None;
                fx.push(Fx::Nav(NavOp::Present(YtArg::LiveChat)));
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
                ui::set_player_dim(0, false);
                Handled::Yes
            }
            ScreenEvent::Uncover => {
                // Back from a menu (keys pressed there): the video details show again, the
                // dimming timeout starts over.
                self.last_key_ms = cx.views.now_ms;
                self.show_hud(cx, fx);
                Handled::Yes
            }
            // OK held on a suggestion: its menu (Play next, Save to playlist, ...).
            ScreenEvent::Timer(T_SUGG_HOLD) => {
                if let Some((s, false)) = self.sugg_press {
                    // Only a video has a menu: a held press on the Shuffle button still switches
                    // it on release.
                    let kind = self.sugg_row(cx).map(|_| self.sugg_kind);
                    if let Some(Tile::Video(it)) = kind.and_then(|k| tile(k, cx, &self.video_id, s)) {
                        self.sugg_press = Some((s, true));
                        fx.push(Fx::Nav(NavOp::Present(YtArg::SuggestionMenu(it.id.clone()))));
                    }
                }
                Handled::Yes
            }
            ScreenEvent::Timer(T_HIDE) => {
                match cx.views.player.phase {
                    Phase::Playing if self.sugg_sel.is_none() && self.seek_target.is_none() => {
                        self.hud = false;
                        self.osd_sel = None;
                        self.pointer = None;
                        fx.invalidate(plx_machine::present::Provenance::Timer(T_HIDE));
                    }
                    // Still loading (after a seek, say) or paused: the controls stay for now and
                    // hide once the video plays, instead of staying up for good.
                    Phase::Idle | Phase::Resolving | Phase::Loading | Phase::Paused if self.sugg_sel.is_none() => {
                        let secs = cx.views.store.settings.hud_timeout_secs.max(1);
                        fx.push(Fx::Timer { id: T_HIDE, after_ms: secs * 1000 });
                    }
                    _ => {}
                }
                Handled::Yes
            }
            ScreenEvent::Timer(T_TICK) => {
                let p = cx.views.player;
                // Screen dimming after the timeout without a key press (while playing).
                {
                    let s = &cx.views.store.settings;
                    // A key taken by a panel over the video (the live chat) counts as a key here,
                    // and lifts a dim a key lifts (the panel let it do nothing else).
                    if let Some(t) = ui::take_panel_key() {
                        self.last_key_ms = t;
                        if self.dimmed && ui::key_lifts_dim(s.screen_off_dimming, s.screen_off_timeout_secs) {
                            self.dimmed = false;
                            fx.invalidate(plx_machine::present::Provenance::Timer(T_TICK));
                        }
                    }
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
                if self.hud || p.phase != Phase::Playing || self.details_shown(cx) {
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
                self.compact = false; // the pointer wants the buttons
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
                // A card of the rows being browsed: it plays (the playlist's Shuffle switches).
                if self.sugg_sel.is_some() {
                    if let Some(&((kind, col), _)) = self.row_rects.iter().find(|(_, r)| r.contains(x, y)) {
                        self.sugg_kind = kind;
                        self.sugg_sel = Some(col);
                        self.open_tile(kind, col, cx, fx);
                        self.show_hud(cx, fx);
                        return Handled::Yes;
                    }
                }
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
                self.seek_repeat = *edge == Edge::Repeat;
                // The compact seek view lasts while seeking; another key (or the controls coming
                // back) brings the whole controls back, as SmartTube's full mode.
                let seek_key = matches!(key, Key::Left | Key::Right) || [WCODE_RW, WCODE_FF].contains(wcode);
                if *edge != Edge::Up && (!seek_key || !self.hud) {
                    self.compact = false;
                }
                // A full dim (or a timed one) is lifted by any key, which does nothing else; a
                // partial dim at once stays until its button is pressed again.
                if self.dimmed && *edge == Edge::Down {
                    let s = &cx.views.store.settings;
                    if ui::key_lifts_dim(s.screen_off_dimming, s.screen_off_timeout_secs) {
                        self.dimmed = false;
                        fx.invalidate(plx_machine::present::Provenance::Input);
                        return Handled::Yes;
                    }
                }
                // Keys take over from the pointer: its last hover no longer highlights a button.
                self.pointer = None;
                // "Confirm seeking with OK": OK jumps to the target, BACK puts it down.
                if let Some(to) = self.seek_target.filter(|_| *edge == Edge::Down) {
                    match key {
                        Key::Ok => {
                            self.drop_seek_target();
                            fx.push(Fx::App(YtFx::SeekTo(to)));
                            self.preview_until = cx.views.now_ms + 1500;
                            self.show_hud(cx, fx);
                            return Handled::Yes;
                        }
                        Key::Back => {
                            self.drop_seek_target();
                            fx.invalidate(plx_machine::present::Provenance::Input);
                            return Handled::Yes;
                        }
                        _ => {}
                    }
                }
                // OK let go on a suggestion: a short press plays it (a held one opened its menu).
                if *key == Key::Ok && *edge != Edge::Down {
                    if let Some((s, opened)) = self.sugg_press {
                        if *edge == Edge::Up {
                            self.sugg_press = None;
                            fx.push(Fx::CancelTimer(T_SUGG_HOLD));
                            if !opened {
                                self.open_tile(self.sugg_kind, s, cx, fx);
                            }
                        }
                        return Handled::Yes;
                    }
                }
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
                // The rows (the playlist, the suggestions): Left / Right through one, Up / Down
                // between them, OK plays, Up from the first / BACK go back.
                if let Some(s) = self.sugg_sel.filter(|_| !transport) {
                    let all = rows(cx, &self.video_id);
                    // The browsed row went (the playlist ended): back to the controls.
                    let Some(r) = self.sugg_row(cx) else {
                        self.sugg_sel = None;
                        self.osd_sel = Some(if yt_style(cx) { OSD_PLAY } else { OSD_ROW_B });
                        self.show_hud(cx, fx);
                        return Handled::Yes;
                    };
                    let n = row_len(all[r], cx, &self.video_id);
                    let s = s.min(n.saturating_sub(1));
                    self.sugg_sel = Some(s);
                    match key {
                        Key::Left => self.sugg_sel = Some(s.saturating_sub(1)),
                        Key::Right => self.sugg_sel = Some((s + 1).min(n.saturating_sub(1))),
                        Key::Down if *edge == Edge::Down && r + 1 < all.len() => {
                            self.sugg_kind = all[r + 1];
                            self.sugg_sel = Some(self.entry_col(all[r + 1], cx));
                        }
                        Key::Up if *edge == Edge::Down && r > 0 => {
                            self.sugg_kind = all[r - 1];
                            self.sugg_sel = Some(self.entry_col(all[r - 1], cx));
                        }
                        // Plays on release; held, opens the suggestion's menu instead.
                        Key::Ok if *edge == Edge::Down => {
                            if s < n {
                                self.sugg_press = Some((s, false));
                                fx.push(Fx::Timer { id: T_SUGG_HOLD, after_ms: SUGG_HOLD_MS });
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
                            self.enter_rows(cx);
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
                        // The seek bar lies between the two rows, as in SmartTube: Down from the
                        // upper row and Up from the lower one go to it.
                        Key::Down if sel < OSD_ROW_B => self.osd_sel = None,
                        Key::Down => {
                            // Below the lower row: the playlist, the suggestions.
                            self.enter_rows(cx);
                        }
                        Key::Ok => self.press_osd(sel, cx, fx),
                        Key::Up if sel < OSD_ROW_B => {
                            // Above the upper row: the controls hide (SmartTube's top edge).
                            self.hud = false;
                            self.osd_sel = None;
                            fx.push(Fx::CancelTimer(T_HIDE));
                            fx.invalidate(plx_machine::present::Provenance::Input);
                            return Handled::Yes;
                        }
                        Key::Up | Key::Back => {
                            if sel >= OSD_ROW_B {
                                self.osd_mem = sel;
                            }
                            self.osd_sel = None;
                        }
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
                    (_, WCODE_RW) if cx.views.store.settings.ff_keys != KeyPair::Seek => {
                        self.key_pair(cx.views.store.settings.ff_keys, -1, cx, fx);
                        Handled::Yes
                    }
                    (_, WCODE_FF) if cx.views.store.settings.ff_keys != KeyPair::Seek => {
                        self.key_pair(cx.views.store.settings.ff_keys, 1, cx, fx);
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
                        self.key_pair(cx.views.store.settings.channel_keys, -1, cx, fx);
                        Handled::Yes
                    }
                    (_, WCODE_CH_DOWN) => {
                        self.key_pair(cx.views.store.settings.channel_keys, 1, cx, fx);
                        Handled::Yes
                    }
                    (Key::Up, _) | (Key::Down, _) if cx.views.store.play_queue.as_ref().is_some_and(|q| q.shorts) => {
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
                    (Key::Down, _) if self.hud && !yt_style(cx) => {
                        // SmartTube style, on the seek bar: down to the lower row of buttons.
                        self.osd_sel = Some(self.osd_mem);
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
        // For a panel over the video (the live chat), drawn after this, to dim with the picture.
        {
            let s = &cx.views.store.settings;
            ui::set_player_dim(if self.dimmed { s.screen_off_dimming } else { 0 }, ui::key_lifts_dim(s.screen_off_dimming, s.screen_off_timeout_secs));
        }
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
        let no_picture = centre.is_some();
        if let Some(msg) = centre {
            ui::fill(p, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.0, 0.0, 0.0, 1.0]);
            if msg.is_empty() {
                // Loading / starting: a spinner, no words.
                ui::spinner(p, cx.views.now_ms, 960.0, 540.0);
            } else {
                // The failure: a headline, YouTube's (or our) reason under it in full; a members-only
                // video just says so.
                ui::text(p, "Can't play this video", 960.0, 440.0, 40, ui::WHITE, ui::ALIGN_CENTER, true);
                // Network trouble and YouTube's common refusals in the interface's language;
                // anything else as it came.
                let msg = match crate::net::classify(&msg) {
                    Some(n) => format!("{}. {}", crate::i18n::tr(n.title()), crate::i18n::tr(n.advice())),
                    None => refusal_text(&msg, cx.views.store.signed_in).map_or_else(|| crate::i18n::tr_parts(&msg), |t| crate::i18n::tr(t).to_string()),
                };
                for (i, line) in ui::wrap_lines(f.measure, &msg, 1300.0, 28, false, 9).iter().enumerate() {
                    ui::text_raw(p, line, 960.0, 510.0 + i as f32 * 42.0, 28, ui::DIM, ui::ALIGN_CENTER, false);
                }
            }
        }
        // Audio only: black where the picture would be, under everything else (dimming included).
        if st.audio_only && !no_picture {
            crate::audio_only::draw(p, f.measure, st, cx.views.now_ms);
        }
        // Rebuffering (the download fell behind): the picture stays, a spinner over it, and after a
        // moment the reason.
        if let (Some(since), false) = (st.buffering_since, no_picture) {
            ui::fill(p, Rect::new(910.0, 490.0, 100.0, 100.0), 50.0, [0.0, 0.0, 0.0, 0.45]);
            ui::spinner(p, cx.views.now_ms, 960.0, 540.0);
            if cx.views.now_ms.wrapping_sub(since) > 2000 {
                ui::text(p, "Slow connection", 960.0, 610.0, 30, ui::WHITE, ui::ALIGN_CENTER, true);
            }
        }
        // Everything below (captions, OSD, toasts) is dimmed over HDR video.
        let _dim = ui::HdrDim::begin(ui::hdr_playing(st));
        if !no_picture {
            // Not around the audio-only black screen: off there, it lights no bars and fetches no
            // storyboard sheets.
            crate::ambient::draw(p, st, cx.views.store.settings.ambient_mode && !st.audio_only, cx.views.now_ms);
        }
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
        // Under the screen dimming, like the captions.
        if self.details_shown(cx) && !ui::comments_open() {
            // Below the YouTube style's title while its controls are up.
            let top = if self.hud && yt_style(cx) && self.sugg_sel.is_none() { 240.0 } else { 40.0 };
            crate::video_details::draw(p, f.measure, st, top, crate::video_details::drift(cx.views.now_ms as u64));
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
        let mut preview: Option<(Rect, f32)> = None;
        // Browsing the suggestions: they take the screen (SmartTube scrolls them up under a dark
        // veil), the title stays on top.
        if self.sugg_sel.is_some() {
            self.draw_rows_full(p, f.measure, cx);
            return;
        }
        // Seeking from the bar: only the bar, the times and the storyboard frame (SmartTube's
        // compact mode); the title and the buttons come back with another key.
        let compact = self.compact && self.osd_sel.is_none();
        // A gradient from the picture into the controls (SmartTube's player background).
        p.rect(Rect::new(0.0, 520.0, 1920.0, 260.0), 0.0, [0.0, 0.0, 0.0, 0.0], ui::dimmed([0.0, 0.0, 0.0, 0.82]), 0.0);
        ui::fill(p, Rect::new(0.0, 780.0, 1920.0, 300.0), 0.0, [0.0, 0.0, 0.0, 0.82]);
        if !compact {
            // The playlist being played, on a line of its own above the title.
            if let Some(q) = playing_list(cx, &self.video_id) {
                list_eyebrow(p, f.measure, &list_line(q, &self.video_id), 96.0, 592.0, 1728.0, 28);
            }
            // Title and the line under it: channel, views, age, likes, dislikes, subscribers.
            ui::glide(p, f.measure, &st.title, 96.0, 640.0, 1728.0, 50, ui::WHITE, true, ui::GLIDE_PLAYER_TITLE);
            let votes: Option<(String, Option<String>)> = match (&ex.votes, ex.watch.as_ref().and_then(|w| w.like_count_text.clone())) {
                (Some(v), _) => Some((ui::full_count(v.likes), Some(ui::full_count(v.dislikes)))),
                (None, Some(l)) => Some((l, None)),
                _ => None,
            };
            let sep = "  \u{2022}  ";
            let views = ex.watch.as_ref().and_then(|w| w.short_view_count_text.clone());
            let age = ex.watch.as_ref().and_then(common::video_age);
            let subs = ex.watch.as_ref().and_then(|w| w.subscriber_count_text.clone());
            // The width the votes take (icons, counts and their separators).
            let sep_w = ui::text_width(f.measure, crate::i18n::tr(sep), 28, false);
            let votes_w = votes.as_ref().map_or(0.0, |(l, d)| {
                sep_w + 34.0 + ui::text_width(f.measure, l, 28, false) + d.as_ref().map_or(0.0, |d| sep_w + 34.0 + ui::text_width(f.measure, d, 28, false))
            });
            // Too long for the line: the subscribers go first, then the age, then the views.
            let (views, age, subs) = fit_byline(
                |v: Option<&str>, a: Option<&str>, sb: Option<&str>| {
                    let mut t = author_line(st, sep);
                    for x in [v, a].into_iter().flatten() {
                        t.push_str(sep);
                        t.push_str(x);
                    }
                    let mut w = ui::text_width(f.measure, &t, 28, false) + votes_w;
                    if let Some(x) = sb {
                        w += ui::text_width(f.measure, &format!("{sep}{x}"), 28, false);
                    }
                    w
                },
                1728.0,
                views,
                age,
                subs,
            );
            let mut byline = author_line(st, sep);
            for x in [&views, &age].into_iter().flatten() {
                byline.push_str(sep);
                byline.push_str(x);
            }
            let my = 712.0;
            let mut bx = 96.0 + ui::text_cut(p, f.measure, &byline, 96.0, my, 1728.0 - votes_w, 28, ui::DIM, ui::ALIGN_LEFT, false);
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
            if let Some(subs) = &subs {
                bx += ui::text_raw(p, &format!("{sep}{subs}"), bx, my, 28, ui::DIM, ui::ALIGN_LEFT, false);
            }
            // The chapter being played, at the right end of that line, in the room the line leaves.
            if let Some(w) = &ex.watch {
                if let Some(i) = yt_core::watch::chapter_at(&w.chapters, st.position_ms) {
                    let room = 1824.0 - (bx + 48.0);
                    if room > 120.0 {
                        let t = format!("\u{25B6} {}", w.chapters[i].title);
                        if ui::text_width(f.measure, &t, 28, false) <= room {
                            ui::text_raw(p, &t, 1824.0, my, 28, ui::WHITE, ui::ALIGN_RIGHT, false);
                        } else {
                            ui::glide(p, f.measure, &t, 1824.0 - room, my, room, 28, ui::WHITE, false, ui::GLIDE_PLAYER_CHAPTER);
                        }
                    }
                }
            }
        }
        // Seek bar: one piece per chapter with a small clear gap between. As SmartTube's: thicker
        // with a white knob at the playhead while it has the focus (no button selected: Left /
        // Right seek), thin with a small white dot otherwise (#30).
        let bar_focus = self.osd_sel.is_none();
        let bh = if bar_focus { 12.0 } else { 8.0 };
        let bar = Rect::new(96.0, BAR_Y + 4.0 - bh / 2.0, 1728.0, bh);
        let frac = if st.duration_ms > 0 { (self.seek_target.unwrap_or(st.position_ms) as f32 / st.duration_ms as f32).clamp(0.0, 1.0) } else { 0.0 };
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
                ui::fill(p, Rect::new(x0, bar.y, x1 - x0, bar.h), bar.h / 2.0, [1.0, 1.0, 1.0, 0.28]);
                let f1 = x1.min(px);
                if f1 > x0 {
                    ui::fill(p, Rect::new(x0, bar.y, f1 - x0, bar.h), bar.h / 2.0, ui::accent());
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
            // Storyboard preview above the playhead while seeking (drawn last, over the controls).
            if cx.views.now_ms < self.preview_until {
                preview = Some((bar, frac));
            }
        }
        // The knob: 24 px with the focus, otherwise a dot as tall as the bar, as SmartTube draws it.
        let kr = if bar_focus { 12.0 } else { bh / 2.0 };
        ui::fill(p, Rect::new(bar.x + bar.w * frac - kr, bar.y + bar.h / 2.0 - kr, kr * 2.0, kr * 2.0), kr, ui::WHITE);
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
        if !compact {
            ui::text_raw(p, &quality, 1824.0, ROW_A_Y + 2.0, 26, ui::DIM, ui::ALIGN_RIGHT, false);
        }
        if s.show_clock {
            // The date where it always was, beside the controls; the time in the top right corner
            // like on the video pages. Both leave with the controls, so nothing stays still on the
            // panel. A soft shade keeps the time legible over a bright picture.
            if !compact {
                ui::text(p, &common::date_text(), 1824.0, ROW_A_Y + 34.0, 26, ui::DIM, ui::ALIGN_RIGHT, false);
            }
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
        if !compact {
            // The two rows of round buttons.
            let w = st.extras.watch.as_ref();
            let rated = rating(cx);
            let liked = matches!(rated, Some(yt_core::watch::LikeStatus::Like));
            let disliked = matches!(rated, Some(yt_core::watch::LikeStatus::Dislike));
            let subscribed = subscribed_now(cx);
            let saved = in_playlist(cx);
            use ui::icon::Kind;
            let play = if matches!(st.phase, Phase::Paused | Phase::Ended) { Kind::Play } else { Kind::Pause };
            let repeat_icon = match s.playback_mode {
                PlaybackMode::RepeatOne => Kind::Repeat1,
                PlaybackMode::ListOnce => Kind::ListEnd,
                PlaybackMode::ListRepeat => Kind::Repeat,
                PlaybackMode::Shuffle => Kind::Shuffle,
                PlaybackMode::Reverse => Kind::ListReverse,
                PlaybackMode::PauseEach => Kind::PauseEnd,
                PlaybackMode::CloseEach => Kind::Leave,
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
                    OSD_SAVE => saved,
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
                    let filled = i <= OSD_NEXT || (on && (i == OSD_LIKE || i == OSD_DISLIKE || i == OSD_SAVE));
                    ui::icon::draw(p, *k, ir, ink, filled);
                }
                // The focused button's name under it (a tooltip, as SmartTube shows).
                let name = if i == OSD_COMMENTS && super::live_chat::available(cx).is_some() { crate::i18n::tr("Live chat").to_string() } else { osd_label(i, cx) };
                let name = name.as_str();
                if self.osd_sel == Some(i) && i >= OSD_ROW_B {
                    ui::text(p, name, b.x + b.w / 2.0, b.y + b.h + 6.0, 22, ui::WHITE, ui::ALIGN_CENTER, false);
                } else if self.osd_sel == Some(i) {
                    ui::text(p, name, b.x + b.w / 2.0, b.y - 32.0, 22, ui::WHITE, ui::ALIGN_CENTER, false);
                }
            }
            // There is more below: the suggestions.
            if let Some(&first) = rows(cx, &self.video_id).first().filter(|_| self.osd_sel.is_some_and(|i| i >= OSD_ROW_B)) {
                let below = match (first, playing_list(cx, &self.video_id)) {
                    (RowKind::Playlist, Some(q)) => list_line(q, &self.video_id),
                    _ => crate::i18n::tr("Suggestions").to_string(),
                };
                ui::text_raw(p, &format!("\u{25BC}  {below}"), 960.0, 1030.0, 24, ui::DIM, ui::ALIGN_CENTER, false);
            }
        }
        if let Some((bar, frac)) = preview {
            storyboard_preview(p, st, bar, frac);
        }
        common::toast_at(p, cx, 540.0); // above the title and the controls
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::VideoPlane
    }
}


/// The viewer's rating of the playing video: what they last pressed, else what YouTube said.
pub fn rating(cx: &Cx<'_, YtHost>) -> Option<yt_core::watch::LikeStatus> {
    let st = cx.views.player;
    cx.views.store.rated.get(&st.video_id).copied().or_else(|| st.extras.watch.as_ref().map(|w| w.like_status))
}

/// The playing video is in a playlist: one of the account's (what the playlist picker last showed,
/// else what YouTube said when the video opened) or one on this TV.
pub(crate) fn in_playlist(cx: &Cx<'_, YtHost>) -> bool {
    let st = cx.views.player;
    let store = cx.views.store;
    let youtube = store.playlist_options.get(&st.video_id).map(|l| l.iter().any(|o| o.contains)).or(st.extras.in_playlist).unwrap_or(false);
    youtube || store.local.lists.iter().any(|l| l.contains(&st.video_id))
}

/// Subscribed to the playing video's channel: what the viewer last pressed, else what YouTube said.
pub(crate) fn subscribed_now(cx: &Cx<'_, YtHost>) -> bool {
    let w = cx.views.player.extras.watch.as_ref();
    let pressed = w.and_then(|w| w.subscribe_actions.channel_ids.first().or(Some(&w.channel_id))).and_then(|id| cx.views.store.subscribed.get(id).copied());
    pressed.or_else(|| w.and_then(|w| w.subscribed)).unwrap_or(false)
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
        let mut preview: Option<(Rect, f32)> = None;
        let ex = &st.extras;
        let w = ex.watch.as_ref();
        // Browsing a row below the first: the rows take the screen.
        if self.sugg_sel.is_some() && self.sugg_row(cx).is_some_and(|r| r > 0) {
            self.yt_rects.clear();
            self.draw_rows_full(p, m, cx);
            return;
        }
        let all_rows = rows(cx, &self.video_id);
        // Browsing the first row lifts the controls; the row comes up under them.
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
        // The second line holds the rest of a long title, gliding when it does not fit.
        for line in ui::wrap_lines(m, &st.title, 1300.0, 46, true, 2) {
            ui::glide(p, m, &line, 96.0, y, 1300.0, 46, ui::WHITE, true, ui::GLIDE_PLAYER_TITLE);
            y += 58.0;
        }
        // Channel, views and age; too long for the room beside the clock: the age goes first,
        // then the views.
        let sep = " \u{2022} ";
        let (views, age, _) = fit_byline(
            |v: Option<&str>, a: Option<&str>, _| {
                let mut t = author_line(st, sep);
                for x in [v, a].into_iter().flatten() {
                    t.push_str(sep);
                    t.push_str(x);
                }
                ui::text_width(m, &t, 26, false)
            },
            1300.0,
            w.and_then(|w| w.short_view_count_text.clone()),
            w.and_then(common::video_age),
            None,
        );
        let mut byline = author_line(st, sep);
        for x in [&views, &age].into_iter().flatten() {
            byline.push_str(sep);
            byline.push_str(x);
        }
        ui::text_cut(p, m, &byline, 96.0, y + 8.0, 1300.0, 26, ui::DIM, ui::ALIGN_LEFT, false);
        // The playlist being played, on a line of its own under that.
        if let Some(q) = playing_list(cx, &self.video_id) {
            list_eyebrow(p, m, &list_line(q, &self.video_id), 96.0, y + 48.0, 1300.0, 26);
        }
        if s.show_clock {
            ui::text(p, &common::clock(&cx.views), 1824.0, 64.0, 40, ui::WHITE, ui::ALIGN_RIGHT, true);
        }

        // Time and chapter over the bar, the length at its right end.
        let time_y = 788.0 + dy;
        let left = ui::fmt_time(st.position_ms);
        let chapter = w.and_then(|w| yt_core::watch::chapter_at(&w.chapters, st.position_ms).map(|i| format!(" \u{2022} {}", w.chapters[i].title)));
        if st.live {
            let behind = st.live_edge_ms.saturating_sub(st.position_ms);
            let r = Rect::new(1824.0 - 92.0, time_y - 2.0, 92.0, 36.0);
            ui::fill(p, r, 6.0, if behind < 20_000 { [0.8, 0.0, 0.0, 1.0] } else { YT_REST });
            ui::text(p, "LIVE", r.x + r.w / 2.0, r.y + 5.0, 22, ui::WHITE, ui::ALIGN_CENTER, true);
        } else {
            // The time and chapter end short of the length at the right.
            // The chapter glides when it does not fit (the time beside it stays still).
            let tw = ui::text_raw(p, &left, 96.0, time_y, 26, ui::WHITE, ui::ALIGN_LEFT, false);
            if let Some(ch) = &chapter {
                ui::glide(p, m, ch, 96.0 + tw, time_y, 1728.0 - 180.0 - tw, 26, ui::WHITE, false, ui::GLIDE_PLAYER_CHAPTER);
            }
            ui::text(p, &ui::fmt_time(st.duration_ms), 1824.0, time_y, 26, ui::WHITE, ui::ALIGN_RIGHT, false);
        }

        // The seek bar: white, one piece per chapter; thicker with a knob while it has the focus.
        let bar_focus = self.osd_sel.is_none() && self.sugg_sel.is_none();
        let bh = if bar_focus { 8.0 } else { 5.0 };
        let bar = Rect::new(96.0, 836.0 + dy - bh / 2.0, 1728.0, bh);
        self.yt_bar = bar;
        let frac = if st.duration_ms > 0 { (self.seek_target.unwrap_or(st.position_ms) as f32 / st.duration_ms as f32).clamp(0.0, 1.0) } else { 0.0 };
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
                preview = Some((bar, frac));
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
        let rated = rating(cx);
        let liked = matches!(rated, Some(yt_core::watch::LikeStatus::Like));
        let disliked = matches!(rated, Some(yt_core::watch::LikeStatus::Dislike));
        let subscribed = subscribed_now(cx);
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
            (OSD_SAVE, Kind::ListPlus, in_playlist(cx)),
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
            ui::text(p, &osd_label(i, cx), r.x + r.w / 2.0, cy + B / 2.0 + 12.0, 22, ui::WHITE, ui::ALIGN_CENTER, false);
        }

        // The first row (the playlist, else the suggestions): peeking in at the bottom, or brought
        // up to browse.
        self.row_rects.clear();
        if let Some(&first) = all_rows.first() {
            let heading = (first == RowKind::Playlist).then(|| playing_list(cx, &self.video_id).map(|q| list_line(q, &self.video_id))).flatten();
            if let Some(sel) = self.sugg_sel {
                let at = 1000.0 + dy;
                if let Some(h) = &heading {
                    ui::text_raw(p, h, 96.0, at - 52.0, 28, ui::WHITE, ui::ALIGN_LEFT, true);
                }
                self.draw_row(p, m, cx, 0, Some(sel).filter(|_| first == self.sugg_kind), at);
            } else {
                const TW: f32 = 400.0;
                // From the playing video on in the playlist (its Shuffle button left out).
                let skip = match first {
                    RowKind::Playlist => playing_col(cx, &self.video_id).unwrap_or(0),
                    RowKind::Suggestions => 0,
                };
                let n = row_len(first, cx, &self.video_id);
                let peek: Vec<yt_core::model::Item> = (skip..n.min(skip + 5))
                    .filter_map(|c| match tile(first, cx, &self.video_id, c) {
                        Some(Tile::Video(it)) => Some(it),
                        _ => None,
                    })
                    .collect();
                for (k, it) in peek.iter().enumerate() {
                    let r = Rect::new(96.0 + k as f32 * (TW + 32.0), 1000.0, TW, TW * 9.0 / 16.0);
                    let url = it.thumbnail.clone().unwrap_or_default();
                    let (tex, tw_px, th_px) = plx_ui::tex::resolve_wh_on(0, &url, 0, 0, false);
                    if tex != 0 {
                        p.tex_uv(tex, crop_16_9(tw_px, th_px), r, 14.0, ui::dimmed([1.0, 1.0, 1.0, 0.85]));
                    } else {
                        ui::fill(p, r, 14.0, [1.0, 1.0, 1.0, 0.08]);
                    }
                }
            }
        }
        if let Some((bar, frac)) = preview {
            storyboard_preview(p, st, bar, frac);
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

/// The playlist playing: the play queue when it is a playlist and holds the playing video.
fn playing_list<'a>(cx: &Cx<'a, YtHost>, current: &str) -> Option<&'a PlayQueue> {
    cx.views.store.play_queue.as_ref().filter(|q| q.playlist && q.contains(current))
}

/// A "Play next" video plays in the middle of a playlist, which goes on after it.
fn resuming(cx: &Cx<'_, YtHost>, current: &str) -> bool {
    cx.views.store.play_queue.as_ref().is_some_and(|q| q.playlist && !q.contains(current) && q.resume_after.as_ref().is_some_and(|v| q.contains(v)))
}

/// The playlist line ([`list_line`]) with a list icon before it, cut to `w` px.
fn list_eyebrow(p: plx_ui::Painter, m: &dyn plx_machine::machine::Measure, line: &str, x: f32, y: f32, w: f32, size: i32) {
    let s = size as f32;
    ui::icon::draw(p, ui::icon::Kind::List, Rect::new(x, y + s * 0.12, s, s), ui::WHITE, false);
    ui::text_cut(p, m, line, x + s + 12.0, y, w - s - 12.0, size, ui::WHITE, ui::ALIGN_LEFT, true);
}

/// The start of the line under the title: the channel, after "18+" for an age-restricted video.
pub(crate) fn author_line(st: &crate::player::Status, sep: &str) -> String {
    if st.age_restricted == Some(true) {
        format!("18+{sep}{}", st.author)
    } else {
        st.author.clone()
    }
}

/// The line under the title, made to fit `room`: `width` measures it with the given parts
/// (views, age, subscribers); the least needed go first (subscribers, then the age, then the
/// views) until it fits.
fn fit_byline(
    width: impl Fn(Option<&str>, Option<&str>, Option<&str>) -> f32,
    room: f32,
    views: Option<String>,
    age: Option<String>,
    subs: Option<String>,
) -> (Option<String>, Option<String>, Option<String>) {
    let (mut v, mut a, mut sb) = (views, age, subs);
    let fits = |v: &Option<String>, a: &Option<String>, sb: &Option<String>| width(v.as_deref(), a.as_deref(), sb.as_deref()) <= room;
    if !fits(&v, &a, &sb) {
        sb = None;
    }
    if !fits(&v, &a, &sb) {
        a = None;
    }
    if !fits(&v, &a, &sb) {
        v = None;
    }
    (v, a, sb)
}

#[cfg(test)]
mod byline_tests {
    #[test]
    fn the_least_needed_parts_go_first() {
        // Each part 100 px, the channel 100 px.
        let width = |v: Option<&str>, a: Option<&str>, s: Option<&str>| 100.0 + [v, a, s].iter().flatten().count() as f32 * 100.0;
        let parts = || (Some("views".to_string()), Some("age".to_string()), Some("subs".to_string()));
        let (v, a, s) = parts();
        assert_eq!(super::fit_byline(width, 400.0, v, a, s), parts(), "it all fits");
        let (v, a, s) = parts();
        assert_eq!(super::fit_byline(width, 300.0, v, a, s), (Some("views".into()), Some("age".into()), None));
        let (v, a, s) = parts();
        assert_eq!(super::fit_byline(width, 200.0, v, a, s), (Some("views".into()), None, None));
        let (v, a, s) = parts();
        assert_eq!(super::fit_byline(width, 50.0, v, a, s), (None, None, None));
    }
}

/// Where the player is in the playlist: "Playlist title · 3 of 15", or "3 of more than 40" while
/// the rest of it is loading (or could not be loaded); a mix, which has no end: "3 of 25 so far".
fn list_line(q: &PlayQueue, current: &str) -> String {
    use crate::i18n::tr;
    // A long title (some run to a hundred characters) would push the position off the line.
    let title = match q.title.chars().count() {
        0 => tr("Playlist").to_string(),
        n if n > 48 => format!("{}\u{2026}", q.title.chars().take(47).collect::<String>().trim_end()),
        _ => q.title.clone(),
    };
    let of = |words: &str, k: usize, n: usize| tr(words).replacen("{}", &k.to_string(), 1).replacen("{}", &n.to_string(), 1);
    match q.position(current) {
        // A mix never ends: how far into what YouTube has given so far.
        Some((k, n)) if q.infinite && n > 1 => format!("{title} \u{00B7} {}", of("{} of {} so far", k, n)),
        Some(_) if q.infinite => title,
        Some((k, n)) if q.complete() => format!("{title} \u{00B7} {}", of("{} of {}", k, n)),
        Some((k, n)) if n > 1 => format!("{title} \u{00B7} {}", of("{} of more than {}", k, n)),
        _ => title,
    }
}

/// The rows under the controls.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RowKind {
    /// The playing playlist in play order, after its Shuffle button (not for a mix).
    Playlist,
    Suggestions,
}

/// A card of those rows.
enum Tile {
    /// The playlist's Shuffle button, and whether shuffle is on.
    Shuffle(bool),
    Video(yt_core::model::Item),
}

/// The rows under the controls: the playlist first while one plays (as YouTube shows it), then
/// the suggestions.
fn rows(cx: &Cx<'_, YtHost>, current: &str) -> Vec<RowKind> {
    let mut out = Vec::new();
    if playing_list(cx, current).is_some() {
        out.push(RowKind::Playlist);
    }
    if !suggestions(cx).is_empty() {
        out.push(RowKind::Suggestions);
    }
    out
}

/// The playlist row starts with its Shuffle button (a finite list of more than one video).
fn shuffle_offset(q: &PlayQueue) -> usize {
    q.can_shuffle() as usize
}

/// The number of cards in a row.
fn row_len(kind: RowKind, cx: &Cx<'_, YtHost>, current: &str) -> usize {
    match kind {
        RowKind::Playlist => playing_list(cx, current).map_or(0, |q| q.len() + shuffle_offset(q)),
        RowKind::Suggestions => suggestions(cx).len(),
    }
}

/// Whether the playing playlist is shuffled, as its Shuffle button shows it (the Shuffle playback
/// mode shuffles it unless the viewer switched that off); `None` when it offers no shuffle (no
/// playlist, or a mix).
pub(crate) fn playlist_shuffle(cx: &Cx<'_, YtHost>) -> Option<bool> {
    let q = playing_list(cx, &cx.views.player.video_id)?;
    let mode_on = cx.views.store.settings.playback_mode == PlaybackMode::Shuffle && !q.keep_order;
    (shuffle_offset(q) > 0).then(|| q.shuffled() || mode_on)
}

/// The card in column `col` of a row.
fn tile(kind: RowKind, cx: &Cx<'_, YtHost>, current: &str, col: usize) -> Option<Tile> {
    match kind {
        RowKind::Playlist => {
            let q = playing_list(cx, current)?;
            let off = shuffle_offset(q);
            if col < off {
                return Some(Tile::Shuffle(playlist_shuffle(cx).unwrap_or(false)));
            }
            // The playing video's own tile in a mix's panel comes without its title: the
            // player's.
            q.item(col - off).map(|mut it| {
                if it.title.is_empty() && it.id == current {
                    it.title = cx.views.player.title.clone();
                    it.lines = vec![cx.views.player.author.clone()];
                }
                Tile::Video(it)
            })
        }
        RowKind::Suggestions => suggestions(cx).into_iter().nth(col).map(Tile::Video),
    }
}

/// The playing video's column in the playlist row.
fn playing_col(cx: &Cx<'_, YtHost>, current: &str) -> Option<usize> {
    let q = playing_list(cx, current)?;
    Some(q.index(current)? + shuffle_offset(q))
}

/// Next video when one ends: the next playlist item (or the first when repeating the list),
/// else the watch page's autoplay pick.
/// What each OSD button does, in words (its tooltip).
/// A button's name under or over it, translated; the like and dislike buttons add their count, as
/// SmartTube's do ("Like  6,992").
fn osd_label(i: usize, cx: &Cx<'_, YtHost>) -> String {
    let name = crate::i18n::tr(osd_name(i)).to_string();
    let ex = &cx.views.player.extras;
    let count = match i {
        OSD_LIKE => ex.votes.as_ref().map(|v| ui::full_count(v.likes)).or_else(|| ex.watch.as_ref().and_then(|w| w.like_count_text.clone())),
        OSD_DISLIKE => ex.votes.as_ref().map(|v| ui::full_count(v.dislikes)),
        _ => None,
    };
    match count {
        Some(c) => format!("{name}  {c}"),
        None => name,
    }
}

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


impl PlayerScreen {
    /// The rows taking the screen (browsing them in the SmartTube style, or below the first row
    /// in the YouTube style): the title on top, then the rows with their headings, the one being
    /// browsed in front.
    fn draw_rows_full(&mut self, p: plx_ui::Painter, m: &dyn plx_machine::machine::Measure, cx: &Cx<'_, YtHost>) {
        let st = cx.views.player;
        ui::fill(p, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.0, 0.0, 0.0, 0.82]);
        let all = rows(cx, &self.video_id);
        // Two rows: the title higher, so that each row's raised card and caption (347 px from its
        // top) stay clear of the next row's heading.
        let (title_y, tops): (f32, &[f32]) = if all.len() > 1 { (56.0, &[250.0, 708.0]) } else { (90.0, &[560.0]) };
        ui::glide(p, m, &st.title, 96.0, title_y, 1728.0, 48, ui::WHITE, true, ui::GLIDE_PLAYER_TITLE);
        ui::text_raw(p, &st.author, 96.0, title_y + 66.0, 28, ui::DIM, ui::ALIGN_LEFT, false);
        self.row_rects.clear();
        for (r, (kind, y)) in all.iter().zip(tops).enumerate() {
            let heading = match (kind, playing_list(cx, &self.video_id)) {
                (RowKind::Playlist, Some(q)) => list_line(q, &self.video_id),
                _ => crate::i18n::tr("Suggestions").to_string(),
            };
            let on = *kind == self.sugg_kind;
            ui::text_raw(p, &heading, 96.0, y - 58.0, if on { 36 } else { 30 }, if on { ui::WHITE } else { ui::DIM }, ui::ALIGN_LEFT, true);
            self.draw_row(p, m, cx, r, self.sugg_sel.filter(|_| on), *y);
        }
        common::toast(p, cx);
    }

    /// Row `r` of [`rows`] at `y`: `sel` is the selected column while it is browsed; otherwise a
    /// playlist rests on its playing video.
    fn draw_row(&mut self, p: plx_ui::Painter, m: &dyn plx_machine::machine::Measure, cx: &Cx<'_, YtHost>, r: usize, sel: Option<usize>, y: f32) {
        let Some(&kind) = rows(cx, &self.video_id).get(r) else { return };
        let n = row_len(kind, cx, &self.video_id);
        let playing = match kind {
            RowKind::Playlist => playing_col(cx, &self.video_id),
            RowKind::Suggestions => None,
        };
        let focus = sel.or(playing).unwrap_or(0);
        let current = self.video_id.clone();
        let rects = draw_cards(p, m, n, &|c| tile(kind, cx, &current, c), sel, focus, playing, y, kind as usize);
        self.row_rects.extend(rects.into_iter().map(|(c, rect)| ((kind, c), rect)));
    }
}

/// The rows' motion, kept between frames (one per row kind): where the row is scrolled to (in
/// cards) and how far each card is lifted (0..1), eased toward the selection as the lists' cards
/// are.
struct SuggMotion {
    /// The row it belongs to (first card, count): another row starts at rest.
    key: (String, usize),
    scroll: f32,
    pop: Vec<f32>,
    at: std::time::Instant,
}

thread_local! {
    static SUGG_MOTION: std::cell::RefCell<[Option<SuggMotion>; 2]> = const { std::cell::RefCell::new([None, None]) };
}

/// Moves `x` toward `to` for `dt` seconds (a critically damped ease without overshoot);
/// `true` while it is still on its way.
fn ease_to(x: &mut f32, to: f32, dt: f32) -> bool {
    const RATE: f32 = 12.0;
    *x += (to - *x) * (1.0 - (-RATE * dt).exp());
    if (to - *x).abs() < 0.002 {
        *x = to;
        false
    } else {
        true
    }
}

/// The part of a `w` x `h` thumbnail (as `tex_uv` takes it: x, y, width, height) that fills a
/// 16:9 card: its middle. YouTube's playlist and mix items often come as 4:3 pictures with black
/// bars baked in above and below; stretched over the card, the picture looked squashed.
fn crop_16_9(w: f32, h: f32) -> [f32; 4] {
    if w <= 0.0 || h <= 0.0 {
        return [0.0, 0.0, 1.0, 1.0];
    }
    let card = 16.0 / 9.0;
    let pic = w / h;
    if pic < card {
        // Taller than 16:9: the middle band.
        let vh = pic / card;
        [0.0, (1.0 - vh) / 2.0, 1.0, vh]
    } else {
        // Wider: the middle part across.
        let uw = card / pic;
        [(1.0 - uw) / 2.0, 0.0, uw, 1.0]
    }
}

#[cfg(test)]
mod crop_tests {
    #[test]
    fn a_4_3_thumbnail_shows_its_16_9_middle() {
        let c = super::crop_16_9(480.0, 360.0);
        assert!((c[1] - 0.125).abs() < 1e-4 && (c[3] - 0.75).abs() < 1e-4, "{c:?}");
        assert_eq!(super::crop_16_9(1280.0, 720.0), [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(super::crop_16_9(0.0, 0.0), [0.0, 0.0, 1.0, 1.0]);
    }
}

fn with_alpha(c: [f32; 4], a: f32) -> [f32; 4] {
    [c[0], c[1], c[2], c[3] * a]
}

fn mix(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, a[3] + (b[3] - a[3]) * t]
}

/// A row of `n` cards at `y` (`tile` gives each), returning where each visible one was drawn. It
/// glides to column `focus` (one before it stays in view); the selected card (`sel`) lifts on an
/// ease and its title and detail glide when they do not fit, as a selected card in a list does.
/// The `playing` card is marked. `slot` keeps this row's motion apart from the other row's.
#[allow(clippy::too_many_arguments)]
fn draw_cards(
    p: plx_ui::Painter,
    m: &dyn plx_machine::machine::Measure,
    n: usize,
    tile: &dyn Fn(usize) -> Option<Tile>,
    sel: Option<usize>,
    focus: usize,
    playing: Option<usize>,
    y: f32,
    slot: usize,
) -> Vec<(usize, Rect)> {
    const W: f32 = 400.0;
    const H: f32 = 225.0;
    const GAP: f32 = 32.0;
    const PITCH: f32 = W + GAP;
    let mut out = Vec::new();
    if n == 0 {
        return out;
    }
    let target = focus.saturating_sub(1) as f32;
    let first_id = match tile(0) {
        Some(Tile::Video(it)) => it.id,
        Some(Tile::Shuffle(_)) => match tile(1) {
            Some(Tile::Video(it)) => it.id,
            _ => String::new(),
        },
        None => String::new(),
    };
    let key = (first_id, n);
    let (scroll, pops) = SUGG_MOTION.with(|cell| {
        let mut cell = cell.borrow_mut();
        let now = std::time::Instant::now();
        let fresh = || {
            let mut pop = vec![0.0; n];
            if let Some(v) = sel.and_then(|s| pop.get_mut(s)) {
                *v = 1.0;
            }
            SuggMotion { key: key.clone(), scroll: target, pop, at: now }
        };
        let st = cell[slot.min(1)].get_or_insert_with(fresh);
        // Another row, or the row comes back after a while: it starts where it rests.
        if st.key != key || now.duration_since(st.at).as_secs_f32() > 0.5 {
            *st = fresh();
        }
        let dt = now.duration_since(st.at).as_secs_f32().min(0.1);
        st.at = now;
        let mut moving = ease_to(&mut st.scroll, target, dt);
        // Only the cards near the view are eased (a long playlist has thousands).
        let lo = (st.scroll.floor().max(0.0) as usize).saturating_sub(2);
        for (k, v) in st.pop.iter_mut().enumerate().skip(lo).take(10) {
            moving |= ease_to(v, if Some(k) == sel { 1.0 } else { 0.0 }, dt);
        }
        if moving {
            ui::animating();
        }
        let lo = st.scroll.floor().max(0.0) as usize;
        (st.scroll, st.pop.iter().skip(lo).take(7).copied().collect::<Vec<f32>>())
    });
    let first = scroll.floor().max(0.0) as usize;
    for k in first..n.min(first + 7) {
        let x = 96.0 + (k as f32 - scroll) * PITCH;
        if x > 1920.0 {
            break;
        }
        // A card leaving on the left fades as it goes (the one before the selection stays).
        let alpha = if x < 96.0 { (1.0 - (96.0 - x) / PITCH).clamp(0.0, 1.0) } else { 1.0 };
        if alpha <= 0.0 {
            continue;
        }
        let Some(t) = tile(k) else { continue };
        let pop = pops.get(k - first).copied().unwrap_or(0.0);
        let grow = 12.0 * pop;
        let r = Rect::new(x - grow, y - grow, W + 2.0 * grow, H + 2.0 * grow);
        out.push((k, r));
        let panel = Rect::new(r.x, r.y + r.h + 6.0, r.w, 104.0);
        let bg = mix([1.0, 1.0, 1.0, 0.08], [0.94, 0.94, 0.96, 1.0], pop);
        let ink = with_alpha(mix([1.0, 1.0, 1.0, 0.92], [0.09, 0.09, 0.11, 1.0], pop), alpha);
        let dim = with_alpha(mix(ui::DIM, [0.09, 0.09, 0.11, 0.66], pop), alpha);
        let tw = panel.w - 32.0;
        let it = match t {
            Tile::Shuffle(on) => {
                // The playlist's Shuffle: its icon on the card (in the accent colour while on),
                // its name and state under it.
                ui::fill(p, r, 14.0, with_alpha(mix([1.0, 1.0, 1.0, 0.08], [1.0, 1.0, 1.0, 0.2], pop), alpha));
                let sz = 96.0;
                let ink_icon = if on { ui::accent() } else { ui::WHITE };
                ui::icon::draw(p, ui::icon::Kind::Shuffle, Rect::new(r.x + (r.w - sz) / 2.0, r.y + (r.h - sz) / 2.0, sz, sz), with_alpha(ink_icon, alpha), false);
                ui::fill(p, panel, 14.0, with_alpha(bg, alpha));
                ui::text(p, "Shuffle", panel.x + 16.0, panel.y + 12.0, 24, ink, ui::ALIGN_LEFT, true);
                ui::text(p, if on { "On" } else { "Off" }, panel.x + 16.0, panel.y + 52.0, 20, dim, ui::ALIGN_LEFT, false);
                continue;
            }
            Tile::Video(it) => it,
        };
        let url = it.thumbnail.clone().unwrap_or_default();
        let (tex, tw_px, th_px) = plx_ui::tex::resolve_wh_on(0, &url, 0, 0, false);
        if tex != 0 {
            p.tex_uv(tex, crop_16_9(tw_px, th_px), r, 14.0, with_alpha(ui::dimmed(ui::WHITE), alpha));
        } else {
            ui::fill(p, r, 14.0, [1.0, 1.0, 1.0, 0.08 * alpha]);
        }
        if let Some(b) = &it.badge {
            let bw = 14.0 * b.chars().count() as f32 + 20.0;
            let br = Rect::new(r.x + r.w - bw - 10.0, r.y + r.h - 44.0, bw, 34.0);
            ui::fill(p, br, 6.0, [0.0, 0.0, 0.0, 0.78 * alpha]);
            ui::text(p, b, br.x + bw / 2.0, br.y + 4.0, 22, with_alpha(ui::WHITE, alpha), ui::ALIGN_CENTER, true);
        }
        // The playing video: a play mark in its corner and a bar along its foot in the accent
        // colour.
        if playing == Some(k) {
            ui::fill(p, Rect::new(r.x, r.y + r.h - 8.0, r.w, 8.0), 4.0, with_alpha(ui::accent(), alpha));
            let c = Rect::new(r.x + 12.0, r.y + 12.0, 52.0, 52.0);
            ui::fill(p, c, 26.0, [0.0, 0.0, 0.0, 0.7 * alpha]);
            ui::icon::draw(p, ui::icon::Kind::Play, Rect::new(c.x + 15.0, c.y + 13.0, 26.0, 26.0), with_alpha(ui::WHITE, alpha), true);
        }
        // A card with nothing to say (a window's tile without its title) gets no caption, rather
        // than an empty band; with only its detail, that takes the title's place.
        let mut detail = it.lines.iter().filter(|l| !l.is_empty()).cloned().collect::<Vec<_>>().join(" \u{2022} ");
        let title = if it.title.is_empty() { std::mem::take(&mut detail) } else { it.title.clone() };
        if title.is_empty() {
            continue;
        }
        ui::fill(p, panel, 14.0, with_alpha(bg, alpha));
        if Some(k) == sel {
            ui::glide(p, m, &title, panel.x + 16.0, panel.y + 12.0, tw, 24, ink, true, ui::GLIDE_CARD_TITLE);
            ui::glide(p, m, &detail, panel.x + 16.0, panel.y + 52.0, tw, 20, dim, false, ui::GLIDE_CARD_DETAIL);
        } else {
            ui::text_cut(p, m, &title, panel.x + 16.0, panel.y + 12.0, tw, 24, ink, ui::ALIGN_LEFT, true);
            ui::text_cut(p, m, &detail, panel.x + 16.0, panel.y + 52.0, tw, 20, dim, ui::ALIGN_LEFT, false);
        }
    }
    out
}

/// The playing video has chapters.
fn has_chapters(cx: &Cx<'_, YtHost>) -> bool {
    cx.views.player.extras.watch.as_ref().is_some_and(|w| !w.chapters.is_empty())
}

/// YouTube's reason for refusing a video (always English: requests are sent with hl=en) as one of
/// this app's translated sentences, when it is a common one.
fn refusal_text(reason: &str, signed_in: bool) -> Option<&'static str> {
    let r = reason.to_ascii_lowercase();
    let has = |k: &str| r.contains(k);
    Some(if has("available to this channel's members") || has("members-only") {
        "Only this channel's members can watch it."
    } else if has("not a bot") && signed_in {
        // Signed in already: a bot check of the whole connection, which passes after a while.
        "YouTube is checking this network for bots. Try again in a few minutes."
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
    cx.views.store.play_next.iter().map(|v| &v.video_id).find(|v| v.as_str() != current).cloned()
}

/// The playing video as a record for a playlist on this TV.
pub fn playing_video(st: &crate::host::PlayerView) -> yt_core::local_playlists::Video {
    yt_core::local_playlists::Video {
        video_id: st.video_id.clone(),
        title: st.title.clone(),
        channel: st.author.clone(),
        duration_secs: if st.live { 0 } else { st.duration_ms / 1000 },
        thumbnail: None,
    }
}

fn next_video(cx: &Cx<'_, YtHost>, current: &str, repeat_list: bool) -> Option<(String, Option<String>)> {
    let ex = cx.views.player.extras.clone();
    let w = ex.watch.as_ref()?;
    let items: Option<(String, Vec<String>)> = w.playlist.as_ref().map(|pl| (pl.id.clone(), pl.items.iter().map(|i| i.id.clone()).collect()));
    crate::next_up::yt_next(items.as_ref().map(|(id, v)| (id.as_str(), v.as_slice())), autoplay_pick(w), current, repeat_list)
}

/// The watch page's autoplay pick as (video, playlist).
fn autoplay_pick(w: &yt_core::watch::WatchInfo) -> Option<(String, Option<String>)> {
    let n = w.autoplay_next.as_ref()?;
    Some(match &n.endpoint {
        Some(yt_core::model::Endpoint::Watch { video_id, playlist_id, .. }) if !video_id.is_empty() => (video_id.clone(), playlist_id.clone()),
        _ => (n.id.clone(), None),
    })
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
        assert_eq!(refusal_text("This video is available to this channel's members on level: X. Join this channel.", false), Some("Only this channel's members can watch it."));
        assert_eq!(refusal_text("Sign in to confirm your age", false), Some("YouTube asks to confirm your age: sign in with an adult account."));
        assert_eq!(refusal_text("Video unavailable", false), Some("This video is not available."));
        assert_eq!(refusal_text("This video is private", false), Some("This video is private."));
        assert_eq!(refusal_text("no client could play abc: Profile(tv): ERROR boom", false), None);
        assert_eq!(refusal_text("Sign in to confirm you're not a bot", true), Some("YouTube is checking this network for bots. Try again in a few minutes."));
        assert_eq!(refusal_text("Sign in to confirm you're not a bot", false), Some("YouTube asks to confirm you're not a bot: try again later, or sign in."));
    }
}

#[cfg(test)]
mod seek_tests {
    use super::PlayerScreen;

    #[test]
    fn a_held_seek_speeds_up_every_second_as_smarttube() {
        let mut p = PlayerScreen::new(plx_machine::machine::EntryId(1), "v".into(), None);
        let base = 10_000;
        // A press: the setting's step.
        p.seek_repeat = false;
        assert_eq!(p.held_step(1, base, 0), base);
        // Repeats in the first second: the same step; after a second: 1.5 times, and again.
        p.seek_repeat = true;
        assert_eq!(p.held_step(1, base, 500), base);
        assert_eq!(p.held_step(1, base, 1000), 15_000);
        assert_eq!(p.held_step(1, base, 1500), 15_000);
        assert_eq!(p.held_step(1, base, 2000), 22_500);
        // A new press starts over; so does the other direction.
        p.seek_repeat = false;
        assert_eq!(p.held_step(1, base, 2100), base);
        p.seek_repeat = true;
        assert_eq!(p.held_step(-1, base, 3200), base);
    }
}

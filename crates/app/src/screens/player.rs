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
    state: State,
}

impl PlayerScreen {
    pub fn new(entry: EntryId, video_id: String, playlist: Option<String>) -> Self {
        PlayerScreen { entry, state: State(video_id.clone()), video_id, playlist, hud: true, ended_handled: false }
    }

    fn show_hud(&mut self, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        self.hud = true;
        let secs = cx.views.store.settings.hud_timeout_secs.max(2);
        fx.push(Fx::CancelTimer(T_HIDE));
        fx.push(Fx::Timer { id: T_HIDE, after_ms: secs * 1000 });
        fx.push(Fx::Timer { id: T_TICK, after_ms: 500 });
        fx.invalidate(plx_machine::present::Provenance::Input);
    }

    fn seek(&mut self, dir: i64, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        let step = cx.views.store.settings.seek_step_secs.max(1) as i64 * 1000;
        fx.push(Fx::App(YtFx::SeekBy(dir * step)));
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
                        _ => self.show_hud(cx, fx),
                    }
                }
                if self.hud || p.phase != Phase::Playing {
                    fx.push(Fx::Timer { id: T_TICK, after_ms: 500 });
                }
                fx.invalidate(plx_machine::present::Provenance::Timer(T_TICK));
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
                    (Key::Ok, _) | (_, WCODE_PLAY) | (_, WCODE_PAUSE) | (_, WCODE_PLAYPAUSE) => {
                        fx.push(Fx::App(YtFx::TogglePause));
                        self.show_hud(cx, fx);
                        Handled::Yes
                    }
                    (Key::Left, _) | (_, WCODE_RW) => {
                        self.seek(-1, cx, fx);
                        Handled::Yes
                    }
                    (Key::Right, _) | (_, WCODE_FF) => {
                        self.seek(1, cx, fx);
                        Handled::Yes
                    }
                    (Key::Up, _) | (Key::Down, _) => {
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
        if let Some(msg) = centre {
            ui::fill(p, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.0, 0.0, 0.0, 1.0]);
            ui::text(p, &msg, 960.0, 510.0, 36, ui::DIM, ui::ALIGN_CENTER, false);
        }
        if !self.hud && st.phase == Phase::Playing {
            common::toast(p, cx);
            return;
        }
        // Bottom scrim + title + progress
        ui::fill(p, Rect::new(0.0, 700.0, 1920.0, 380.0), 0.0, [0.0, 0.0, 0.0, 0.55]);
        ui::fill(p, Rect::new(0.0, 0.0, 1920.0, 140.0), 0.0, [0.0, 0.0, 0.0, 0.40]);
        ui::text(p, &ui::ellipsize(&st.title, 80), 96.0, 780.0, 44, ui::WHITE, ui::ALIGN_LEFT, true);
        ui::text(p, &st.author, 96.0, 842.0, 30, ui::DIM, ui::ALIGN_LEFT, false);
        let bar = Rect::new(96.0, 920.0, 1728.0, 10.0);
        ui::fill(p, bar, 5.0, [1.0, 1.0, 1.0, 0.25]);
        let frac = if st.duration_ms > 0 { (st.position_ms as f32 / st.duration_ms as f32).clamp(0.0, 1.0) } else { 0.0 };
        ui::fill(p, Rect::new(bar.x, bar.y, bar.w * frac, bar.h), 5.0, ui::ACCENT);
        ui::fill(p, Rect::new(bar.x + bar.w * frac - 12.0, bar.y - 7.0, 24.0, 24.0), 12.0, ui::WHITE);
        ui::text(p, &ui::fmt_time(st.position_ms), 96.0, 950.0, 30, ui::WHITE, ui::ALIGN_LEFT, false);
        let mut right = ui::fmt_time(st.duration_ms);
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
        if st.height > 0 {
            info.push_str(&format!("   {}p{}", st.height, if st.fps > 30 { st.fps.to_string() } else { String::new() }));
        }
        if (st.rate - 1.0).abs() > 0.01 && st.rate > 0.0 {
            info.push_str(&format!("   {}x", st.rate));
        }
        ui::text(p, &info, 96.0, 1010.0, 26, ui::DIM, ui::ALIGN_LEFT, false);
        ui::text(p, "\u{25B2} Options", 1824.0, 1010.0, 26, ui::FAINT, ui::ALIGN_RIGHT, false);
        if cx.views.store.settings.show_clock {
            ui::text(p, &common::clock(&cx.views), 1824.0, 50.0, 34, ui::WHITE, ui::ALIGN_RIGHT, false);
        }
        common::toast(p, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::VideoPlane
    }
}

//! Full-screen playback. Draws nothing over the video plane yet (the HUD comes next); keys:
//! OK / Play / Pause toggle, Left / Right seek 10 s, BACK closes (which stops playback).

use crate::host::{YtFx, YtHost};
use plx_machine::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, Key, LogicalState, Machine, ScreenEvent,
};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use std::borrow::Cow;

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
    state: State,
}

impl PlayerScreen {
    pub fn new(entry: EntryId, video_id: String) -> Self {
        PlayerScreen { entry, state: State(video_id.clone()), video_id }
    }
}

const WCODE_PLAY: u32 = 450;
const WCODE_PAUSE: u32 = 72;
const WCODE_PLAYPAUSE: u32 = 261;
const WCODE_FF: u32 = 451;
const WCODE_RW: u32 = 452;

impl Machine<YtHost> for PlayerScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                fx.push(Fx::App(YtFx::Play(self.video_id.clone())));
                Handled::Yes
            }
            ScreenEvent::Unmount => {
                fx.push(Fx::App(YtFx::Stop));
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key, wcode, edge, .. }, .. }) => {
                if *edge == Edge::Up {
                    return if *key == Key::Back { Handled::No } else { Handled::Yes };
                }
                match (key, *wcode) {
                    (Key::Back, _) => Handled::No,
                    (Key::Ok, _) | (_, WCODE_PLAY) | (_, WCODE_PAUSE) | (_, WCODE_PLAYPAUSE) => {
                        fx.push(Fx::App(YtFx::TogglePause));
                        Handled::Yes
                    }
                    (Key::Left, _) | (_, WCODE_RW) => {
                        fx.push(Fx::App(YtFx::SeekBy(-10_000)));
                        Handled::Yes
                    }
                    (Key::Right, _) | (_, WCODE_FF) => {
                        fx.push(Fx::App(YtFx::SeekBy(10_000)));
                        Handled::Yes
                    }
                    _ => Handled::Yes,
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
    fn draw(&mut self, _f: &mut DrawFrame<'_, '_, YtHost>) {}
    fn render(&self) -> RenderStrategy {
        RenderStrategy::VideoPlane
    }
}

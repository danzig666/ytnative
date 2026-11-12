//! Home: a grid of video cards (landscape thumbnails), D-pad focus, OK opens, BACK goes back.

use crate::host::{Card, Feed, YtArg, YtFx, YtHost, YtMsg};
use plx_machine::machine::{Canon, Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, LogicalState, Machine, NavOp, ScreenEvent};
use plx_ui::card_row::{RowStyle, TileLabel};
use plx_ui::cards::{CardEvent, CardSource, Grid, GridSpec};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::widgets::Art;
use std::borrow::Cow;

/// 16:9 cards, four per row.
const STYLE: RowStyle = RowStyle { w: 384.0, h: 216.0, gap: 48.0, ..RowStyle::HOME };
const SPEC: GridSpec = GridSpec::new(120.0, 96.0).columns(4, STYLE, 96.0);
const G: GroupId = GroupId(1);

struct Src<'a>(&'a [Card]);

impl CardSource<YtHost> for Src<'_> {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn elem(&self, i: usize) -> u32 {
        self.0[i].elem
    }
    fn index_of(&self, e: &u32) -> Option<usize> {
        self.0.iter().position(|c| c.elem == *e)
    }
    fn art(&self, i: usize) -> Art<'_> {
        Art::Thumb { sid: 0, key: &self.0[i].thumb, res: (384, 216) }
    }
    fn label(&self, i: usize) -> TileLabel {
        TileLabel::from_lines([Some(self.0[i].title.clone()), Some(self.0[i].detail.clone())])
    }
}

struct State(u32);
impl LogicalState for State {
    fn write(&self, c: &mut Canon) {
        c.u32(self.0);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("home:{}", self.0));
    }
}

pub struct HomeScreen {
    entry: EntryId,
    grid: Grid,
    state: State,
}

impl HomeScreen {
    pub fn new(entry: EntryId) -> Self {
        HomeScreen { entry, grid: Grid::new(entry, SPEC), state: State(0) }
    }
    /// Placeholder for the video page until the player screen exists: same grid, deeper entry.
    pub fn detail(entry: EntryId, from: u32) -> Self {
        HomeScreen { entry, grid: Grid::new(entry, SPEC), state: State(from) }
    }
}

impl Machine<YtHost> for HomeScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        match ev {
            ScreenEvent::Mount if self.state.0 == 0 => fx.push(Fx::App(YtFx::Load(Feed::Home))),
            ScreenEvent::Async(_, YtMsg::Loaded(_)) | ScreenEvent::Async(_, YtMsg::Failed(..)) => {
                fx.invalidate(plx_machine::present::Provenance::Lifecycle);
            }
            _ => {}
        }
        let src = Src(cx.views.home);
        if let Some(CardEvent::Activate(e)) = self.grid.on(ev, cx, &src, fx) {
            fx.push(Fx::Nav(NavOp::Push(YtArg::Video(e))));
            return Handled::Yes;
        }
        match ev {
            ScreenEvent::Input(_) => Handled::No,
            _ => Handled::Yes,
        }
    }
}

impl Focusable<YtHost> for HomeScreen {
    fn groups(&self, cx: &Cx<'_, YtHost>, out: &mut Vec<GroupSpec>) {
        out.push(self.grid.group_spec(G, cx.views.home.len()));
    }
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        Some(G)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, YtHost>) -> Step<u32> {
        self.grid.neighbour(&Src(cx.views.home), key, dir)
    }
    fn place(&self, key: &u32, cx: &Cx<'_, YtHost>, at: At) -> Option<Placed> {
        self.grid.place(cx, &Src(cx.views.home), key, at)
    }
    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        want
    }
    fn seat(&self, _g: GroupId, from: Placed, cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        let src = Src(cx.views.home);
        self.grid
            .seat(&src, from)
            .unwrap_or(FocusKey { entry: self.grid_entry(), elem: cx.views.home.first().map_or(0, |c| c.elem) })
    }
}

impl HomeScreen {
    fn grid_entry(&self) -> EntryId {
        self.entry
    }
}

impl Screen<YtHost> for HomeScreen {
    fn name(&self) -> &'static str {
        if self.state.0 == 0 { "home" } else { "detail" }
    }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, YtHost>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, YtHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, YtHost>) {
        let src = Src(f.cx.views.home);
        let p = f.painter;
        self.grid.draw(f, p, &src);
        self.grid.record_stops(f, p, &src);
    }
    fn redraw_focused(&self, f: &mut DrawFrame<'_, '_, YtHost>, focus: Option<FocusKey<u32>>) {
        let p = f.painter;
        self.grid.redraw_focused(f, p, &Src(f.cx.views.home), focus);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

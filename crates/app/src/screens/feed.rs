//! A full-width grid page for any browse target: a channel, a playlist, a library entry, search
//! results. Title from the response; pages in more as you scroll; BACK returns.

use super::common;
use crate::host::{YtArg, YtFx, YtHost, YtMsg};
use crate::store::FeedKey;
use crate::ui::{self, CardsSrc};
use plx_machine::machine::{Canon, Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, LogicalState, Machine, NavOp, ScreenEvent};
use plx_ui::card_row::RowStyle;
use plx_ui::cards::{CardEvent, Grid, GridSpec};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use std::borrow::Cow;

const STYLE: RowStyle = RowStyle { w: 380.0, h: 214.0, gap: 44.0, ..RowStyle::HOME };
const SPEC: GridSpec = GridSpec::new(170.0, 96.0).columns(4, STYLE, 112.0);
const G: GroupId = GroupId(1);

struct State(String);
impl LogicalState for State {
    fn write(&self, c: &mut Canon) {
        c.str(&self.0);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&self.0);
    }
}

pub struct FeedScreen {
    entry: EntryId,
    key: FeedKey,
    grid: Grid,
    state: State,
}

impl FeedScreen {
    pub fn new(entry: EntryId, key: FeedKey) -> Self {
        FeedScreen { entry, state: State(format!("{key:?}")), key, grid: Grid::new(entry, SPEC) }
    }
    fn src<'a>(&self, cx: &Cx<'a, YtHost>) -> CardsSrc<'a> {
        let f = cx.views.store.feed(&self.key);
        CardsSrc { cards: f.map(|f| f.cards.as_slice()).unwrap_or(&[]), more: f.is_some_and(|f| f.continuation.is_some()) }
    }
    fn title(&self, cx: &Cx<'_, YtHost>) -> String {
        if let Some(t) = cx.views.store.feed(&self.key).map(|f| f.title.clone()).filter(|t| !t.is_empty()) {
            return t;
        }
        match &self.key {
            FeedKey::Search { query, .. } => format!("Results for \u{201C}{query}\u{201D}"),
            FeedKey::Browse { id, .. } if id.starts_with("VL") => "Playlist".into(),
            FeedKey::Browse { .. } => "Channel".into(),
            FeedKey::Section(s) => s.title().into(),
            FeedKey::Related(_) => "Up next".into(),
        }
    }
}

impl Machine<YtHost> for FeedScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        match ev {
            ScreenEvent::Mount => fx.push(Fx::App(YtFx::Load(self.key.clone()))),
            ScreenEvent::Async(_, YtMsg::Loaded(_) | YtMsg::Failed(..)) => fx.invalidate(plx_machine::present::Provenance::Lifecycle),
            _ => {}
        }
        // Up from the top row of search results opens the filters panel.
        if let (FeedKey::Search { query, .. }, ScreenEvent::Input(plx_machine::machine::InputEvent {
            kind: plx_machine::machine::InputKind::Key { key: plx_machine::machine::Key::Up, edge: plx_machine::machine::Edge::Down, .. },
            ..
        })) = (&self.key, ev)
        {
            let src = self.src(cx);
            let at_top = cx.focus.current.and_then(|k| src.cards.iter().position(|c| c.elem == k.elem)).map_or(true, |i| i < 4);
            if at_top {
                fx.push(Fx::Nav(NavOp::Present(YtArg::SearchFilters(query.clone()))));
                return Handled::Yes;
            }
        }
        let src = self.src(cx);
        match self.grid.on(ev, cx, &src, fx) {
            Some(CardEvent::Activate(e)) => {
                if let Some(card) = cx.views.store.card(e) {
                    common::open_card(card, fx);
                }
                return Handled::Yes;
            }
            Some(CardEvent::Hold(e)) => {
                fx.push(Fx::Nav(NavOp::Present(YtArg::Menu(e))));
                return Handled::Yes;
            }
            Some(CardEvent::Want(_)) => fx.push(Fx::App(YtFx::More(self.key.clone()))),
            None => {}
        }
        match ev {
            ScreenEvent::Input(_) => Handled::No,
            _ => Handled::Yes,
        }
    }
}

impl Focusable<YtHost> for FeedScreen {
    fn groups(&self, cx: &Cx<'_, YtHost>, out: &mut Vec<GroupSpec>) {
        out.push(self.grid.group_spec(G, self.src(cx).cards.len()));
    }
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        Some(G)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, YtHost>) -> Step<u32> {
        self.grid.neighbour(&self.src(cx), key, dir)
    }
    fn place(&self, key: &u32, cx: &Cx<'_, YtHost>, at: At) -> Option<Placed> {
        self.grid.place(cx, &self.src(cx), key, at)
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        let src = self.src(cx);
        if src.cards.iter().any(|c| c.elem == want.elem) {
            return want;
        }
        src.cards.first().map_or(want, |c| FocusKey { entry: self.entry, elem: c.elem })
    }
    fn seat(&self, _g: GroupId, from: Placed, cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        let src = self.src(cx);
        self.grid.seat(&src, from).unwrap_or(FocusKey { entry: self.entry, elem: src.cards.first().map_or(0, |c| c.elem) })
    }
}

impl Screen<YtHost> for FeedScreen {
    fn name(&self) -> &'static str {
        "feed"
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
        ui::text(p, &ui::ellipsize(&self.title(cx), 70), 112.0, 64.0, 46, ui::WHITE, ui::ALIGN_LEFT, true);
        if let FeedKey::Search { params, .. } = &self.key {
            let f = &cx.views.store.search_filters;
            let hint = if params.is_some() && !f.is_default() { format!("{}   \u{25B2} Filters", f.summary()) } else { "\u{25B2} Filters".into() };
            ui::text(p, &hint, 1808.0, 74.0, 26, ui::FAINT, ui::ALIGN_RIGHT, false);
        }
        let src = self.src(cx);
        if src.cards.is_empty() {
            common::empty_state(p, cx.views.store.feed(&self.key), Rect::new(112.0, 170.0, 1700.0, 800.0));
        } else {
            self.grid.draw(f, p, &src);
            self.grid.record_stops(f, p, &src);
        }
        common::toast(p, cx);
    }
    fn redraw_focused(&self, f: &mut DrawFrame<'_, '_, YtHost>, focus: Option<FocusKey<u32>>) {
        let p = f.painter;
        let src = self.src(f.cx);
        self.grid.redraw_focused(f, p, &src, focus);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

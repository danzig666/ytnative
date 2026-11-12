//! A full-width grid page for any browse target: a channel, a playlist, a library entry, search
//! results. Title from the response; pages in more as you scroll; BACK returns.

use super::common;
use crate::host::{YtArg, YtFx, YtHost};
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
fn spec() -> GridSpec {
    GridSpec::new(170.0, 96.0).columns(ui::card_cols(), ui::card_style(ui::card_cols(), 1652.0, 44.0), 112.0)
}
const G: GroupId = GroupId(1);
const G_BTN: GroupId = GroupId(2);
const SUB_BTN: u32 = 0x7900_0000;
const SUB_RECT: Rect = Rect::new(112.0, 112.0, 300.0, 50.0);

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
        FeedScreen { entry, state: State(format!("{key:?}")), key, grid: Grid::new(entry, spec()) }
    }
    fn src<'a>(&self, cx: &Cx<'a, YtHost>) -> CardsSrc<'a> {
        let f = cx.views.store.feed(&self.key);
        CardsSrc { cards: f.map(|f| f.cards.as_slice()).unwrap_or(&[]), more: f.is_some_and(|f| f.continuation.is_some()) }
    }
    /// The channel id when this page is a channel.
    fn channel(&self) -> Option<&str> {
        match &self.key {
            FeedKey::Browse { id, .. } if id.starts_with("UC") => Some(id),
            _ => None,
        }
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
            FeedKey::Group(id) => cx.views.store.groups.iter().find(|g| &g.0 == id).map(|g| g.1.clone()).unwrap_or_else(|| "Group".into()),
        }
    }
}

impl Machine<YtHost> for FeedScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        if let Some(a) = super::common::press_commit_as_activate(ev, cx, |e| e == SUB_BTN) {
            return self.step(&a, cx, fx);
        }
        match ev {
            ScreenEvent::Mount => {
                fx.push(Fx::App(YtFx::Load(self.key.clone())));
                if let Some(ch) = self.channel() {
                    fx.push(Fx::App(YtFx::LoadChannelInfo(ch.to_string())));
                }
            }
            ScreenEvent::Activate(SUB_BTN) => {
                if let Some(ch) = self.channel() {
                    let now = cx.views.store.subscribed.get(ch).copied().unwrap_or(false);
                    fx.push(Fx::App(YtFx::Act(crate::host::UserAction::Subscribe { channel_id: ch.to_string(), subscribe: !now })));
                    fx.invalidate(plx_machine::present::Provenance::Input);
                }
                return Handled::Yes;
            }
            ScreenEvent::Async(..) => fx.invalidate(plx_machine::present::Provenance::Lifecycle),
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
                    common::open_card_in(card, src.cards, fx);
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
        if self.channel().is_some() {
            out.push(GroupSpec {
                id: G_BTN,
                kind: plx_ui::screen::GroupKind::Row { wrap: false },
                seat: plx_ui::screen::Seat::First,
                reachable: plx_ui::screen::AxisMask::BOTH,
                edge: [plx_ui::screen::EdgeRule::Geometric; 4],
                extent: SUB_RECT,
                len: 1,
                elem: plx_ui::screen::ElemKind::Control,
            });
        }
        out.push(self.grid.group_spec(G, self.src(cx).cards.len()));
    }
    fn group_of(&self, key: &u32, _cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        Some(if *key == SUB_BTN { G_BTN } else { G })
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, YtHost>) -> Step<u32> {
        if key.elem == SUB_BTN {
            return match (dir, self.src(cx).cards.first()) {
                (Dir::Down, Some(c)) => Step::Move(FocusKey { entry: self.entry, elem: c.elem }),
                _ => Step::Edge,
            };
        }
        match self.grid.neighbour(&self.src(cx), key, dir) {
            Step::Edge if dir == Dir::Up && self.channel().is_some() => Step::Move(FocusKey { entry: self.entry, elem: SUB_BTN }),
            s => s,
        }
    }
    fn place(&self, key: &u32, cx: &Cx<'_, YtHost>, at: At) -> Option<Placed> {
        if *key == SUB_BTN {
            return Some(Placed { rect: SUB_RECT, rest_rect: SUB_RECT, clip: Rect::new(0.0, 0.0, 1920.0, 1080.0), index: Some(0) });
        }
        self.grid.place(cx, &self.src(cx), key, at)
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        let src = self.src(cx);
        if src.cards.iter().any(|c| c.elem == want.elem) || (want.elem == SUB_BTN && self.channel().is_some()) {
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
        if self.channel().is_some() {
            ui::stop(f, FocusKey { entry: self.entry, elem: SUB_BTN }, SUB_RECT, Rect::new(0.0, 0.0, 1920.0, 1080.0));
        }
        if let Some(ch) = self.channel() {
            let sub = cx.views.store.subscribed.get(ch).copied();
            let focused = cx.focus.current.is_some_and(|k| k.elem == SUB_BTN);
            let (label, bg) = match sub {
                Some(true) => ("Subscribed", [1.0, 1.0, 1.0, 0.16]),
                Some(false) => ("Subscribe", ui::accent()),
                None => ("\u{2026}", [1.0, 1.0, 1.0, 0.10]),
            };
            ui::fill(p, SUB_RECT, 25.0, if focused { ui::WHITE } else { bg });
            let ink = if focused { [0.0, 0.0, 0.0, 1.0] } else { ui::WHITE };
            ui::text(p, label, SUB_RECT.x + SUB_RECT.w / 2.0, SUB_RECT.y + 10.0, 26, ink, ui::ALIGN_CENTER, true);
        }
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

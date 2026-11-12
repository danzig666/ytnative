//! The root screen: a sidebar of sections (SmartTube style) and a grid of the selected section's
//! cards. Moving through the sidebar switches the section; OK on a card opens it; holding OK
//! opens the card's menu; the grid pages in more cards as you scroll.

use super::common;
use crate::host::{YtArg, YtFx, YtHost, YtMsg};
use crate::store::{FeedKey, Section};
use crate::ui::{self, CardsSrc, Row, VList};
use plx_machine::machine::{Canon, Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, LogicalState, Machine, NavOp, ScreenEvent};
use plx_ui::card_row::RowStyle;
use plx_ui::cards::{CardEvent, Grid, GridSpec};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use std::borrow::Cow;

pub const STYLE: RowStyle = RowStyle { w: 336.0, h: 189.0, gap: 40.0, ..RowStyle::HOME };
const SPEC: GridSpec = GridSpec::new(150.0, 96.0).columns(4, STYLE, 420.0);
const G_NAV: GroupId = GroupId(1);
const G_GRID: GroupId = GroupId(2);
const NAV_BASE: u32 = 0x4000_0000;

#[derive(Clone, Debug, PartialEq)]
enum NavItem {
    Section(Section),
    Search,
    Settings,
    Account,
}

struct State(String);
impl LogicalState for State {
    fn write(&self, c: &mut Canon) {
        c.str(&self.0);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&self.0);
    }
}

pub struct BrowseScreen {
    entry: EntryId,
    nav: VList,
    grid: Grid,
    section: Section,
    state: State,
}

fn nav_items(cx: &Cx<'_, YtHost>) -> Vec<NavItem> {
    let s = &cx.views.store.settings;
    let mut v: Vec<NavItem> = s.sections.iter().filter_map(|k| Section::from_key(k)).map(NavItem::Section).collect();
    if v.is_empty() {
        v.push(NavItem::Section(Section::Home));
    }
    v.insert(0, NavItem::Search);
    v.push(NavItem::Settings);
    v.push(NavItem::Account);
    v
}

impl BrowseScreen {
    pub fn new(entry: EntryId, cx: &Cx<'_, YtHost>) -> Self {
        let section = Section::from_key(&cx.views.store.settings.boot_section).unwrap_or(Section::Home);
        BrowseScreen {
            entry,
            nav: VList { entry, group: G_NAV, base: NAV_BASE, rect: Rect::new(24.0, 150.0, 340.0, 860.0), row_h: 68.0, text_size: 30 },
            grid: Grid::new(entry, SPEC),
            section,
            state: State(section.key().into()),
        }
    }

    fn key(&self) -> FeedKey {
        FeedKey::Section(self.section)
    }

    fn src<'a>(&self, cx: &Cx<'a, YtHost>) -> CardsSrc<'a> {
        let f = cx.views.store.feed(&self.key());
        CardsSrc { cards: f.map(|f| f.cards.as_slice()).unwrap_or(&[]), more: f.is_some_and(|f| f.continuation.is_some()) }
    }

    fn switch(&mut self, section: Section, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        if section == self.section {
            return;
        }
        self.section = section;
        self.state = State(section.key().into());
        self.grid = Grid::new(self.entry, SPEC);
        if cx.views.store.feed(&self.key()).is_none() {
            fx.push(Fx::App(YtFx::Load(self.key())));
        }
        fx.invalidate(plx_machine::present::Provenance::Input);
    }

    fn activate_nav(&mut self, item: &NavItem, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        match item {
            NavItem::Section(s) => {
                self.switch(*s, cx, fx);
                // Reload on explicit OK so the feed can be refreshed.
                fx.push(Fx::App(YtFx::Load(self.key())));
            }
            NavItem::Search => fx.push(Fx::Nav(NavOp::Push(YtArg::Search))),
            NavItem::Settings => fx.push(Fx::Nav(NavOp::Push(YtArg::Settings(crate::host::SettingsPage::Root)))),
            NavItem::Account => fx.push(Fx::Nav(NavOp::Push(YtArg::SignIn))),
        }
    }

    fn nav_index_of_section(&self, items: &[NavItem]) -> usize {
        items.iter().position(|i| *i == NavItem::Section(self.section)).unwrap_or(0)
    }
}

impl Machine<YtHost> for BrowseScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        let items = nav_items(cx);
        match ev {
            ScreenEvent::Mount => {
                if !cx.views.store.signed_in {
                    fx.push(Fx::Nav(NavOp::Push(YtArg::SignIn)));
                }
                if cx.views.store.feed(&self.key()).is_none() {
                    fx.push(Fx::App(YtFx::Load(self.key())));
                }
            }
            ScreenEvent::Uncover => {
                // Back from sign-in or settings: feeds may have been cleared.
                if cx.views.store.feed(&self.key()).is_none() {
                    fx.push(Fx::App(YtFx::Load(self.key())));
                }
                fx.invalidate(plx_machine::present::Provenance::Nav);
            }
            ScreenEvent::Async(_, YtMsg::Loaded(_) | YtMsg::Failed(..) | YtMsg::SignIn) => {
                fx.invalidate(plx_machine::present::Provenance::Lifecycle);
            }
            ScreenEvent::FocusMoved { to, .. } if to.elem >= NAV_BASE => {
                if let Some(NavItem::Section(s)) = self.nav.index(to.elem, items.len()).map(|i| items[i].clone()) {
                    self.switch(s, cx, fx);
                }
            }
            ScreenEvent::Activate(e) if *e >= NAV_BASE => {
                if let Some(i) = self.nav.index(*e, items.len()) {
                    let item = items[i].clone();
                    self.activate_nav(&item, cx, fx);
                }
                return Handled::Yes;
            }
            _ => {}
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
            Some(CardEvent::Want(_)) => fx.push(Fx::App(YtFx::More(self.key()))),
            None => {}
        }
        match ev {
            ScreenEvent::Input(_) => Handled::No,
            _ => Handled::Yes,
        }
    }
}

impl Focusable<YtHost> for BrowseScreen {
    fn groups(&self, cx: &Cx<'_, YtHost>, out: &mut Vec<GroupSpec>) {
        out.push(self.nav.spec(nav_items(cx).len()));
        out.push(self.grid.group_spec(G_GRID, self.src(cx).cards.len()));
    }
    fn group_of(&self, key: &u32, _cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        Some(if *key >= NAV_BASE { G_NAV } else { G_GRID })
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, YtHost>) -> Step<u32> {
        let items = nav_items(cx);
        if key.elem >= NAV_BASE {
            if dir == Dir::Right && !self.src(cx).cards.is_empty() {
                return Step::Move(FocusKey { entry: self.entry, elem: self.src(cx).cards[0].elem });
            }
            return self.nav.neighbour(key, dir, items.len());
        }
        match self.grid.neighbour(&self.src(cx), key, dir) {
            Step::Edge if dir == Dir::Left => Step::Move(self.nav.key(self.nav_index_of_section(&items))),
            s => s,
        }
    }
    fn place(&self, key: &u32, cx: &Cx<'_, YtHost>, at: At) -> Option<Placed> {
        if *key >= NAV_BASE {
            let n = nav_items(cx).len();
            return self.nav.place(*key, n, self.nav.focused(cx, n));
        }
        self.grid.place(cx, &self.src(cx), key, at)
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        let items = nav_items(cx);
        let src = self.src(cx);
        let valid = if want.elem >= NAV_BASE { self.nav.index(want.elem, items.len()).is_some() } else { src.cards.iter().any(|c| c.elem == want.elem) };
        if valid {
            return want;
        }
        match src.cards.first() {
            Some(c) => FocusKey { entry: self.entry, elem: c.elem },
            None => self.nav.key(self.nav_index_of_section(&items)),
        }
    }
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        let src = self.src(cx);
        if g == G_GRID {
            if let Some(k) = self.grid.seat(&src, from) {
                return k;
            }
        }
        match src.cards.first() {
            Some(c) if g == G_GRID => FocusKey { entry: self.entry, elem: c.elem },
            _ => self.nav.key(self.nav_index_of_section(&nav_items(cx))),
        }
    }
}

impl Screen<YtHost> for BrowseScreen {
    fn name(&self) -> &'static str {
        "browse"
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
        // Sidebar
        ui::fill(p, Rect::new(0.0, 0.0, 390.0, 1080.0), 0.0, [0.0, 0.0, 0.0, 0.35]);
        common::logo(p, 48.0, 52.0);
        let items = nav_items(cx);
        let rows: Vec<Row> = items
            .iter()
            .map(|i| match i {
                NavItem::Section(s) => Row::new(s.title()).active(*s == self.section),
                NavItem::Search => Row::new("\u{1F50D}  Search"),
                NavItem::Settings => Row::new("Settings"),
                NavItem::Account => Row::new(match (&cx.views.store.account, cx.views.store.signed_in) {
                    (Some(a), true) => ui::ellipsize(a, 18),
                    (None, true) => "Account".into(),
                    _ => "Sign in".into(),
                }),
            })
            .collect();
        self.nav.draw(p, &rows, self.nav.focused(cx, items.len()));

        // Content
        let feed = cx.views.store.feed(&self.key());
        ui::text(p, self.section.title(), 420.0, 56.0, 44, ui::WHITE, ui::ALIGN_LEFT, true);
        let src = self.src(cx);
        if src.cards.is_empty() {
            common::empty_state(p, feed, Rect::new(420.0, 150.0, 1460.0, 800.0));
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

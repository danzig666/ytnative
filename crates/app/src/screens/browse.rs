//! The root screen: a sidebar of sections (SmartTube style) and a grid of the selected section's
//! cards. Moving through the sidebar switches the section; OK on a card opens it; holding OK
//! opens the card's menu; the grid pages in more cards as you scroll.

use super::common;
use crate::host::{YtArg, YtFx, YtHost, YtMsg};
use crate::store::{FeedKey, Section};
use crate::ui::{self, CardsSrc, Row, VList};
use plx_machine::machine::{Canon, Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, LogicalState, Machine, NavOp, ScreenEvent};
use plx_ui::card_row::RowStyle;
use plx_ui::cards::{CardEvent, Grid, GridSpec, Kind, SectionSpec, Stack, StackEvent, StackPage};
use plx_ui::screen::Part;
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use std::borrow::Cow;

pub const STYLE: RowStyle = RowStyle { w: 336.0, h: 189.0, gap: 40.0, ..RowStyle::HOME };
/// Width the cards of a row share (right of the sidebar).
const ROW_W: f32 = 1768.0;
/// Left edge of the content (the sidebar is hidden and slides in over it).
const LEFT: f32 = 96.0;
/// Sidebar width.
const NAV_W: f32 = 390.0;

fn spec() -> GridSpec {
    GridSpec::new(150.0, 96.0).columns(ui::card_cols(), ui::card_style(ui::card_cols(), ROW_W, 40.0), LEFT)
}
const G_NAV: GroupId = GroupId(1);
const G_GRID: GroupId = GroupId(2);
const NAV_BASE: u32 = 0x4000_0000;
const T_SWITCH: plx_machine::machine::TimerId = plx_machine::machine::TimerId(31);
const T_BACK: plx_machine::machine::TimerId = plx_machine::machine::TimerId(32);
/// Two BACK presses this close together on the sidebar exit the app.
const BACK_TWICE_MS: u32 = 700;
/// Shelf rows (Home, topics): 16:9 cards starting right of the sidebar.
const SHELF_STYLE: RowStyle = RowStyle { w: 336.0, h: 189.0, gap: 40.0, margin_x: LEFT, ..RowStyle::HOME };
/// Shelf styles for 3, 4 and 5 cards per row (sections hold a `&'static` style).
static SHELF_STYLES: std::sync::LazyLock<[RowStyle; 3]> = std::sync::LazyLock::new(|| {
    [3usize, 4, 5].map(|c| RowStyle { margin_x: LEFT, ..ui::card_style(c, ROW_W, 40.0) })
});

fn shelf_style() -> &'static RowStyle {
    &SHELF_STYLES[ui::card_cols() - 3]
}
const SPACER: usize = usize::MAX;

/// The section's titled rows as a plx `Stack` page.
struct Rows {
    key: FeedKey,
}

impl StackPage<YtHost> for Rows {
    type Key = usize;
    type Cards<'a> = CardsSrc<'a>;
    fn revision(&self, cx: &Cx<'_, YtHost>) -> u64 {
        cx.views.store.feed(&self.key).map_or(0, |f| ((f.shelves.len() as u64) << 32) | f.gen as u64)
    }
    fn sections(&self, cx: &Cx<'_, YtHost>, out: &mut Vec<SectionSpec<usize>>) {
        out.push(SectionSpec::new(SPACER, Kind::Custom { height: 110.0, focusable: false }, GroupId(90)));
        if let Some(f) = cx.views.store.feed(&self.key) {
            for i in 0..f.shelves.len() {
                out.push(SectionSpec::new(i, Kind::Shelf { style: shelf_style(), heading: 56.0 }, GroupId(100 + i as u32)));
            }
        }
    }
    fn fallback(&self, _cx: &Cx<'_, YtHost>, out: &mut Vec<usize>) {
        out.push(0);
    }
    fn cards<'a>(&'a self, cx: &'a Cx<'_, YtHost>, k: usize) -> Option<CardsSrc<'a>> {
        let f = cx.views.store.feed(&self.key)?;
        let r = f.shelves.get(k)?;
        // A row with a token asks for more as focus nears its end (plx's look-ahead).
        Some(CardsSrc { cards: &f.cards[r.range.clone()], more: r.continuation.is_some() })
    }
    /// Room under every shelf for the caption of the shelf below (plx asserts at least this much).
    fn shelf_foot(&self, _cx: &Cx<'_, YtHost>, _k: usize) -> f32 {
        plx_ui::consts::UNDER_LABEL_AIR
    }
    fn draw_heading(&self, k: usize, f: &mut DrawFrame<'_, '_, YtHost>, r: Rect, _lift: f32) {
        if let Some(row) = f.cx.views.store.feed(&self.key).and_then(|fd| fd.shelves.get(k)) {
            // An untitled row in a titled page reads as YouTube's own suggestions (as SmartTube).
            let title = if row.title.is_empty() { crate::i18n::tr("Suggestions") } else { row.title.as_str() };
            ui::text(f.painter, title, SHELF_STYLE.margin_x, r.y + 8.0, 32, ui::WHITE, ui::ALIGN_LEFT, true);
        }
    }
}

/// The sidebar icon of a section (SmartTube's set, drawn as line icons).
fn section_icon(s: Section) -> ui::icon::Kind {
    use ui::icon::Kind as K;
    match s {
        Section::Home => K::Home,
        Section::Subscriptions => K::Subs,
        Section::Channels => K::Users,
        Section::History => K::History,
        Section::Library => K::Library,
        Section::Music => K::Music,
        Section::Gaming => K::Gamepad,
        Section::News => K::News,
        Section::Podcasts => K::Mic,
        Section::Live => K::Live,
        Section::Movies => K::Film,
        Section::Sports => K::Trophy,
        Section::Trending => K::Flame,
    }
}

#[derive(Clone, Debug, PartialEq)]
enum NavItem {
    Section(Section),
    /// A pinned channel or playlist: (title, browse id, params).
    Pinned(String, String, Option<String>),
    /// A local channel group: (id, title).
    Group(String, String),
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
    stack: Stack<usize>,
    rows: Rows,
    section: Section,
    /// Sidebar slide position (0 hidden, 1 shown) and the clock it was last stepped at.
    nav_t: f32,
    /// Last BACK on the sidebar (a second one soon after exits).
    back_at: u32,
    exit_armed: bool,
    nav_ms: u32,
    /// The sidebar was asked for (Left from the leftmost tile, or BACK): focus may stay there.
    nav_open: std::cell::Cell<bool>,
    /// A focus move the screen asks for (taken by the next `reconcile`).
    focus_req: std::cell::Cell<Option<u32>>,
    /// The sidebar row focus was last on.
    nav_last: std::cell::Cell<u32>,
    /// The card focus was last on (where a closed sidebar returns it).
    content_last: std::cell::Cell<u32>,
    /// Section the sidebar focus rests on, switched to when `T_SWITCH` fires.
    pending: Option<Section>,
    state: State,
    /// Cards per row the grid / rows were built for.
    cols: usize,
}

fn nav_items(cx: &Cx<'_, YtHost>) -> Vec<NavItem> {
    let s = &cx.views.store.settings;
    let mut v: Vec<NavItem> = s.sections.iter().filter_map(|k| Section::from_key(k)).map(NavItem::Section).collect();
    if v.is_empty() {
        v.push(NavItem::Section(Section::Home));
    }
    v.extend(s.pinned.iter().map(|(t, id, p)| NavItem::Pinned(t.clone(), id.clone(), p.clone())));
    v.extend(cx.views.store.groups.iter().map(|(id, t)| NavItem::Group(id.clone(), t.clone())));
    if !s.child_mode {
        v.insert(0, NavItem::Search);
    }
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
            grid: Grid::new(entry, spec()),
            cols: ui::card_cols(),
            stack: Stack::new(entry).clipped(Rect::new(0.0, 0.0, 1920.0, 1080.0)),
            rows: Rows { key: FeedKey::Section(section) },
            section,
            pending: None,
            nav_t: 0.0,
            back_at: 0,
            exit_armed: false,
            nav_ms: 0,
            nav_open: std::cell::Cell::new(false),
            focus_req: std::cell::Cell::new(None),
            nav_last: std::cell::Cell::new(NAV_BASE),
            content_last: std::cell::Cell::new(0),
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
        self.grid = Grid::new(self.entry, spec());
        self.cols = ui::card_cols();
        self.stack = Stack::new(self.entry).clipped(Rect::new(0.0, 0.0, 1920.0, 1080.0));
        self.rows = Rows { key: self.key() };
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
            NavItem::Group(id, _) => fx.push(Fx::Nav(NavOp::Push(YtArg::Feed(FeedKey::Group(id.clone()))))),
            NavItem::Pinned(_, id, params) => fx.push(Fx::Nav(NavOp::Push(YtArg::Feed(FeedKey::Browse { id: id.clone(), params: params.clone() })))),
            NavItem::Settings => {
                let locked = !cx.views.store.settings.settings_pin.is_empty();
                fx.push(Fx::Nav(NavOp::Push(if locked {
                    YtArg::Pin(crate::host::PinPurpose::OpenSettings)
                } else {
                    YtArg::Settings(crate::host::SettingsPage::Root)
                })));
            }
            NavItem::Account => fx.push(Fx::Nav(NavOp::Push(if cx.views.store.signed_in { YtArg::Accounts } else { YtArg::SignIn }))),
        }
    }

    fn rows_mode(&self, cx: &Cx<'_, YtHost>) -> bool {
        cx.views.store.feed(&self.key()).is_some_and(|f| f.has_rows())
    }

    fn nav_index_of_section(&self, items: &[NavItem]) -> usize {
        items.iter().position(|i| *i == NavItem::Section(self.section)).unwrap_or(0)
    }

    /// Left past the leftmost tile: into the sidebar when "Left opens the sidebar" is on.
    fn left_edge(&self, key: FocusKey<u32>, items: &[NavItem], cx: &Cx<'_, YtHost>) -> Step<u32> {
        if cx.views.store.settings.left_opens_sidebar {
            self.nav_open.set(true);
            Step::Move(self.nav.key(self.nav_index_of_section(items)))
        } else {
            Step::Move(key)
        }
    }
}

impl Machine<YtHost> for BrowseScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        if let Some(a) = common::press_commit_as_activate(ev, cx, |e| e >= NAV_BASE) {
            return self.step(&a, cx, fx);
        }
        let items = nav_items(cx);
        if let ScreenEvent::FocusMoved { to, by, .. } = ev {
            if to.elem >= NAV_BASE {
                self.nav_last.set(to.elem);
            } else if !self.nav_open.get() {
                self.content_last.set(to.elem);
            }
            if to.elem >= NAV_BASE {
            } else if self.nav_open.get() && *by == plx_machine::machine::By::Pointer {
                // Hovering a card behind the open sidebar: keep the sidebar and its selection.
                self.focus_req.set(Some(self.nav_last.get()));
                return Handled::Yes;
            } else {
                self.nav_open.set(false);
            }
        }
        // BACK on the content brings the sidebar; BACK on the sidebar leaves the app.
        if let ScreenEvent::Input(plx_machine::machine::InputEvent {
            kind: plx_machine::machine::InputKind::Key { key: plx_machine::machine::Key::Back, edge, .. }, ..
        }) = ev {
            let on_nav = cx.focus.current.is_some_and(|k| k.entry == self.entry && k.elem >= NAV_BASE);
            if on_nav {
                // BACK twice on the sidebar leaves the app; once only warns.
                if *edge == plx_machine::machine::Edge::Down {
                    let now = cx.views.now_ms;
                    if self.back_at != 0 && now.wrapping_sub(self.back_at) < BACK_TWICE_MS {
                        self.exit_armed = true;
                        return Handled::No;
                    }
                    // A single BACK closes the sidebar once the double-press window has passed.
                    self.back_at = now;
                    fx.push(Fx::CancelTimer(T_BACK));
                    fx.push(Fx::Timer { id: T_BACK, after_ms: BACK_TWICE_MS });
                    return Handled::Yes;
                }
                return if self.exit_armed { Handled::No } else { Handled::Yes };
            }
            if !on_nav {
                if *edge == plx_machine::machine::Edge::Down {
                    self.nav_open.set(true);
                    self.focus_req.set(Some(self.nav.elem(self.nav_index_of_section(&items))));
                    fx.invalidate(plx_machine::present::Provenance::Input);
                }
                return Handled::Yes;
            }
        }
        if let ScreenEvent::Input(plx_machine::machine::InputEvent { kind: plx_machine::machine::InputKind::Click { x, .. }, .. }) = ev {
            if self.nav_open.get() && *x > NAV_W {
                self.nav_open.set(false);
                fx.invalidate(plx_machine::present::Provenance::Input);
                return Handled::Yes;
            }
        }
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
                // Back from settings with another card size: rebuild the layouts.
                if self.cols != ui::card_cols() {
                    self.cols = ui::card_cols();
                    self.grid = Grid::new(self.entry, spec());
                    self.stack = Stack::new(self.entry).clipped(Rect::new(0.0, 0.0, 1920.0, 1080.0));
                }
                // Back from sign-in or settings: feeds may have been cleared.
                if cx.views.store.feed(&self.key()).is_none() {
                    fx.push(Fx::App(YtFx::Load(self.key())));
                }
                fx.invalidate(plx_machine::present::Provenance::Nav);
            }
            ScreenEvent::Async(_, YtMsg::Loaded(_) | YtMsg::Failed(..) | YtMsg::SignIn) => {
                fx.invalidate(plx_machine::present::Provenance::Lifecycle);
            }
            // Keys: switch once the selection rests (stepping through the sidebar must not rebuild
            // the page and start a load for every section passed). Pointer hover: never; a click
            // switches (`Activate`).
            ScreenEvent::FocusMoved { to, by, .. } if to.elem >= NAV_BASE => {
                if *by != plx_machine::machine::By::Pointer {
                    if let Some(NavItem::Section(s)) = self.nav.index(to.elem, items.len()).map(|i| items[i].clone()) {
                        self.pending = Some(s);
                        fx.push(Fx::CancelTimer(T_SWITCH));
                        fx.push(Fx::Timer { id: T_SWITCH, after_ms: 280 });
                    }
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
            }
            ScreenEvent::Timer(T_BACK) => {
                let on_nav = cx.focus.current.is_some_and(|k| k.entry == self.entry && k.elem >= NAV_BASE);
                if on_nav && self.nav_open.get() {
                    self.nav_open.set(false);
                    self.focus_req.set(Some(self.content_last.get()));
                    fx.invalidate(plx_machine::present::Provenance::Input);
                }
                self.back_at = 0;
                return Handled::Yes;
            }
            ScreenEvent::Timer(T_SWITCH) => {
                // Only if focus is still on that sidebar row.
                let still = cx.focus.current.and_then(|k| self.nav.index(k.elem, items.len())).map(|i| items[i].clone());
                if let (Some(s), Some(NavItem::Section(f))) = (self.pending.take(), still) {
                    if s == f {
                        self.switch(s, cx, fx);
                    }
                }
                return Handled::Yes;
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
        if self.rows_mode(cx) {
            // Near the last rows: ask for the next page of shelves.
            if let ScreenEvent::FocusMoved { to, .. } = ev {
                if let Some(f) = cx.views.store.feed(&self.key()) {
                    let idx = f.cards.iter().position(|c| c.elem == to.elem);
                    let row = idx.and_then(|i| f.shelves.iter().position(|r| r.range.contains(&i)));
                    if row.is_some_and(|r| r + 3 >= f.shelves.len()) && f.continuation.is_some() {
                        fx.push(Fx::App(YtFx::More(self.key())));
                    }
                }
            }
            match self.stack.on(&self.rows, ev, cx, fx) {
                Some(StackEvent::Card(_, CardEvent::Activate(e))) => {
                    if let (Some(card), Some(f)) = (cx.views.store.card(e), cx.views.store.feed(&self.key())) {
                        let row = f.shelves.iter().find(|r| f.cards[r.range.clone()].iter().any(|c| c.elem == e));
                        let list = row.map(|r| &f.cards[r.range.clone()]).unwrap_or(&f.cards[..]);
                        common::open_card_in(card, list, fx);
                    }
                    return Handled::Yes;
                }
                Some(StackEvent::Card(_, CardEvent::Hold(e))) => {
                    fx.push(Fx::Nav(NavOp::Present(YtArg::Menu(e))));
                    return Handled::Yes;
                }
                Some(StackEvent::Card(k, CardEvent::Want(_))) if k != SPACER => {
                    fx.push(Fx::App(YtFx::MoreShelf { key: self.key(), row: k }));
                    return Handled::Yes;
                }
                _ => {}
            }
            return match ev {
                ScreenEvent::Input(_) => Handled::No,
                _ => Handled::Yes,
            };
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
        // The sidebar is entered sideways only: its tall extent would otherwise win the geometric
        // search for Up/Down from a shelf over the shelf above or below.
        let mut nav = self.nav.spec(nav_items(cx).len());
        nav.reachable = plx_ui::screen::AxisMask::HORIZONTAL;
        nav.edge = [plx_ui::screen::EdgeRule::Stop; 4];
        out.push(nav);
        if self.rows_mode(cx) {
            self.stack.view(&self.rows).groups(cx, out);
        } else {
            out.push(self.grid.group_spec(G_GRID, self.src(cx).cards.len()));
        }
    }
    fn group_of(&self, key: &u32, cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        if *key >= NAV_BASE {
            return Some(G_NAV);
        }
        if self.rows_mode(cx) {
            return self.stack.view(&self.rows).group_of(key, cx);
        }
        Some(G_GRID)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, YtHost>) -> Step<u32> {
        let items = nav_items(cx);
        // While the sidebar is open, a move starts from its row even if the pointer just touched a
        // card behind it (the correction lands a frame later).
        if self.nav_open.get() && key.elem < NAV_BASE {
            return self.neighbour(FocusKey { entry: self.entry, elem: self.nav_last.get() }, dir, cx);
        }
        if self.rows_mode(cx) && key.elem < NAV_BASE {
            // Left on the leftmost tile opens the sidebar when that option is on; otherwise it
            // stays there and the sidebar opens only with BACK.
            return match self.stack.view(&self.rows).neighbour(key, dir, cx) {
                Step::Edge if dir == Dir::Left => self.left_edge(key, &items, cx),
                s => s,
            };
        }
        if key.elem >= NAV_BASE {
            if dir == Dir::Right && !self.src(cx).cards.is_empty() {
                return Step::Move(FocusKey { entry: self.entry, elem: self.src(cx).cards[0].elem });
            }
            return self.nav.neighbour(key, dir, items.len());
        }
        match self.grid.neighbour(&self.src(cx), key, dir) {
            Step::Edge if dir == Dir::Left => self.left_edge(key, &items, cx),
            s => s,
        }
    }
    fn place(&self, key: &u32, cx: &Cx<'_, YtHost>, at: At) -> Option<Placed> {
        if *key >= NAV_BASE {
            let n = nav_items(cx).len();
            return self.nav.place(*key, n, self.nav.focused(cx, n));
        }
        if self.rows_mode(cx) {
            return self.stack.view(&self.rows).place(key, cx, at);
        }
        self.grid.place(cx, &self.src(cx), key, at)
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        let items = nav_items(cx);
        if let Some(e) = self.focus_req.take() {
            return FocusKey { entry: self.entry, elem: e };
        }
        // Focus on the sidebar without it being asked for (start-up, before the content loaded):
        // move to the content once it is there. The sidebar opens only on request.
        if want.elem >= NAV_BASE && !self.nav_open.get() {
            if let Some(c) = self.src(cx).cards.first() {
                return FocusKey { entry: self.entry, elem: c.elem };
            }
        }
        if self.rows_mode(cx) && want.elem < NAV_BASE {
            return self.stack.view(&self.rows).reconcile(want, cx);
        }
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
        if self.rows_mode(cx) && g != G_NAV {
            return self.stack.view(&self.rows).seat(g, from, cx);
        }
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
        // Content
        let feed = cx.views.store.feed(&self.key());
        let src = self.src(cx);
        let on_nav = cx.focus.current.is_some_and(|k| k.entry == self.entry && k.elem >= NAV_BASE);
        ui::PAGE_SCROLLS.store(self.rows_mode(cx) && !(self.nav_open.get() && on_nav), std::sync::atomic::Ordering::Relaxed);
        if self.rows_mode(cx) {
            let mut view = self.stack.view(&self.rows);
            ui::reset_focused_caption();
            Part::draw(&mut view, f, Rect::new(0.0, 0.0, 1920.0, 1080.0));
            ui::draw_focused_caption(p, f.measure, 20.0);
            ui::draw_row_arrows(p, LEFT, 1920.0 - LEFT + 40.0);
            if !self.nav_open.get() {
                view.record_stops(f);
            }
        } else if src.cards.is_empty() {
            common::empty_state(p, feed, Rect::new(LEFT, 150.0, 1920.0 - 2.0 * LEFT, 800.0));
        } else {
            ui::reset_focused_caption();
            self.grid.draw(f, p, &src);
            ui::draw_focused_caption(p, f.measure, 20.0);
            if !self.nav_open.get() {
                self.grid.record_stops(f, p, &src);
            }
        }
        // Sidebar
        // Page title over a header band, after the content: scrolled rows go under it.
        ui::header_band(p, Rect::new(0.0, 0.0, 1920.0, 118.0));
        ui::text(p, self.section.title(), LEFT, 30.0, 44, ui::WHITE, ui::ALIGN_LEFT, true);
        if cx.views.store.settings.clock_on_pages {
            common::clock_block(p, cx, 1824.0, 22.0);
        }
        // The sidebar: hidden; slides in over the content while it has focus (Left from the first
        // tile) or the pointer rests on the screen's left edge, and out again after.
        let open = self.nav_open.get() && cx.focus.current.is_some_and(|k| k.entry == self.entry && k.elem >= NAV_BASE);
        let now = cx.views.now_ms;
        let dt = now.wrapping_sub(self.nav_ms).min(100) as f32;
        self.nav_ms = now;
        let target = if open { 1.0 } else { 0.0 };
        self.nav_t += (target - self.nav_t) * (1.0 - (-dt / 60.0).exp());
        if (target - self.nav_t).abs() < 0.004 {
            self.nav_t = target;
        } else {
            ui::animating();
        }
        let t = self.nav_t;
        ui::set_sidebar_shown(t > 0.0);
        if t > 0.0 {
            ui::fill(p, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.0, 0.0, 0.0, 0.45 * t]);
        }
        let p = p.translate(-NAV_W * (1.0 - t), 0.0);
        let bg = ui::bg_rgb();
        let bg = if bg == (0.0, 0.0, 0.0) { (0.07, 0.07, 0.08) } else { bg };
        ui::fill(p, Rect::new(0.0, 0.0, NAV_W, 1080.0), 0.0, [bg.0 * 0.65, bg.1 * 0.65, bg.2 * 0.65, 1.0]);
        common::logo(p, 48.0, 52.0);
        let items = nav_items(cx);
        let rows: Vec<Row> = items
            .iter()
            .map(|i| {
                use ui::icon::Kind as K;
                match i {
                    NavItem::Section(s) => Row::new(s.title()).active(*s == self.section).icon(section_icon(*s)),
                    NavItem::Search => Row::new("Search").icon(K::Search),
                    NavItem::Pinned(t, ..) => Row::new(ui::ellipsize(t, 16)).icon(K::Star),
                    NavItem::Group(_, t) => Row::new(ui::ellipsize(t, 16)).icon(K::Folder),
                    NavItem::Settings => Row::new("Settings").icon(K::Gear),
                    NavItem::Account => Row::new(match (&cx.views.store.account, cx.views.store.signed_in) {
                        (Some(a), true) if !a.trim().is_empty() => ui::ellipsize(a, 16),
                        (_, true) => "Account".into(),
                        _ => "Sign in".into(),
                    })
                    .icon(K::User),
                }
            })
            .collect();
        self.nav.draw(p, &rows, self.nav.focused(cx, items.len()));
        if t > 0.9 {
            self.nav.stops(f, items.len());
        }

        common::toast(f.painter, cx);
    }
    fn redraw_focused(&self, f: &mut DrawFrame<'_, '_, YtHost>, focus: Option<FocusKey<u32>>) {
        if self.rows_mode(f.cx) {
            self.stack.view(&self.rows).redraw_focused(f, focus);
            return;
        }
        let p = f.painter;
        let src = self.src(f.cx);
        self.grid.redraw_focused(f, p, &src, focus);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

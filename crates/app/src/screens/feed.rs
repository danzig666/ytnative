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
/// Where the grid starts: lower on a channel page and a playlist's page, whose buttons (Subscribe;
/// Play all and Shuffle) have a line of their own under the title.
fn grid_top(key: &FeedKey) -> f32 {
    if is_channel(key) || playlist_of(key).is_some() { 212.0 } else { 170.0 }
}

/// A playlist's page: the YouTube playlist's id ("VL" taken off), or None for a playlist on this
/// TV. Its videos play as a playlist (`play_queue`), and the page offers Play all and Shuffle.
fn playlist_of(key: &FeedKey) -> Option<Option<&str>> {
    match key {
        FeedKey::Browse { id, .. } if id.len() > 2 && id.starts_with("VL") => Some(Some(&id[2..])),
        FeedKey::Local(_) => Some(None),
        _ => None,
    }
}

fn is_channel(key: &FeedKey) -> bool {
    matches!(key, FeedKey::Browse { id, .. } if id.starts_with("UC"))
}

fn spec(top: f32) -> GridSpec {
    GridSpec::new(top, 96.0).columns(ui::card_cols(), ui::card_style(ui::card_cols(), 1652.0, 44.0), 112.0)
}
const G: GroupId = GroupId(1);
const G_BTN: GroupId = GroupId(2);
const SUB_BTN: u32 = 0x7900_0000;
const SUB_RECT: Rect = Rect::new(112.0, 112.0, 300.0, 50.0);
/// A playlist's Play all and Shuffle, on the line where a channel has Subscribe.
const PLAY_BTN: u32 = 0x7900_0001;
const SHUFFLE_BTN: u32 = 0x7900_0002;
const PLAY_RECT: Rect = Rect::new(112.0, 112.0, 300.0, 50.0);
const SHUFFLE_RECT: Rect = Rect::new(436.0, 112.0, 300.0, 50.0);

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
        let top = grid_top(&key);
        FeedScreen { entry, state: State(format!("{key:?}")), key, grid: Grid::new(entry, spec(top)) }
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

    /// The buttons under the title, left to right: Subscribe on a channel's page, Play all and
    /// Shuffle on a playlist's (once it has videos).
    fn buttons(&self, cx: &Cx<'_, YtHost>) -> Vec<(u32, Rect)> {
        if self.channel().is_some() {
            return vec![(SUB_BTN, SUB_RECT)];
        }
        if playlist_of(&self.key).is_some() && self.src(cx).cards.iter().any(|c| c.video_id().is_some()) {
            return vec![(PLAY_BTN, PLAY_RECT), (SHUFFLE_BTN, SHUFFLE_RECT)];
        }
        Vec::new()
    }

    /// Play all / Shuffle: the playlist from its first video, or in a random order (each video
    /// once; the rest of a YouTube playlist joins the order as it loads).
    fn play_all(&self, shuffle: bool, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        let Some(pl) = playlist_of(&self.key) else { return };
        let mut q = common::playlist_queue(self.src(cx).cards, pl, &self.title(cx));
        if shuffle {
            q.shuffle(None, crate::play_queue::seed(cx.views.now_ms));
        }
        let Some(first) = q.at(0).cloned() else { return };
        fx.push(Fx::App(YtFx::SetQueue(q)));
        fx.push(Fx::Nav(NavOp::Push(YtArg::Player { video_id: first, playlist: pl.map(str::to_string) })));
    }

    /// The page title, ready to draw: the app's own words translated, a channel's name and the
    /// query as they are.
    fn title(&self, cx: &Cx<'_, YtHost>) -> String {
        use crate::i18n::tr;
        if let Some(t) = cx.views.store.feed(&self.key).map(|f| f.title.clone()).filter(|t| !t.is_empty()) {
            // YouTube's own pages come back with English names ("History", "Watch later"), which
            // the table translates; a channel's name and a playlist on this TV are shown as they are.
            return if self.channel().is_some() || matches!(self.key, FeedKey::Local(_)) { t } else { tr(&t).to_string() };
        }
        match &self.key {
            FeedKey::Search { query, .. } => tr("Results for \u{201C}{}\u{201D}").replace("{}", query),
            FeedKey::Browse { id, .. } if id.starts_with("VL") => tr("Playlist").into(),
            FeedKey::Browse { .. } => tr("Channel").into(),
            FeedKey::Section(s) => tr(s.title()).into(),
            FeedKey::Related(_) => tr("Up next").into(),
            FeedKey::Group(id) => cx.views.store.groups.iter().find(|g| &g.0 == id).map(|g| g.1.clone()).unwrap_or_else(|| tr("Group").into()),
            FeedKey::Local(id) => cx.views.store.local.get(id).map(|l| l.title.clone()).unwrap_or_else(|| tr("Playlist").into()),
        }
    }
}

/// The search filters in effect, in words of the interface language ("This week · Videos").
fn filters_summary(f: &yt_core::search_filters::Filters) -> String {
    use crate::i18n::tr;
    use yt_core::search_filters::*;
    let mut parts: Vec<&str> = Vec::new();
    parts.extend(match f.upload_date {
        UploadDate::Any => None,
        UploadDate::Hour => Some("Last hour"),
        UploadDate::Today => Some("Today"),
        UploadDate::Week => Some("This week"),
        UploadDate::Month => Some("This month"),
        UploadDate::Year => Some("This year"),
    });
    parts.extend(match f.kind {
        Kind::Any => None,
        Kind::Video => Some("Videos"),
        Kind::Channel => Some("Channels"),
        Kind::Playlist => Some("Playlists"),
        Kind::Movie => Some("Movies"),
    });
    parts.extend(match f.duration {
        Duration::Any => None,
        Duration::Short => Some("Under 4 minutes"),
        Duration::Medium => Some("4 to 20 minutes"),
        Duration::Long => Some("Over 20 minutes"),
    });
    if f.live {
        parts.push("Live");
    }
    if f.hd_4k {
        parts.push("4K");
    }
    if f.hdr {
        parts.push("HDR");
    }
    parts.extend(match f.sort {
        Sort::Relevance | Sort::Rating => None,
        Sort::UploadDate => Some("newest first"),
        Sort::ViewCount => Some("most viewed"),
    });
    parts.iter().map(|p| tr(p)).collect::<Vec<_>>().join(" \u{00B7} ")
}

impl FeedScreen {
    /// The filters hint drawn on the title's line (search results only).
    fn filters_hint(&self, cx: &Cx<'_, YtHost>) -> Option<String> {
        let FeedKey::Search { params, .. } = &self.key else { return None };
        let f = &cx.views.store.search_filters;
        let label = format!("\u{25B2} {}", crate::i18n::tr("Filters"));
        Some(if params.is_some() && !f.is_default() { format!("{}   {label}", filters_summary(f)) } else { label })
    }
}

impl Machine<YtHost> for FeedScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        if let Some(a) = super::common::press_commit_as_activate(ev, cx, |e| [SUB_BTN, PLAY_BTN, SHUFFLE_BTN].contains(&e)) {
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
            ScreenEvent::Activate(e @ (PLAY_BTN | SHUFFLE_BTN)) => {
                self.play_all(*e == SHUFFLE_BTN, cx, fx);
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
            let at_top = cx.focus.current.and_then(|k| src.cards.iter().position(|c| c.elem == k.elem)).map_or(true, |i| i < ui::card_cols());
            if at_top {
                fx.push(Fx::Nav(NavOp::Present(YtArg::SearchFilters(query.clone()))));
                return Handled::Yes;
            }
        }
        let src = self.src(cx);
        match self.grid.on(ev, cx, &src, fx) {
            Some(CardEvent::Activate(e)) => {
                if let Some(card) = cx.views.store.card(e) {
                    if let Some(pl) = playlist_of(&self.key) {
                        common::open_card_in_playlist(card, src.cards, pl, &self.title(cx), fx);
                    } else {
                        common::open_card_in(card, src.cards, fx);
                    }
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
        let buttons = self.buttons(cx);
        if let (Some(first), Some(last)) = (buttons.first(), buttons.last()) {
            out.push(GroupSpec {
                id: G_BTN,
                kind: plx_ui::screen::GroupKind::Row { wrap: false },
                seat: plx_ui::screen::Seat::First,
                reachable: plx_ui::screen::AxisMask::BOTH,
                edge: [plx_ui::screen::EdgeRule::Geometric; 4],
                extent: Rect::new(first.1.x, first.1.y, last.1.x + last.1.w - first.1.x, first.1.h),
                len: buttons.len(),
                elem: plx_ui::screen::ElemKind::Control,
            });
        }
        out.push(self.grid.group_spec(G, self.src(cx).cards.len()));
    }
    fn group_of(&self, key: &u32, _cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        Some(if [SUB_BTN, PLAY_BTN, SHUFFLE_BTN].contains(key) { G_BTN } else { G })
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, YtHost>) -> Step<u32> {
        let buttons = self.buttons(cx);
        if let Some(i) = buttons.iter().position(|b| b.0 == key.elem) {
            let to = |e: u32| Step::Move(FocusKey { entry: self.entry, elem: e });
            return match (dir, self.src(cx).cards.first()) {
                (Dir::Down, Some(c)) => to(c.elem),
                (Dir::Left, _) if i > 0 => to(buttons[i - 1].0),
                (Dir::Right, _) if i + 1 < buttons.len() => to(buttons[i + 1].0),
                _ => Step::Edge,
            };
        }
        let src = self.src(cx);
        match self.grid.neighbour(&src, key, dir) {
            Step::Edge if dir == Dir::Up && !buttons.is_empty() => Step::Move(FocusKey { entry: self.entry, elem: buttons[0].0 }),
            Step::Edge if dir == Dir::Right => common::next_row_start(src.cards, key).map_or(Step::Edge, Step::Move),
            s => s,
        }
    }
    fn place(&self, key: &u32, cx: &Cx<'_, YtHost>, at: At) -> Option<Placed> {
        if let Some((i, (_, r))) = self.buttons(cx).iter().enumerate().find(|(_, b)| b.0 == *key) {
            return Some(Placed { rect: *r, rest_rect: *r, clip: Rect::new(0.0, 0.0, 1920.0, 1080.0), index: Some(i as u32) });
        }
        self.grid.place(cx, &self.src(cx), key, at)
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        let src = self.src(cx);
        if src.cards.iter().any(|c| c.elem == want.elem) || self.buttons(cx).iter().any(|b| b.0 == want.elem) {
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
        let src = self.src(cx);
        if src.cards.is_empty() {
            common::empty_state(p, f.measure, cx.views.store.feed(&self.key), common::empty_text(&self.key), Rect::new(112.0, 170.0, 1700.0, 800.0), cx.views.now_ms);
        } else {
            ui::reset_focused_caption();
            self.grid.draw(f, p, &src);
            ui::draw_focused_caption(p, f.measure, 20.0);
            self.grid.record_stops(f, p, &src);
        }
        ui::header_band(p, Rect::new(0.0, 0.0, 1920.0, grid_top(&self.key) - 20.0));
        // The title takes the line up to the filters hint and the clock (both right-aligned).
        let mut right = 1824.0;
        if cx.views.store.settings.clock_on_pages {
            common::clock_block(p, cx, 1824.0, 40.0);
            let (time, date) = (common::clock(&cx.views), common::date_text());
            right -= f.measure.width_str(&time, 40, true).max(f.measure.width_str(&date, 22, false)) + 40.0;
        }
        let hint = self.filters_hint(cx);
        let hint_w = hint.as_ref().map_or(0.0, |h| f.measure.width_str(h, 26, false) + 40.0);
        let room = (right - hint_w - 112.0).max(240.0);
        ui::text_cut(p, f.measure, &self.title(cx), 112.0, 64.0, room, 46, ui::WHITE, ui::ALIGN_LEFT, true);
        if let Some(h) = &hint {
            ui::text_raw(p, h, right, 74.0, 26, ui::FAINT, ui::ALIGN_RIGHT, false);
        }
        for (e, r) in self.buttons(cx) {
            ui::stop(f, FocusKey { entry: self.entry, elem: e }, r, Rect::new(0.0, 0.0, 1920.0, 1080.0));
        }
        // A playlist's Play all and Shuffle, drawn as the channel's Subscribe is, with an icon.
        for (e, r) in self.buttons(cx).into_iter().filter(|b| b.0 != SUB_BTN) {
            let focused = cx.focus.current.is_some_and(|k| k.elem == e);
            ui::fill(p, r, 25.0, if focused { ui::WHITE } else { [1.0, 1.0, 1.0, 0.16] });
            let ink = if focused { [0.0, 0.0, 0.0, 1.0] } else { ui::WHITE };
            let (icon, label) = if e == PLAY_BTN { (ui::icon::Kind::Play, "Play all") } else { (ui::icon::Kind::Shuffle, "Shuffle") };
            let tw = f.measure.width_str(crate::i18n::tr(label), 26, true);
            let x = r.x + (r.w - tw - 38.0) / 2.0;
            ui::icon::draw(p, icon, Rect::new(x, r.y + 12.0, 26.0, 26.0), ink, icon == ui::icon::Kind::Play);
            ui::text(p, label, x + 38.0, r.y + 10.0, 26, ink, ui::ALIGN_LEFT, true);
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

//! Chapter selector over the player: a panel on the right listing the video's chapters with their
//! start times. Focus starts on the chapter being played; OK jumps there and closes, BACK closes.
//! Up/Down, the wheel and the Magic Remote pointer move through the list. A long chapter name
//! wraps onto more lines (its row grows), so the whole name is readable.

use crate::host::{YtFx, YtHost};
use crate::ui::{self, VList};
use plx_machine::machine::{Canon, Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, LogicalState, Machine, NavOp, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use std::borrow::Cow;

const G: GroupId = GroupId(1);
const BASE: u32 = 0x7900_0000;
/// The panel on the right (the video keeps playing full size under the rest).
const PANEL: Rect = Rect::new(1260.0, 0.0, 660.0, 1080.0);
const TEXT: i32 = 26;
/// One line of a chapter name, the padding around a row's lines, the shortest row.
const LINE_H: f32 = 34.0;
const PAD: f32 = 15.0;
const MIN_H: f32 = 64.0;
/// A name longer than this many lines ends with "…".
const MAX_LINES: usize = 4;
/// Room for the start time on the right (with the gap before it).
const TIME_W: f32 = 120.0;

/// One chapter row: where it starts below the list's top (unscrolled), its height and its lines.
struct Lay {
    y: f32,
    h: f32,
    lines: Vec<String>,
}

struct State;
impl LogicalState for State {
    fn write(&self, c: &mut Canon) {
        c.u32(16);
    }
    fn probe(&self, out: &mut String) {
        out.push_str("chapters");
    }
}

pub struct ChaptersScreen {
    list: VList,
}

impl ChaptersScreen {
    pub fn new(entry: EntryId) -> Self {
        ChaptersScreen { list: VList { entry, group: G, base: BASE, rect: Rect::new(PANEL.x + 24.0, 120.0, PANEL.w - 48.0, 920.0), row_h: MIN_H, text_size: TEXT } }
    }
    fn chapters(cx: &Cx<'_, YtHost>) -> Vec<(String, u64)> {
        cx.views.player.extras.watch.as_ref().map(|w| w.chapters.iter().map(|c| (c.title.clone(), c.start_ms)).collect()).unwrap_or_default()
    }
    /// The rows: each as tall as its name's lines.
    fn layout(&self, m: &dyn plx_machine::machine::Measure, ch: &[(String, u64)]) -> Vec<Lay> {
        let w = self.list.rect.w - 28.0 - 24.0 - TIME_W;
        let mut y = 0.0;
        ch.iter()
            .map(|(t, _)| {
                let lines = ui::wrap_lines(m, t, w, TEXT, true, MAX_LINES);
                let h = (lines.len().max(1) as f32 * LINE_H + 2.0 * PAD).max(MIN_H);
                let l = Lay { y, h, lines };
                y += h;
                l
            })
            .collect()
    }
    /// How far the list is scrolled: just enough for the focused row to show whole.
    fn scroll(&self, lay: &[Lay], focus: Option<usize>) -> f32 {
        let view = self.list.rect.h;
        let total = lay.last().map_or(0.0, |l| l.y + l.h);
        match focus.and_then(|f| lay.get(f)) {
            Some(l) if l.y + l.h > view => (l.y + l.h - view).min((total - view).max(0.0)),
            _ => 0.0,
        }
    }
    fn row_rect(&self, l: &Lay, scroll: f32) -> Rect {
        Rect::new(self.list.rect.x, self.list.rect.y + l.y - scroll, self.list.rect.w, l.h - 6.0)
    }
    /// A row wholly inside the list's area.
    fn shows(&self, r: Rect) -> bool {
        r.y >= self.list.rect.y - 0.5 && r.y + r.h <= self.list.rect.y + self.list.rect.h + 0.5
    }
    /// The chapter being played.
    fn current(cx: &Cx<'_, YtHost>, ch: &[(String, u64)]) -> usize {
        let pos = cx.views.player.position_ms;
        ch.iter().rposition(|c| c.1 <= pos).unwrap_or(0)
    }
}

impl Machine<YtHost> for ChaptersScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        if let Some(a) = super::common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        match ev {
            // Counted as a menu: the wheel and the pointer's top / bottom edges step the rows.
            ScreenEvent::Mount => {
                ui::MENUS_OPEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Handled::Yes
            }
            ScreenEvent::Unmount => {
                ui::MENUS_OPEN.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                Handled::Yes
            }
            ScreenEvent::Activate(e) => {
                let ch = Self::chapters(cx);
                if let Some(i) = self.list.index(*e, ch.len()) {
                    fx.push(Fx::App(YtFx::SeekTo(ch[i].1)));
                    // A presented overlay closes with Dismiss (Pop would act on the player's stack).
                    fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
                }
                Handled::Yes
            }
            ScreenEvent::Input(_) => Handled::No,
            _ => Handled::Yes,
        }
    }
}

impl Focusable<YtHost> for ChaptersScreen {
    fn groups(&self, cx: &Cx<'_, YtHost>, out: &mut Vec<GroupSpec>) {
        out.push(self.list.spec(Self::chapters(cx).len().max(1)));
    }
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        Some(G)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, YtHost>) -> Step<u32> {
        self.list.neighbour(key, dir, Self::chapters(cx).len())
    }
    fn place(&self, key: &u32, cx: &Cx<'_, YtHost>, _at: At) -> Option<Placed> {
        let ch = Self::chapters(cx);
        let i = self.list.index(*key, ch.len())?;
        let lay = self.layout(cx.measure, &ch);
        let r = self.row_rect(&lay[i], self.scroll(&lay, self.list.focused(cx, ch.len())));
        Some(Placed { rect: r, rest_rect: r, clip: self.list.rect, index: Some(i as u32) })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        let ch = Self::chapters(cx);
        if self.list.index(want.elem, ch.len()).is_some() {
            return want;
        }
        // Start on the chapter being played.
        self.list.key(Self::current(cx, &ch))
    }
    fn seat(&self, _g: GroupId, _from: Placed, cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        let ch = Self::chapters(cx);
        self.list.key(Self::current(cx, &ch))
    }
}

impl Screen<YtHost> for ChaptersScreen {
    fn name(&self) -> &'static str {
        "chapters"
    }
    fn state(&self) -> &dyn LogicalState {
        &State
    }
    fn crumb(&self, _cx: &Cx<'_, YtHost>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, YtHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, YtHost>) {
        let p = f.painter;
        let cx = f.cx;
        let _dim = ui::HdrDim::begin(ui::hdr_playing(cx.views.player));
        ui::fill(p, PANEL, 0.0, [0.05, 0.05, 0.07, 0.94]);
        ui::text(p, "Chapters", PANEL.x + 48.0, 40.0, 34, ui::WHITE, ui::ALIGN_LEFT, true);
        let ch = Self::chapters(cx);
        if ch.is_empty() {
            ui::text(p, "No chapters", PANEL.x + PANEL.w / 2.0, 500.0, 28, ui::FAINT, ui::ALIGN_CENTER, false);
            return;
        }
        let cur = Self::current(cx, &ch);
        let lay = self.layout(f.measure, &ch);
        let focus = self.list.focused(cx, ch.len());
        let scroll = self.scroll(&lay, focus);
        let mut hidden_below = false;
        for (i, l) in lay.iter().enumerate() {
            let r = self.row_rect(l, scroll);
            if !self.shows(r) {
                hidden_below |= r.y > self.list.rect.y;
                continue;
            }
            let focused = focus == Some(i);
            let active = i == cur;
            if focused {
                ui::fill(p, r, 12.0, ui::ROW_FOCUS);
            }
            if active {
                ui::fill(p, Rect::new(r.x, r.y + 12.0, 6.0, r.h - 24.0), 3.0, ui::accent());
            }
            let col = if focused || active { ui::WHITE } else { ui::DIM };
            let top = r.y + (r.h - l.lines.len().max(1) as f32 * LINE_H) / 2.0 + (LINE_H - TEXT as f32 * 1.3) / 2.0;
            for (k, line) in l.lines.iter().enumerate() {
                ui::text_raw(p, line, r.x + 28.0, top + k as f32 * LINE_H, TEXT, col, ui::ALIGN_LEFT, focused || active);
            }
            ui::text_raw(p, &ui::fmt_time(ch[i].1), r.x + r.w - 24.0, top, TEXT, if focused { ui::WHITE } else { ui::DIM }, ui::ALIGN_RIGHT, false);
            ui::stop(f, self.list.key(i), r, self.list.rect);
        }
        if scroll > 0.0 {
            ui::text(p, "\u{25B2}", self.list.rect.x + self.list.rect.w / 2.0, self.list.rect.y - 34.0, 22, ui::FAINT, ui::ALIGN_CENTER, false);
        }
        if hidden_below {
            ui::text(p, "\u{25BC}", self.list.rect.x + self.list.rect.w / 2.0, self.list.rect.y + self.list.rect.h, 22, ui::FAINT, ui::ALIGN_CENTER, false);
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

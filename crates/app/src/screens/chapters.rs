//! Chapter selector over the player: a panel on the right listing the video's chapters with their
//! start times. Focus starts on the chapter being played; OK jumps there and closes, BACK closes.
//! Up/Down, the wheel and the Magic Remote pointer move through the list.

use crate::host::{YtFx, YtHost};
use crate::ui::{self, Row, VList};
use plx_machine::machine::{Canon, Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, LogicalState, Machine, NavOp, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use std::borrow::Cow;

const G: GroupId = GroupId(1);
const BASE: u32 = 0x7900_0000;
/// The panel on the right (the video keeps playing full size under the rest).
const PANEL: Rect = Rect::new(1260.0, 0.0, 660.0, 1080.0);

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
        ChaptersScreen { list: VList { entry, group: G, base: BASE, rect: Rect::new(PANEL.x + 24.0, 120.0, PANEL.w - 48.0, 920.0), row_h: 64.0, text_size: 26 } }
    }
    fn chapters(cx: &Cx<'_, YtHost>) -> Vec<(String, u64)> {
        cx.views.player.extras.watch.as_ref().map(|w| w.chapters.iter().map(|c| (c.title.clone(), c.start_ms)).collect()).unwrap_or_default()
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
        let n = Self::chapters(cx).len().max(1);
        self.list.place(*key, n, self.list.focused(cx, n))
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
        let rows: Vec<Row> = ch
            .iter()
            .enumerate()
            .map(|(i, (t, ms))| Row::new(t.clone()).value(ui::fmt_time(*ms)).active(i == cur).raw())
            .collect();
        self.list.draw(p, f.measure, &rows, self.list.focused(cx, rows.len()));
        self.list.stops(f, rows.len());
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

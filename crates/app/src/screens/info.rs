//! Video info (over the player): title, channel, stats, description, and the chapter list —
//! OK on a chapter jumps there and returns to the video.

use crate::host::{YtFx, YtHost};
use crate::ui::{self, Row, VList};
use plx_machine::machine::{Canon, Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, LogicalState, Machine, NavOp, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use std::borrow::Cow;

const G: GroupId = GroupId(1);
const BASE: u32 = 0x7800_0000;

fn wrap(s: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in s.lines() {
        let mut cur = String::new();
        for w in para.split_whitespace() {
            if !cur.is_empty() && cur.chars().count() + 1 + w.chars().count() > width {
                out.push(std::mem::take(&mut cur));
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(w);
        }
        out.push(cur);
    }
    out
}

struct State;
impl LogicalState for State {
    fn write(&self, c: &mut Canon) {
        c.u32(13);
    }
    fn probe(&self, out: &mut String) {
        out.push_str("info");
    }
}

pub struct InfoScreen {
    list: VList,
}

impl InfoScreen {
    pub fn new(entry: EntryId) -> Self {
        InfoScreen { list: VList { entry, group: G, base: BASE, rect: Rect::new(1180.0, 180.0, 680.0, 860.0), row_h: 70.0, text_size: 26 } }
    }
    fn chapters<'a>(&self, cx: &Cx<'a, YtHost>) -> Vec<(String, u64)> {
        cx.views.player.extras.watch.as_ref().map(|w| w.chapters.iter().map(|c| (c.title.clone(), c.start_ms)).collect()).unwrap_or_default()
    }
}

impl Machine<YtHost> for InfoScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        if let Some(a) = super::common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        match ev {
            ScreenEvent::Activate(e) => {
                let ch = self.chapters(cx);
                if let Some(i) = self.list.index(*e, ch.len()) {
                    fx.push(Fx::App(YtFx::SeekTo(ch[i].1)));
                    fx.push(Fx::Nav(NavOp::Pop));
                }
                Handled::Yes
            }
            ScreenEvent::Input(_) => Handled::No,
            _ => Handled::Yes,
        }
    }
}

impl Focusable<YtHost> for InfoScreen {
    fn groups(&self, cx: &Cx<'_, YtHost>, out: &mut Vec<GroupSpec>) {
        let n = self.chapters(cx).len();
        if n > 0 {
            out.push(self.list.spec(n));
        }
    }
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        Some(G)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, YtHost>) -> Step<u32> {
        self.list.neighbour(key, dir, self.chapters(cx).len())
    }
    fn place(&self, key: &u32, cx: &Cx<'_, YtHost>, _at: At) -> Option<Placed> {
        let n = self.chapters(cx).len();
        self.list.place(*key, n, self.list.focused(cx, n))
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        let ch = self.chapters(cx);
        if self.list.index(want.elem, ch.len()).is_some() {
            return want;
        }
        // Start on the chapter being played.
        let pos = cx.views.player.position_ms;
        let cur = ch.iter().rposition(|c| c.1 <= pos).unwrap_or(0);
        self.list.key(cur)
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        self.list.key(0)
    }
}

impl Screen<YtHost> for InfoScreen {
    fn name(&self) -> &'static str {
        "info"
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
        ui::fill(p, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.03, 0.03, 0.05, 0.94]);
        let st = cx.views.player;
        let w = st.extras.watch.as_ref();
        ui::text(p, &ui::ellipsize(&st.title, 60), 80.0, 70.0, 42, ui::WHITE, ui::ALIGN_LEFT, true);
        let mut meta = st.author.clone();
        if let Some(w) = w {
            for t in [&w.subscriber_count_text, &w.view_count_text, &w.published_text, &w.like_count_text].into_iter().flatten() {
                meta.push_str(&format!("  \u{2022}  {t}"));
            }
        }
        ui::text(p, &ui::ellipsize(&meta, 110), 80.0, 130.0, 26, ui::DIM, ui::ALIGN_LEFT, false);
        let desc = w.map(|w| w.description.as_str()).unwrap_or("");
        for (i, line) in wrap(desc, 62).iter().take(24).enumerate() {
            ui::text(p, line, 80.0, 190.0 + i as f32 * 34.0, 24, ui::WHITE, ui::ALIGN_LEFT, false);
        }
        let ch = self.chapters(cx);
        if ch.is_empty() {
            ui::text(p, "No chapters", 1180.0, 190.0, 28, ui::FAINT, ui::ALIGN_LEFT, false);
        } else {
            ui::text(p, "Chapters", 1180.0, 130.0, 30, ui::WHITE, ui::ALIGN_LEFT, true);
            let pos = st.position_ms;
            let cur = ch.iter().rposition(|c| c.1 <= pos);
            let rows: Vec<Row> = ch
                .iter()
                .enumerate()
                .map(|(i, (t, ms))| Row::new(ui::ellipsize(t, 34)).value(ui::fmt_time(*ms)).active(Some(i) == cur))
                .collect();
            self.list.draw(p, &rows, self.list.focused(cx, rows.len()));
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

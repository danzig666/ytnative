//! Video info (over the player): title, channel, stats, description, and the chapter list —
//! OK on a chapter jumps there and returns to the video. A description longer than the screen
//! scrolls: Up / Down while it is selected (Left from the chapters), the wheel or the pointer
//! resting at the top / bottom edge while the pointer is over it.

use crate::host::{YtFx, YtHost};
use crate::ui::{self, Row, VList};
use plx_machine::machine::{Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, Key, LogicalState, Machine, NavOp, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use std::borrow::Cow;

const G: GroupId = GroupId(1);
const G_DESC: GroupId = GroupId(2);
const BASE: u32 = 0x7800_0000;
/// The description as one focusable block.
const DESC: u32 = BASE - 1;
/// Where the description is drawn, and its line height.
const DESC_RECT: Rect = Rect { x: 60.0, y: 172.0, w: 1100.0, h: 868.0 };
const DESC_TOP: f32 = 190.0;
const LINE_H: f32 = 34.0;
const DESC_SIZE: i32 = 24;
/// Lines moved by one key press / one wheel notch.
const KEY_LINES: f32 = 6.0;
const WHEEL_LINES: f32 = 4.0;

/// The description wrapped to `w` px, paragraph by paragraph, at most `max` lines (a word wider
/// than the line, a long link say, is cut across lines; the last line shown is ellipsised).
fn wrap(m: &dyn plx_machine::machine::Measure, s: &str, w: f32, size: i32, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in s.lines() {
        if out.len() >= max {
            break;
        }
        if para.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let one_line: String = para.split_whitespace().collect::<Vec<_>>().join(" ");
        out.extend(ui::wrap_lines(m, &one_line, w, size, false, max - out.len()));
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
    /// First description line shown (fractional while the wheel or an edge glides).
    scroll: f32,
    /// The wrapped description, kept while the text is the same.
    wrapped: Option<(String, Vec<String>)>,
}

impl InfoScreen {
    pub fn new(entry: EntryId) -> Self {
        InfoScreen { list: VList { entry, group: G, base: BASE, rect: Rect::new(1180.0, 180.0, 680.0, 860.0), row_h: 56.0, text_size: 24 }, scroll: 0.0, wrapped: None }
    }
    fn desc_key(&self) -> FocusKey<u32> {
        FocusKey { entry: self.list.entry, elem: DESC }
    }
    /// Description lines that fit on screen at once.
    fn visible_lines() -> usize {
        ((DESC_RECT.y + DESC_RECT.h - DESC_TOP) / LINE_H).floor() as usize
    }
    fn max_scroll(&self) -> f32 {
        let n = self.wrapped.as_ref().map_or(0, |(_, l)| l.len());
        n.saturating_sub(Self::visible_lines()) as f32
    }
    fn scroll_by(&mut self, lines: f32, fx: &mut Effects<'_, YtHost>) {
        self.scroll = (self.scroll + lines).clamp(0.0, self.max_scroll());
        fx.invalidate(plx_machine::present::Provenance::Input);
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
            // Where the pointer is decides what the wheel and the edges scroll.
            // By column only: the edges that scroll are the very top and bottom of the screen.
            ScreenEvent::Input(InputEvent { kind: InputKind::Pointer { x, .. }, .. }) => {
                ui::INFO_DESC_HOVER.store(*x < DESC_RECT.x + DESC_RECT.w + 20.0, std::sync::atomic::Ordering::Relaxed);
                Handled::No
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::PointerHidden, .. }) => {
                ui::INFO_DESC_HOVER.store(false, std::sync::atomic::Ordering::Relaxed);
                Handled::No
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Wheel { dy }, .. }) => {
                self.scroll_by(-*dy * WHEEL_LINES, fx);
                Handled::Yes
            }
            // The description selected: Up / Down scroll it (Right goes to the chapters).
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: k @ (Key::Up | Key::Down), edge: Edge::Down | Edge::Repeat, .. }, .. })
                if cx.focus.current == Some(self.desc_key()) =>
            {
                self.scroll_by(if *k == Key::Down { KEY_LINES } else { -KEY_LINES }, fx);
                Handled::Yes
            }
            ScreenEvent::Unmount => {
                ui::INFO_DESC_HOVER.store(false, std::sync::atomic::Ordering::Relaxed);
                Handled::Yes
            }
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
        out.push(GroupSpec {
            id: G_DESC,
            kind: plx_ui::screen::GroupKind::Row { wrap: false },
            seat: plx_ui::screen::Seat::First,
            reachable: plx_ui::screen::AxisMask::BOTH,
            edge: [plx_ui::screen::EdgeRule::Geometric; 4],
            extent: DESC_RECT,
            len: 1,
            elem: plx_ui::screen::ElemKind::Control,
        });
        let n = self.chapters(cx).len();
        if n > 0 {
            out.push(self.list.spec(n));
        }
    }
    fn group_of(&self, key: &u32, _cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        Some(if *key == DESC { G_DESC } else { G })
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, YtHost>) -> Step<u32> {
        let ch = self.chapters(cx);
        if key.elem == DESC {
            // Right: the chapter being played.
            return match dir {
                Dir::Right if !ch.is_empty() => {
                    let pos = cx.views.player.position_ms;
                    Step::Move(self.list.key(ch.iter().rposition(|c| c.1 <= pos).unwrap_or(0)))
                }
                _ => Step::Edge,
            };
        }
        match dir {
            Dir::Left => Step::Move(self.desc_key()),
            _ => self.list.neighbour(key, dir, ch.len()),
        }
    }
    fn place(&self, key: &u32, cx: &Cx<'_, YtHost>, _at: At) -> Option<Placed> {
        if *key == DESC {
            return Some(Placed { rect: DESC_RECT, rest_rect: DESC_RECT, clip: Rect::new(0.0, 0.0, 1920.0, 1080.0), index: Some(0) });
        }
        let n = self.chapters(cx).len();
        self.list.place(*key, n, self.list.focused(cx, n))
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        let ch = self.chapters(cx);
        if want.elem == DESC || self.list.index(want.elem, ch.len()).is_some() {
            return want;
        }
        if ch.is_empty() {
            return self.desc_key();
        }
        // Start on the chapter being played.
        let pos = cx.views.player.position_ms;
        let cur = ch.iter().rposition(|c| c.1 <= pos).unwrap_or(0);
        self.list.key(cur)
    }
    fn seat(&self, g: GroupId, _from: Placed, _cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        if g == G_DESC {
            self.desc_key()
        } else {
            self.list.key(0)
        }
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
        let m = f.measure;
        ui::text_raw(p, &m.fit_line(&st.title, 1760.0, 42, true).to_string_lossy(), 80.0, 70.0, 42, ui::WHITE, ui::ALIGN_LEFT, true);
        let mut meta = st.author.clone();
        if let Some(w) = w {
            for t in [&w.subscriber_count_text, &w.view_count_text, &super::common::video_age(w), &w.like_count_text].into_iter().flatten() {
                meta.push_str(&format!("  \u{2022}  {t}"));
            }
        }
        // The left column (stats, description) ends short of the chapter column.
        let col_w = 1180.0 - 80.0 - 40.0;
        ui::text_raw(p, &m.fit_line(&meta, col_w, 26, false).to_string_lossy(), 80.0, 130.0, 26, ui::DIM, ui::ALIGN_LEFT, false);
        let desc = w.map(|w| w.description.as_str()).unwrap_or("");
        if self.wrapped.as_ref().is_none_or(|(d, _)| d != desc) {
            self.wrapped = Some((desc.to_string(), wrap(m, desc, col_w, DESC_SIZE, 5000)));
            self.scroll = 0.0;
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
        let vis = Self::visible_lines();
        let first = self.scroll.floor() as usize;
        let desc_focused = cx.focus.current == Some(self.desc_key());
        let total = self.wrapped.as_ref().map_or(0, |(_, l)| l.len());
        if let Some((_, lines)) = &self.wrapped {
            for (i, line) in lines.iter().skip(first).take(vis).enumerate() {
                ui::text_raw(p, line, 80.0, DESC_TOP + i as f32 * LINE_H, DESC_SIZE, ui::WHITE, ui::ALIGN_LEFT, false);
            }
        }
        if total > vis {
            // A scroll bar beside the text: where the visible part is in the whole.
            let track = Rect::new(DESC_RECT.x + DESC_RECT.w - 8.0, DESC_TOP, 6.0, vis as f32 * LINE_H);
            ui::fill(p, track, 3.0, [1.0, 1.0, 1.0, 0.12]);
            let h = (track.h * vis as f32 / total as f32).max(40.0);
            let y = track.y + (track.h - h) * (self.scroll / self.max_scroll().max(1.0));
            ui::fill(p, Rect::new(track.x, y, track.w, h), 3.0, if desc_focused { ui::WHITE } else { [1.0, 1.0, 1.0, 0.45] });
        }
        if desc_focused {
            // Selected (Up / Down scroll it): an accent bar along its left side.
            ui::fill(p, Rect::new(DESC_RECT.x, DESC_TOP, 4.0, total.clamp(1, vis) as f32 * LINE_H), 2.0, ui::accent());
        }
        ui::stop(f, self.desc_key(), DESC_RECT, Rect::new(0.0, 0.0, 1920.0, 1080.0));
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
                .map(|(i, (t, ms))| Row::new(t.clone()).value(ui::fmt_time(*ms)).active(Some(i) == cur).raw())
                .collect();
            self.list.draw(p, f.measure, &rows, self.list.focused(cx, rows.len()));
            self.list.stops(f, rows.len());
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

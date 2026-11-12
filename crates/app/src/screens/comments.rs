//! Comments for a video: a scrolling list; more pages load as you approach the end.

use crate::host::{YtArg, YtFx, YtHost, YtMsg};
use plx_machine::machine::NavOp;
use crate::ui::{self, VList};
use plx_machine::machine::{Canon, Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, LogicalState, Machine, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use std::borrow::Cow;

const G: GroupId = GroupId(1);
const BASE: u32 = 0x7000_0000;
const TEXT: i32 = 26;
const CHARS_PER_LINE: usize = 46;
/// The panel on the right; the video shrinks into the space on the left.
const PANEL: Rect = Rect::new(1236.0, 0.0, 684.0, 1080.0);
/// The video beside the panel: 16:9 in the left 1216 px, centred vertically.
const VIDEO: [i32; 4] = [10, 198, 1216, 684];

/// Greedy word wrap by character count (good enough for a TV font at a fixed size).
fn wrap(s: &str, width: usize, max_lines: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for para in s.lines() {
        let mut cur = String::new();
        for word in para.split_whitespace() {
            if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > width {
                lines.push(std::mem::take(&mut cur));
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(word);
        }
        if !cur.is_empty() {
            lines.push(cur);
        }
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        if let Some(l) = lines.last_mut() {
            l.push('\u{2026}');
        }
    }
    lines
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

pub struct CommentsScreen {
    /// Store key: the video id, or the replies continuation for a reply thread.
    video_id: String,
    /// Some(parent author) when showing replies.
    replies_to: Option<String>,
    list: VList,
    /// First visible comment while the wheel has scrolled the list (focus stays where it was);
    /// `None`: the list follows the focus.
    scroll: Option<usize>,
    /// Smooth scroll position in rows: where the list is drawn (`scroll_at`) and is going.
    scroll_at: std::cell::Cell<f32>,
    scroll_to: f32,
    /// When the panel opened (it slides in from the right).
    opened_ms: u32,
    /// Already gave the video back (BACK, then Unmount).
    released: bool,
    /// BACK pressed at: the panel slides out over `CLOSE_MS` (ahead of the overlay's own fade).
    closing_at: Option<u32>,
    /// Wheel travel not yet turned into whole rows (the pointer's edge scroll is fractional).
    wheel: f32,
    state: State,
}

impl CommentsScreen {
    pub fn new(entry: EntryId, video_id: String, replies_to: Option<String>) -> Self {
        CommentsScreen {
            state: State(video_id.clone()),
            replies_to,
            video_id,
            list: VList { entry, group: G, base: BASE, rect: Rect::new(PANEL.x + 20.0, 100.0, PANEL.w - 40.0, 970.0), row_h: 192.0, text_size: TEXT },
            scroll: None,
            scroll_at: std::cell::Cell::new(0.0),
            scroll_to: 0.0,
            opened_ms: 0,
            released: false,
            closing_at: None,
            wheel: 0.0,
        }
    }
    fn len(&self, cx: &Cx<'_, YtHost>) -> usize {
        cx.views.store.comments.get(&self.video_id).map_or(0, |c| c.items.len())
    }
    /// Gives the video back (once): full size unless another comments panel is still open.
    fn release(&mut self, fx: &mut Effects<'_, YtHost>) {
        if std::mem::replace(&mut self.released, true) {
            return;
        }
        ui::COMMENTS_OPEN.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        if !ui::comments_open() {
            fx.push(Fx::App(YtFx::VideoRect(None)));
        }
    }
    /// First visible comment.
    fn top(&self, cx: &Cx<'_, YtHost>) -> usize {
        let n = self.len(cx);
        let max = n.saturating_sub(self.list.rows_visible());
        match self.scroll {
            Some(t) => t.min(max),
            None => self.list.top_for(self.list.focused(cx, n), n),
        }
    }
    fn more_if_near_end(&self, last_seen: usize, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        let n = self.len(cx);
        if let Some(c) = cx.views.store.comments.get(&self.video_id) {
            if last_seen + 4 >= n && !c.loading {
                if let Some(next) = c.next.clone() {
                    fx.push(Fx::App(YtFx::LoadComments { key: self.video_id.clone(), continuation: Some(next), append: true }));
                }
            }
        }
    }
}

impl Machine<YtHost> for CommentsScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        if let Some(a) = super::common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        match ev {
            ScreenEvent::Mount => {
                let continuation = self.replies_to.as_ref().map(|_| self.video_id.clone());
                fx.push(Fx::App(YtFx::LoadComments { key: self.video_id.clone(), continuation, append: false }));
                fx.push(Fx::App(YtFx::VideoRect(Some(VIDEO))));
                ui::COMMENTS_OPEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.opened_ms = cx.views.now_ms;
            }
            ScreenEvent::Uncover => {
                fx.push(Fx::App(YtFx::VideoRect(Some(VIDEO))));
                fx.invalidate(plx_machine::present::Provenance::Nav);
            }
            ScreenEvent::Unmount => self.release(fx),
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: plx_machine::machine::Key::Back, edge: plx_machine::machine::Edge::Down, .. }, .. }) => {
                // Grow the video back and bring the player's captions at once; the panel's close
                // fade runs alongside instead of before.
                self.release(fx);
                self.closing_at = Some(cx.views.now_ms);
                fx.invalidate(plx_machine::present::Provenance::Input);
                return Handled::No;
            }
            // The wheel scrolls the list; focus stays on its comment.
            ScreenEvent::Input(InputEvent { kind: InputKind::Wheel { dy }, .. }) => {
                let n = self.len(cx);
                let max = n.saturating_sub(self.list.rows_visible()) as f32;
                if self.scroll.is_none() {
                    self.scroll_to = self.top(cx) as f32;
                    self.scroll_at.set(self.scroll_to);
                }
                self.scroll_to = (self.scroll_to - *dy).clamp(0.0, max.max(0.0));
                self.scroll = Some(self.scroll_to.floor() as usize);
                self.more_if_near_end(self.scroll_to as usize + self.list.rows_visible(), cx, fx);
                fx.invalidate(plx_machine::present::Provenance::Input);
                return Handled::Yes;
            }
            ScreenEvent::Activate(e) => {
                let n = self.len(cx);
                if let Some(i) = self.list.index(*e, n) {
                    let c = &cx.views.store.comments[&self.video_id].items[i];
                    // Only comments that have replies open a thread; the rest do nothing.
                    if let Some(token) = c.replies_continuation.as_ref().filter(|_| c.reply_count > 0) {
                        fx.push(Fx::Nav(NavOp::Present(YtArg::Replies(token.clone(), c.author.clone()))));
                    }
                }
            }
            ScreenEvent::Async(_, YtMsg::Comments) => fx.invalidate(plx_machine::present::Provenance::Lifecycle),
            ScreenEvent::FocusMoved { to, by, .. } => {
                if *by != plx_machine::machine::By::Pointer {
                    self.scroll = None; // keys: the list follows the focus again
                }
                let n = self.len(cx);
                if let Some(i) = self.list.index(to.elem, n) {
                    self.more_if_near_end(i, cx, fx);
                }
            }
            ScreenEvent::Input(_) => return Handled::No,
            _ => {}
        }
        Handled::Yes
    }
}

impl Focusable<YtHost> for CommentsScreen {
    fn groups(&self, cx: &Cx<'_, YtHost>, out: &mut Vec<GroupSpec>) {
        // Always one row, even while loading, so focus is seated on entry and lands on the first
        // comment once they arrive.
        out.push(self.list.spec(self.len(cx).max(1)));
    }
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        Some(G)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, YtHost>) -> Step<u32> {
        self.list.neighbour(key, dir, self.len(cx))
    }
    fn place(&self, key: &u32, cx: &Cx<'_, YtHost>, _at: At) -> Option<Placed> {
        let n = self.len(cx).max(1);
        self.list.place_top(*key, n, self.top(cx))
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        if self.list.index(want.elem, self.len(cx).max(1)).is_some() { want } else { self.list.key(0) }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        self.list.key(0)
    }
}

impl Screen<YtHost> for CommentsScreen {
    fn name(&self) -> &'static str {
        "comments"
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
        // A panel on the right, sliding in with the video's zoom-out; the video, shrunk, plays on
        // the left (the UI plane is clear there).
        // Replies open over the comments panel: no slide, so nothing shows through.
        let k = if self.replies_to.is_some() { 1.0 } else { ((cx.views.now_ms.wrapping_sub(self.opened_ms)) as f32 / 260.0).clamp(0.0, 1.0) };
        let ease = 1.0 - (1.0 - k).powi(3);
        if ease < 1.0 {
            ui::animating();
        }
        // Closing: slide out quickly, then draw nothing while the overlay finishes its fade.
        let out = match self.closing_at {
            Some(t0) => {
                let k = (cx.views.now_ms.wrapping_sub(t0) as f32 / 160.0).clamp(0.0, 1.0);
                if k >= 1.0 {
                    return;
                }
                ui::animating();
                k * k
            }
            None => 0.0,
        };
        let p = p.translate(PANEL.w * (1.0 - ease + out), 0.0);
        ui::fill(p, PANEL, 0.0, [0.06, 0.06, 0.08, 1.0]);
        {
            let n = self.len(cx);
            let top = self.top(cx);
            for i in (top..n).take(self.list.rows_visible()) {
                ui::stop(f, self.list.key(i), self.list.row_rect(i, top), self.list.rect);
            }
        }
        let title = match &self.replies_to {
            Some(a) => format!("Replies to {a}"),
            None => "Comments".into(),
        };
        ui::text(p, &title, PANEL.x + 40.0, 36.0, 34, ui::WHITE, ui::ALIGN_LEFT, true);
        let Some(c) = cx.views.store.comments.get(&self.video_id) else {
            ui::text(p, "Loading\u{2026}", PANEL.x + PANEL.w / 2.0, 500.0, 30, ui::DIM, ui::ALIGN_CENTER, false);
            return;
        };
        if c.items.is_empty() {
            let msg = c.error.clone().unwrap_or_else(|| if c.loading { "Loading\u{2026}".into() } else { "No comments".into() });
            ui::text(p, &msg, PANEL.x + PANEL.w / 2.0, 500.0, 30, ui::DIM, ui::ALIGN_CENTER, false);
            return;
        }
        let focus = self.list.focused(cx, c.items.len());
        // Wheel / edge scrolling: ease toward the target, drawn with a sub-row offset.
        let (top, frac) = if self.scroll.is_some() {
            let at = self.scroll_at.get();
            let next = at + (self.scroll_to - at) * 0.25;
            let next = if (self.scroll_to - next).abs() < 0.01 { self.scroll_to } else { ui::animating(); next };
            self.scroll_at.set(next);
            (next.floor() as usize, next.fract())
        } else {
            (self.top(cx), 0.0)
        };
        let p = p.translate(0.0, -frac * self.list.row_h);
        let _clip = plx_ui::screen::ClipScope::open_in(f.painter, Rect::new(PANEL.x, self.list.rect.y - 8.0, PANEL.w, self.list.rect.h + 8.0));
        let vis = self.list.rows_visible();
        for (i, cm) in c.items.iter().enumerate().skip(top).take(vis + 1) {
            let y = self.list.rect.y + (i - top) as f32 * self.list.row_h;
            let r = Rect::new(self.list.rect.x, y, self.list.rect.w, self.list.row_h - 12.0);
            if focus == Some(i) {
                ui::fill(p, r, 14.0, ui::ROW_FOCUS);
            }
            let mut head = cm.author.clone();
            if !cm.published_text.is_empty() {
                head.push_str(&format!("  \u{2022}  {}", cm.published_text));
            }
            if cm.is_pinned {
                head.push_str("  \u{2022}  Pinned");
            }
            ui::text(p, &ui::ellipsize(&head, 52), r.x + 20.0, r.y + 12.0, 22, ui::DIM, ui::ALIGN_LEFT, true);
            for (j, line) in wrap(&cm.text, CHARS_PER_LINE, 3).iter().enumerate() {
                ui::text(p, line, r.x + 20.0, r.y + 44.0 + j as f32 * 31.0, TEXT, ui::WHITE, ui::ALIGN_LEFT, false);
            }
            let mut foot = String::new();
            if let Some(l) = &cm.like_count_text {
                foot.push_str(&format!("\u{1F44D} {l}"));
            }
            if cm.reply_count > 0 {
                foot.push_str(&format!("      \u{1F4AC} {}", cm.reply_count));
            }
            ui::text(p, &foot, r.x + 20.0, r.y + r.h - 34.0, 22, ui::FAINT, ui::ALIGN_LEFT, false);
        }
        if c.loading {
            ui::text(p, "Loading more\u{2026}", PANEL.x + PANEL.w / 2.0, 1046.0, 22, ui::FAINT, ui::ALIGN_CENTER, false);
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

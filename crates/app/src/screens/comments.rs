//! Comments for a video: a scrolling list; more pages load as you approach the end.
//!
//! Every comment is as tall as its text: header, all of its wrapped lines (nothing is cut off),
//! and a likes/replies footer when there is one. The list scrolls in pixels.

use crate::host::{YtArg, YtFx, YtHost, YtMsg};
use crate::ui::{self, VList};
use plx_machine::machine::NavOp;
use plx_machine::machine::{Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, Key, LogicalState, Machine, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use std::borrow::Cow;

const G: GroupId = GroupId(1);
const BASE: u32 = 0x7000_0000;
const TEXT: i32 = 26;
/// The panel on the right; the video shrinks into the space on the left.
const PANEL: Rect = Rect::new(1236.0, 0.0, 684.0, 1080.0);
/// The video beside the panel: 16:9 in the left 1216 px, centred vertically.
const VIDEO: [i32; 4] = [10, 198, 1216, 684];
/// Gap between two comments.
const GAP: f32 = 8.0;
/// Line pitch of the comment text.
const LINE: f32 = 31.0;
/// Wheel travel per unit (px).
const WHEEL_PX: f32 = 150.0;

/// Word wrap to `w` px; a word wider than a line is broken by characters. Every line is kept.
fn wrap_px(m: &dyn plx_machine::machine::Measure, s: &str, w: f32, size: i32) -> Vec<String> {
    let width = |t: &str| std::ffi::CString::new(t).map_or(0.0, |c| m.width(&c, size, false));
    let mut lines = Vec::new();
    for para in s.lines() {
        let mut cur = String::new();
        for word in para.split_whitespace() {
            let trial = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
            if width(&trial) <= w {
                cur = trial;
                continue;
            }
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            if width(word) <= w {
                cur = word.to_string();
            } else {
                for ch in word.chars() {
                    let mut t = cur.clone();
                    t.push(ch);
                    if !cur.is_empty() && width(&t) > w {
                        lines.push(std::mem::take(&mut cur));
                        cur.push(ch);
                    } else {
                        cur = t;
                    }
                }
            }
        }
        if !cur.is_empty() {
            lines.push(cur);
        }
    }
    lines
}

/// Height of one comment: header, every text line, the likes/replies footer when there is one.
fn comment_h(lines: usize, foot: bool) -> f32 {
    10.0 + 30.0 + lines as f32 * LINE + if foot { 30.0 } else { 0.0 } + 10.0
}

/// The footer has something to show (likes or replies).
fn has_footer(cm: &yt_core::comments::Comment) -> bool {
    cm.like_count_text.is_some() || cm.reply_count > 0
}

/// Likes and replies, with the same thumb / chat icons as the player.
fn draw_footer(p: plx_ui::Painter, cm: &yt_core::comments::Comment, x: f32, y: f32) {
    let mut x = x;
    if let Some(l) = &cm.like_count_text {
        ui::icon::draw(p, ui::icon::Kind::ThumbUp, Rect::new(x, y + 2.0, 22.0, 22.0), ui::FAINT, false);
        x += 30.0;
        x += ui::text(p, l, x, y, 22, ui::FAINT, ui::ALIGN_LEFT, false) + 28.0;
    }
    if cm.reply_count > 0 {
        ui::icon::draw(p, ui::icon::Kind::Chat, Rect::new(x, y + 2.0, 22.0, 22.0), ui::FAINT, false);
        x += 30.0;
        ui::text(p, &cm.reply_count.to_string(), x, y, 22, ui::FAINT, ui::ALIGN_LEFT, false);
    }
}

/// Wrapped lines and positions of the loaded comments (built in `draw`, where text can be
/// measured; comments are only ever appended, so only new ones are laid out).
#[derive(Default)]
struct Layout {
    key: String,
    lines: Vec<Vec<String>>,
    top: Vec<f32>,
    h: Vec<f32>,
}

impl Layout {
    fn total(&self) -> f32 {
        match (self.top.last(), self.h.last()) {
            (Some(t), Some(h)) => t + h,
            _ => 0.0,
        }
    }
    fn span(&self, i: usize) -> Option<(f32, f32)> {
        Some((*self.top.get(i)?, *self.h.get(i)?))
    }
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
    /// Scroll position in px: where the list is drawn (`scroll_at`, eased) and where it is going.
    scroll_at: std::cell::Cell<f32>,
    scroll_to: f32,
    layout: std::cell::RefCell<Layout>,
    /// When the panel opened (it slides in from the right).
    opened_ms: u32,
    /// Already gave the video back (BACK, then Unmount).
    released: bool,
    /// BACK pressed at: the panel slides out quickly (ahead of the overlay's own fade).
    closing_at: Option<u32>,
    state: State,
}

impl CommentsScreen {
    pub fn new(entry: EntryId, video_id: String, replies_to: Option<String>) -> Self {
        CommentsScreen {
            state: State(video_id.clone()),
            replies_to,
            video_id,
            list: VList { entry, group: G, base: BASE, rect: Rect::new(PANEL.x + 20.0, 100.0, PANEL.w - 40.0, 970.0), row_h: 192.0, text_size: TEXT },
            scroll_at: std::cell::Cell::new(0.0),
            scroll_to: 0.0,
            layout: Default::default(),
            opened_ms: 0,
            released: false,
            closing_at: None,
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
    fn max_scroll(&self) -> f32 {
        (self.layout.borrow().total() - self.list.rect.h).max(0.0)
    }
    /// Scrolls (the target) so comment `i` is on screen: its top when it is taller than the list.
    fn reveal(&mut self, i: usize) {
        let Some((t, h)) = self.layout.borrow().span(i) else { return };
        let view = self.list.rect.h;
        let s = self.scroll_to;
        let to = if h > view || t < s {
            t
        } else if t + h > s + view {
            t + h - view
        } else {
            s
        };
        self.scroll_to = to.clamp(0.0, self.max_scroll());
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
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Back, edge: Edge::Down, .. }, .. }) => {
                // Grow the video back and bring the player's captions at once; the panel's close
                // fade runs alongside instead of before.
                self.release(fx);
                self.closing_at = Some(cx.views.now_ms);
                fx.invalidate(plx_machine::present::Provenance::Input);
                return Handled::No;
            }
            // The wheel scrolls the list; focus stays on its comment.
            ScreenEvent::Input(InputEvent { kind: InputKind::Wheel { dy }, .. }) => {
                self.scroll_to = (self.scroll_to - *dy * WHEEL_PX).clamp(0.0, self.max_scroll());
                if self.scroll_to + self.list.rect.h > self.layout.borrow().total() - 600.0 {
                    self.more_if_near_end(self.len(cx), cx, fx);
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
                return Handled::Yes;
            }
            // Up/Down inside a comment taller than the list: scroll through it before moving on.
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: k @ (Key::Up | Key::Down), edge: Edge::Down | Edge::Repeat, .. }, .. }) => {
                let n = self.len(cx);
                let span = self.list.focused(cx, n).and_then(|i| self.layout.borrow().span(i));
                if let Some((t, h)) = span {
                    let view = self.list.rect.h;
                    let s = self.scroll_to;
                    let step = view * 0.6;
                    let to = match k {
                        Key::Down if t + h > s + view + 1.0 => Some((s + step).min(t + h - view)),
                        Key::Up if t < s - 1.0 => Some((s - step).max(t)),
                        _ => None,
                    };
                    if let Some(to) = to {
                        self.scroll_to = to.clamp(0.0, self.max_scroll());
                        fx.invalidate(plx_machine::present::Provenance::Input);
                        return Handled::Yes;
                    }
                }
                return Handled::No;
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
                let n = self.len(cx);
                if let Some(i) = self.list.index(to.elem, n) {
                    if *by != plx_machine::machine::By::Pointer {
                        self.reveal(i); // keys: the list follows the focus
                    }
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
        let i = self.list.index(*key, self.len(cx).max(1))?;
        let (t, h) = self.layout.borrow().span(i).unwrap_or((0.0, 120.0));
        let r = Rect::new(self.list.rect.x, self.list.rect.y + t - self.scroll_to, self.list.rect.w, h);
        Some(Placed { rect: r, rest_rect: r, clip: self.list.rect, index: Some(i as u32) })
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
        let _dim = ui::HdrDim::begin(ui::hdr_playing(cx.views.player));
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
        // Lay out what arrived since the last frame.
        {
            let mut l = self.layout.borrow_mut();
            if l.key != self.video_id || l.lines.len() > c.items.len() {
                *l = Layout { key: self.video_id.clone(), ..Default::default() };
            }
            let w = self.list.rect.w - 40.0;
            let from = l.lines.len();
            for cm in &c.items[from..] {
                let lines = wrap_px(f.measure, &cm.text, w, TEXT);
                let h = comment_h(lines.len(), has_footer(cm));
                let t = if l.top.is_empty() { 0.0 } else { l.total() + GAP };
                l.lines.push(lines);
                l.top.push(t);
                l.h.push(h);
            }
        }
        let max = self.max_scroll();
        self.scroll_to = self.scroll_to.min(max);
        // Ease toward the target.
        let at = self.scroll_at.get().min(max);
        let next = at + (self.scroll_to - at) * 0.3;
        let s = if (self.scroll_to - next).abs() < 0.5 {
            self.scroll_to
        } else {
            ui::animating();
            next
        };
        self.scroll_at.set(s);
        let l = self.layout.borrow();
        let (y0, view) = (self.list.rect.y, self.list.rect.h);
        let visible = |i: usize| l.span(i).filter(|(t, h)| t + h > s && *t < s + view);
        for i in 0..c.items.len() {
            if let Some((t, h)) = visible(i) {
                ui::stop(f, self.list.key(i), Rect::new(self.list.rect.x, y0 + t - s, self.list.rect.w, h), self.list.rect);
            }
        }
        let focus = self.list.focused(cx, c.items.len());
        let _clip = plx_ui::screen::ClipScope::open_in(f.painter, Rect::new(PANEL.x, y0 - 8.0, PANEL.w, view + 8.0));
        for (i, cm) in c.items.iter().enumerate() {
            let Some((t, h)) = visible(i) else { continue };
            let r = Rect::new(self.list.rect.x, y0 + t - s, self.list.rect.w, h);
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
            ui::text(p, &ui::ellipsize(&head, 52), r.x + 20.0, r.y + 10.0, 22, ui::DIM, ui::ALIGN_LEFT, true);
            for (j, line) in l.lines[i].iter().enumerate() {
                let ly = r.y + 40.0 + j as f32 * LINE;
                if ly + LINE < y0 - 8.0 || ly > y0 + view {
                    continue;
                }
                ui::text(p, line, r.x + 20.0, ly, TEXT, ui::WHITE, ui::ALIGN_LEFT, false);
            }
            if has_footer(cm) {
                draw_footer(p, cm, r.x + 20.0, r.y + 40.0 + l.lines[i].len() as f32 * LINE + 2.0);
            }
        }
        if c.loading {
            ui::text(p, "Loading more\u{2026}", PANEL.x + PANEL.w / 2.0, 1046.0, 22, ui::FAINT, ui::ALIGN_CENTER, false);
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

//! Comments for a video: a scrolling list; more pages load as you approach the end.

use crate::host::{YtArg, YtFx, YtHost, YtMsg};
use plx_machine::machine::NavOp;
use crate::ui::{self, VList};
use plx_machine::machine::{Canon, Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, LogicalState, Machine, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use std::borrow::Cow;

const G: GroupId = GroupId(1);
const BASE: u32 = 0x7000_0000;
const TEXT: i32 = 26;
const CHARS_PER_LINE: usize = 92;

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
    state: State,
}

impl CommentsScreen {
    pub fn new(entry: EntryId, video_id: String, replies_to: Option<String>) -> Self {
        CommentsScreen {
            state: State(video_id.clone()),
            replies_to,
            video_id,
            list: VList { entry, group: G, base: BASE, rect: Rect::new(160.0, 170.0, 1600.0, 880.0), row_h: 176.0, text_size: TEXT },
        }
    }
    fn len(&self, cx: &Cx<'_, YtHost>) -> usize {
        cx.views.store.comments.get(&self.video_id).map_or(0, |c| c.items.len())
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
            }
            ScreenEvent::Activate(e) => {
                let n = self.len(cx);
                if let Some(i) = self.list.index(*e, n) {
                    let c = &cx.views.store.comments[&self.video_id].items[i];
                    if let Some(token) = &c.replies_continuation {
                        fx.push(Fx::Nav(NavOp::Push(YtArg::Replies(token.clone(), c.author.clone()))));
                    }
                }
            }
            ScreenEvent::Async(_, YtMsg::Comments) => fx.invalidate(plx_machine::present::Provenance::Lifecycle),
            ScreenEvent::FocusMoved { to, .. } => {
                let n = self.len(cx);
                if let Some(i) = self.list.index(to.elem, n) {
                    let c = &cx.views.store.comments[&self.video_id];
                    if i + 4 >= n && !c.loading {
                        if let Some(next) = c.next.clone() {
                            fx.push(Fx::App(YtFx::LoadComments { key: self.video_id.clone(), continuation: Some(next), append: true }));
                        }
                    }
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
        let n = self.len(cx);
        if n > 0 {
            out.push(self.list.spec(n));
        }
    }
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        Some(G)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, YtHost>) -> Step<u32> {
        self.list.neighbour(key, dir, self.len(cx))
    }
    fn place(&self, key: &u32, cx: &Cx<'_, YtHost>, _at: At) -> Option<Placed> {
        let n = self.len(cx);
        self.list.place(*key, n, self.list.focused(cx, n))
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        if self.list.index(want.elem, self.len(cx)).is_some() { want } else { self.list.key(0) }
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
        let title = match &self.replies_to {
            Some(a) => format!("Replies to {a}"),
            None => "Comments".into(),
        };
        ui::text(p, &title, 160.0, 70.0, 48, ui::WHITE, ui::ALIGN_LEFT, true);
        let Some(c) = cx.views.store.comments.get(&self.video_id) else {
            ui::text(p, "Loading\u{2026}", 960.0, 500.0, 34, ui::DIM, ui::ALIGN_CENTER, false);
            return;
        };
        if c.items.is_empty() {
            let msg = c.error.clone().unwrap_or_else(|| if c.loading { "Loading\u{2026}".into() } else { "No comments".into() });
            ui::text(p, &msg, 960.0, 500.0, 34, ui::DIM, ui::ALIGN_CENTER, false);
            return;
        }
        let focus = self.list.focused(cx, c.items.len());
        let top = self.list.top_for(focus, c.items.len());
        let vis = self.list.rows_visible();
        for (i, cm) in c.items.iter().enumerate().skip(top).take(vis) {
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
            ui::text(p, &head, r.x + 24.0, r.y + 14.0, 24, ui::DIM, ui::ALIGN_LEFT, true);
            for (j, line) in wrap(&cm.text, CHARS_PER_LINE, 3).iter().enumerate() {
                ui::text(p, line, r.x + 24.0, r.y + 50.0 + j as f32 * 34.0, TEXT, ui::WHITE, ui::ALIGN_LEFT, false);
            }
            let mut foot = String::new();
            if let Some(l) = &cm.like_count_text {
                foot.push_str(&format!("Likes {l}"));
            }
            if cm.reply_count > 0 {
                foot.push_str(&format!("   {} replies  (OK)", cm.reply_count));
            }
            ui::text(p, &foot, r.x + r.w - 24.0, r.y + 14.0, 22, ui::FAINT, ui::ALIGN_RIGHT, false);
        }
        if c.loading {
            ui::text(p, "Loading more\u{2026}", 960.0, 1040.0, 24, ui::FAINT, ui::ALIGN_CENTER, false);
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

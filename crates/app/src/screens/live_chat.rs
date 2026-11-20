//! Live chat of the live stream playing: a panel on the right like the comments panel (the video
//! shrinks beside it), newest message at the bottom. The list follows new messages while it is
//! at the bottom; Up and Down (or the wheel) scroll back through older ones, and coming back to
//! the bottom follows again.
//!
//! Messages are polled only while the panel is open and can be seen (`crate::live_chat`).
//!
//! OLED care: the panel dims with the player's screen dimming (and a key on it counts as one on
//! the player), its fixed title and notes turn faint after a few seconds, a list left scrolled
//! back follows the newest messages again after a while, and the panel closes itself a while
//! after nothing more can come (the chat or the video ended, playback failed).

use crate::host::{YtFx, YtHost, YtMsg};
use crate::ui::{self, VList};
use plx_machine::machine::{Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, Key, LogicalState, Machine, NavOp, ScreenEvent, TimerId};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use std::borrow::Cow;
use std::collections::HashMap;
use yt_core::live_chat::{ChatMessage, Kind, Role};

const G: GroupId = GroupId(1);
const BASE: u32 = 0x7100_0000;
const TEXT: i32 = 26;
/// The same place as the comments panel, so the video shrinks the same way.
const PANEL: Rect = Rect::new(1236.0, 0.0, 684.0, 1080.0);
const VIDEO: [i32; 4] = [10, 198, 1216, 684];
/// Gap between two messages.
const GAP: f32 = 6.0;
/// Line pitch of the message text, and the height of the author line above it.
const LINE: f32 = 31.0;
const HEAD: f32 = 28.0;
/// Wheel travel per unit (px).
const WHEEL_PX: f32 = 150.0;
/// The panel's periodic look (see `ScreenEvent::Timer`).
const T_CHECK: TimerId = TimerId(1);
const CHECK_MS: u32 = 2000;
/// The fixed title and notes are drawn faint after this long open.
const BRIGHT_MS: u32 = 4000;
/// A list left scrolled back follows the newest messages again after this long without a key.
const REFOLLOW_MS: u32 = 60_000;
/// The panel closes itself this long after nothing more can come to it.
const CLOSE_AFTER_MS: u32 = 30_000;

/// The panel closes itself: nothing has come for `CLOSE_AFTER_MS` since `over_since`.
fn closes(over_since: Option<u32>, now_ms: u32) -> bool {
    over_since.is_some_and(|t| now_ms.wrapping_sub(t) >= CLOSE_AFTER_MS)
}

/// The live chat of the video playing can be opened: the option is on and the video is live.
/// Gives (video id, the chat's first continuation when the player's watch page had one; the TV
/// watch page usually has none, and the chat looks it up itself).
pub fn available(cx: &Cx<'_, YtHost>) -> Option<(String, Option<String>)> {
    let s = &cx.views.store.settings;
    let st = cx.views.player;
    // Only engine B tells a live stream from its playback; on the system player the card it was
    // opened from does.
    let live = st.live || cx.views.store.card_is_live(&st.video_id);
    if !s.live_chat || s.child_mode || !live || st.video_id.is_empty() {
        return None;
    }
    let token = st.extras.watch.as_ref().filter(|w| w.video_id == st.video_id).and_then(|w| w.live_chat_continuation.clone());
    Some((st.video_id.clone(), token))
}

/// Height of one message: author line, every text line, the padding around them.
fn message_h(lines: usize) -> f32 {
    8.0 + HEAD + lines as f32 * LINE + 8.0
}

/// The author's name colour: the channel owner, moderators and members stand out as on YouTube.
fn author_ink(r: Role) -> [f32; 4] {
    match r {
        Role::Owner => [1.0, 0.84, 0.3, 1.0],
        Role::Moderator => [0.5, 0.68, 1.0, 1.0],
        Role::Member => [0.45, 0.88, 0.5, 1.0],
        Role::Viewer => ui::DIM,
    }
}

/// The row behind a Super Chat (its tier colour, darkened so white text reads on it) or a
/// membership message; None for plain messages.
fn ground(m: &ChatMessage) -> Option<[f32; 4]> {
    match &m.kind {
        Kind::Paid { .. } => {
            let c = m.color.unwrap_or(0xFF1E88E5);
            let ch = |s: u32| ((c >> s) & 0xFF) as f32 / 255.0 * 0.6;
            Some([ch(16), ch(8), ch(0), 0.9])
        }
        Kind::Membership { .. } => Some([0.06, 0.36, 0.16, 0.9]),
        Kind::Text => None,
    }
}

/// Wrapped lines and height of each message laid out so far, by message id (built in `draw`,
/// where text can be measured).
#[derive(Default)]
struct Layout {
    rows: HashMap<String, (Vec<String>, f32)>,
}

fn key_of(m: &ChatMessage) -> String {
    if m.id.is_empty() { format!("{}\u{1}{}", m.author, m.text) } else { m.id.clone() }
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

pub struct LiveChatScreen {
    list: VList,
    scroll: crate::live_chat::Scroll,
    /// As drawn last: the offset from the bottom, the largest offset and the newest message (what
    /// a key or the wheel scrolls from).
    last: std::cell::Cell<(f32, f32)>,
    newest: Option<String>,
    layout: std::cell::RefCell<Layout>,
    opened_ms: u32,
    /// The video was given back (or never taken: the panel is not over a playing video).
    released: bool,
    closing_at: Option<u32>,
    /// When nothing more could come to the panel (None: the chat goes on).
    over_since: Option<u32>,
    /// The last key or wheel on the panel.
    last_key_ms: u32,
    state: State,
}

impl LiveChatScreen {
    pub fn new(entry: EntryId) -> Self {
        LiveChatScreen {
            list: VList { entry, group: G, base: BASE, rect: Rect::new(PANEL.x + 20.0, 100.0, PANEL.w - 40.0, 970.0), row_h: 970.0, text_size: TEXT },
            scroll: Default::default(),
            last: std::cell::Cell::new((0.0, 0.0)),
            newest: None,
            layout: Default::default(),
            opened_ms: 0,
            released: true,
            closing_at: None,
            over_since: None,
            last_key_ms: 0,
            state: State("live chat".into()),
        }
    }

    /// Stops the polling and gives the video back (once), as the comments panel does.
    fn release(&mut self, fx: &mut Effects<'_, YtHost>) {
        fx.push(Fx::App(YtFx::LiveChat(None)));
        if std::mem::replace(&mut self.released, true) {
            return;
        }
        ui::COMMENTS_OPEN.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        if !ui::comments_open() {
            fx.push(Fx::App(YtFx::VideoRect(None)));
        }
    }

    /// A key or the wheel on the panel: the player counts it; true when it lifts the player's
    /// dimming and so does nothing else.
    fn note_key(&mut self, now_ms: u32) -> bool {
        self.last_key_ms = now_ms;
        ui::panel_key(now_ms);
        let (dim, lifts) = ui::player_dim();
        dim > 0.0 && lifts
    }

    fn scroll_by(&mut self, by: f32, fx: &mut Effects<'_, YtHost>) {
        let (offset, max) = self.last.get();
        self.scroll.scroll_by(by, offset, max, self.newest.as_deref());
        fx.invalidate(plx_machine::present::Provenance::Input);
    }
}

impl Machine<YtHost> for LiveChatScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                if let Some((video_id, token)) = available(cx) {
                    fx.push(Fx::App(YtFx::LiveChat(Some((video_id, token)))));
                }
                let st = cx.views.player;
                if !st.video_id.is_empty() && !matches!(st.phase, crate::player::Phase::Idle | crate::player::Phase::Failed(_)) {
                    self.released = false;
                    fx.push(Fx::App(YtFx::VideoRect(Some(VIDEO))));
                    ui::COMMENTS_OPEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                self.opened_ms = cx.views.now_ms;
                self.last_key_ms = cx.views.now_ms;
                fx.push(Fx::Timer { id: T_CHECK, after_ms: CHECK_MS });
            }
            ScreenEvent::Timer(T_CHECK) => {
                let now = cx.views.now_ms;
                let st = cx.views.player;
                if self.closing_at.is_some() {
                    return Handled::Yes;
                }
                if cx.views.store.live_chat.over(&st.video_id, &st.phase) {
                    self.over_since.get_or_insert(now);
                } else {
                    self.over_since = None;
                }
                if closes(self.over_since, now) {
                    self.release(fx);
                    self.closing_at = Some(now);
                    fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
                    fx.invalidate(plx_machine::present::Provenance::Timer(T_CHECK));
                    return Handled::Yes;
                }
                if !self.scroll.following() && now.wrapping_sub(self.last_key_ms) >= REFOLLOW_MS {
                    self.scroll = Default::default();
                    fx.invalidate(plx_machine::present::Provenance::Timer(T_CHECK));
                }
                // The title turning faint.
                let open_for = now.wrapping_sub(self.opened_ms);
                if (BRIGHT_MS..BRIGHT_MS + CHECK_MS).contains(&open_for) {
                    fx.invalidate(plx_machine::present::Provenance::Timer(T_CHECK));
                }
                fx.push(Fx::Timer { id: T_CHECK, after_ms: CHECK_MS });
            }
            // Any key or the wheel here is a key for the player's screen dimming too; a dim a key
            // lifts takes the key (as on the player, it does nothing else).
            ScreenEvent::Input(InputEvent { kind, .. })
                if matches!(kind, InputKind::Key { edge: Edge::Down, .. } | InputKind::Wheel { .. }) && self.note_key(cx.views.now_ms) =>
            {
                fx.invalidate(plx_machine::present::Provenance::Input);
            }
            ScreenEvent::Uncover => {
                if !self.released {
                    fx.push(Fx::App(YtFx::VideoRect(Some(VIDEO))));
                }
                fx.invalidate(plx_machine::present::Provenance::Nav);
            }
            ScreenEvent::Unmount => self.release(fx),
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Back, edge: Edge::Down, .. }, .. }) => {
                self.release(fx);
                self.closing_at = Some(cx.views.now_ms);
                fx.invalidate(plx_machine::present::Provenance::Input);
                return Handled::No;
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Wheel { dy }, .. }) => self.scroll_by(*dy * WHEEL_PX, fx),
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: k @ (Key::Up | Key::Down), edge: Edge::Down | Edge::Repeat, .. }, .. }) => {
                let step = self.list.rect.h * 0.4;
                self.scroll_by(if *k == Key::Up { step } else { -step }, fx);
            }
            ScreenEvent::Async(_, YtMsg::LiveChat) => fx.invalidate(plx_machine::present::Provenance::Lifecycle),
            ScreenEvent::Input(_) => return Handled::No,
            _ => {}
        }
        Handled::Yes
    }
}

impl Focusable<YtHost> for LiveChatScreen {
    fn groups(&self, _cx: &Cx<'_, YtHost>, out: &mut Vec<GroupSpec>) {
        // One stop: the whole list (Up and Down scroll it instead of moving between messages).
        out.push(self.list.spec(1));
    }
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        Some(G)
    }
    fn neighbour(&self, _key: FocusKey<u32>, _dir: Dir, _cx: &Cx<'_, YtHost>) -> Step<u32> {
        Step::Edge
    }
    fn place(&self, key: &u32, _cx: &Cx<'_, YtHost>, _at: At) -> Option<Placed> {
        self.list.index(*key, 1)?;
        let r = self.list.rect;
        Some(Placed { rect: r, rest_rect: r, clip: r, index: Some(0) })
    }
    fn reconcile(&self, _want: FocusKey<u32>, _cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        self.list.key(0)
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        self.list.key(0)
    }
}

impl Screen<YtHost> for LiveChatScreen {
    fn name(&self) -> &'static str {
        "live chat"
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
        // Slides in from the right, and quickly out again on BACK (as the comments panel).
        let k = ((cx.views.now_ms.wrapping_sub(self.opened_ms)) as f32 / 260.0).clamp(0.0, 1.0);
        let ease = 1.0 - (1.0 - k).powi(3);
        if ease < 1.0 {
            ui::animating();
        }
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
        // With the player's screen dimming: switched off, nothing; else the same black over it.
        let (dim, _) = ui::player_dim();
        if dim >= 1.0 {
            return;
        }
        let p = p.translate(PANEL.w * (1.0 - ease + out), 0.0);
        self.draw_panel(f, p);
        if dim > 0.0 {
            ui::fill(p, PANEL, 0.0, [0.0, 0.0, 0.0, dim]);
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

impl LiveChatScreen {
    fn draw_panel(&mut self, f: &mut DrawFrame<'_, '_, YtHost>, p: plx_ui::Painter) {
        let cx = f.cx;
        // The fixed title and notes: faint after a few seconds (they would stay put for hours).
        let calm = cx.views.now_ms.wrapping_sub(self.opened_ms) >= BRIGHT_MS;
        ui::fill(p, PANEL, 0.0, [0.06, 0.06, 0.08, 1.0]);
        ui::text(p, "Live chat", PANEL.x + 40.0, 36.0, 34, if calm { ui::FAINT } else { ui::WHITE }, ui::ALIGN_LEFT, true);
        let r = self.list.rect;
        ui::stop(f, self.list.key(0), r, r);
        let chat = &cx.views.store.live_chat;
        let msgs: &[ChatMessage] = if chat.video_id == cx.views.player.video_id { &chat.messages } else { &[] };
        self.newest = msgs.last().map(key_of);
        if msgs.is_empty() {
            let msg = if chat.video_id != cx.views.player.video_id {
                if available(cx).is_none() { "This video has no live chat" } else { "Loading\u{2026}" }
            } else if chat.no_chat {
                "This video has no live chat"
            } else if chat.ended {
                "The live chat has ended"
            } else if chat.loading {
                "Loading\u{2026}"
            } else if let Some(e) = chat.error.as_deref() {
                e
            } else {
                "No messages yet"
            };
            let msg = crate::i18n::tr(msg);
            ui::text_cut(p, f.measure, msg, PANEL.x + PANEL.w / 2.0, 500.0, PANEL.w - 80.0, 30, if calm { ui::FAINT } else { ui::DIM }, ui::ALIGN_CENTER, false);
            return;
        }
        // Lay out what arrived since the last frame; forget messages no longer kept.
        let mut l = self.layout.borrow_mut();
        let w = r.w - 40.0;
        for m in msgs {
            let k = key_of(m);
            if !l.rows.contains_key(&k) {
                let lines = if m.text.is_empty() { Vec::new() } else { super::comments::wrap_px(f.measure, &m.text, w, TEXT) };
                let h = message_h(lines.len());
                l.rows.insert(k, (lines, h));
            }
        }
        if l.rows.len() > msgs.len() * 2 {
            let keep: std::collections::HashSet<String> = msgs.iter().map(key_of).collect();
            l.rows.retain(|k, _| keep.contains(k));
        }
        let mut tops = Vec::with_capacity(msgs.len());
        let mut total = 0.0f32;
        for m in msgs {
            if !tops.is_empty() {
                total += GAP;
            }
            tops.push(total);
            total += l.rows[&key_of(m)].1;
        }
        let bottoms: HashMap<String, f32> = msgs.iter().zip(&tops).map(|(m, t)| (key_of(m), t + l.rows[&key_of(m)].1)).collect();
        let offset = self.scroll.offset(|id| bottoms.get(id).copied(), total, r.h);
        self.last.set((offset, (total - r.h).max(0.0)));
        // The newest message's bottom on the list's bottom (when following), older ones above.
        let y_of = |top: f32| r.y + r.h - (total - top) + offset;
        let _clip = plx_ui::screen::ClipScope::open_in(f.painter, Rect::new(PANEL.x, r.y - 8.0, PANEL.w, r.h + 8.0));
        for (m, top) in msgs.iter().zip(&tops) {
            let (lines, h) = &l.rows[&key_of(m)];
            let y = y_of(*top);
            if y + h < r.y - 8.0 || y > r.y + r.h {
                continue;
            }
            let row = Rect::new(r.x, y, r.w, *h);
            if let Some(g) = ground(m) {
                ui::fill(p, row, 12.0, g);
            }
            let mut x = row.x + 20.0;
            x += ui::text_cut(p, f.measure, &m.author, x, y + 8.0, (row.w - 40.0) * 0.6, 22, author_ink(m.role), ui::ALIGN_LEFT, true);
            let extra = match &m.kind {
                Kind::Paid { amount } => Some(amount.as_str()),
                Kind::Membership { header } if !header.is_empty() => Some(header.as_str()),
                _ => None,
            };
            if let Some(e) = extra {
                ui::text_cut(p, f.measure, &format!("  \u{2022}  {e}"), x, y + 8.0, row.x + row.w - 20.0 - x, 22, ui::WHITE, ui::ALIGN_LEFT, true);
            }
            for (j, line) in lines.iter().enumerate() {
                let ly = y + 8.0 + HEAD + j as f32 * LINE;
                if ly + LINE < r.y - 8.0 || ly > r.y + r.h {
                    continue;
                }
                ui::text_raw(p, line, row.x + 20.0, ly, TEXT, ui::WHITE, ui::ALIGN_LEFT, false);
            }
        }
        drop(_clip);
        if self.scroll.newer_below(self.newest.as_deref()) {
            ui::text_raw(p, &format!("\u{25BC}  {}", crate::i18n::tr("Newer messages below")), PANEL.x + PANEL.w / 2.0, 1046.0, 22, ui::FAINT, ui::ALIGN_CENTER, false);
        } else if chat.ended {
            ui::text(p, "The live chat has ended", PANEL.x + PANEL.w / 2.0, 1046.0, 22, ui::FAINT, ui::ALIGN_CENTER, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closes_a_while_after_nothing_more_can_come() {
        assert!(!closes(None, 1_000_000));
        assert!(!closes(Some(10_000), 10_000 + CLOSE_AFTER_MS - 1));
        assert!(closes(Some(10_000), 10_000 + CLOSE_AFTER_MS));
        // Across the frame clock's wrap.
        assert!(closes(Some(u32::MAX - 100), CLOSE_AFTER_MS));
    }
}

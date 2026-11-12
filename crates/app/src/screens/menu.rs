//! Modal menus: a card's actions (hold OK on a card) and the player options (Up in the player).
//! A panel on the right; OK runs an action, Left/Right change a player option in place.

use crate::host::{UserAction, YtArg, YtFx, YtHost};
use crate::settings::Repeat;
use crate::store::FeedKey;
use crate::ui::{self, Row, VList};
use plx_machine::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, Key, LogicalState, Machine, NavOp, ScreenEvent,
};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use std::borrow::Cow;
use yt_core::model::Endpoint;

const G: GroupId = GroupId(1);
const BASE: u32 = 0x5000_0000;
const PANEL: Rect = Rect::new(1220.0, 0.0, 700.0, 1080.0);

#[derive(Clone, Debug)]
enum Act {
    Play(String),
    OpenChannel(String),
    OpenPlaylist(String),
    BlockChannel(String),
    SearchFor(String),
    User(UserAction),
    Comments(String),
    Speed,
    MaxQuality,
    Subtitles,
    RepeatMode,
    Close,
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

pub struct MenuScreen {
    title: String,
    acts: Vec<(String, Act)>,
    list: VList,
    state: State,
}

const SPEEDS: &[f32] = &[0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0];

impl MenuScreen {
    fn make(entry: EntryId, title: String, acts: Vec<(String, Act)>) -> Self {
        MenuScreen {
            state: State(title.clone()),
            title,
            acts,
            list: VList { entry, group: G, base: BASE, rect: Rect::new(PANEL.x + 40.0, 200.0, PANEL.w - 80.0, 820.0), row_h: 74.0, text_size: 30 },
        }
    }

    pub fn card(entry: EntryId, elem: u32, cx: &Cx<'_, YtHost>) -> Self {
        let mut acts = Vec::new();
        let mut title = "Options".to_string();
        if let Some(card) = cx.views.store.card(elem) {
            title = card.title.clone();
            if let Some(v) = card.video_id() {
                acts.push(("Play".to_string(), Act::Play(v.to_string())));
            }
            if let Some(Endpoint::Browse { browse_id, .. }) = &card.item.endpoint {
                if browse_id.starts_with("UC") {
                    acts.push(("Open channel".into(), Act::OpenChannel(browse_id.clone())));
                } else {
                    acts.push(("Open".into(), Act::OpenPlaylist(browse_id.clone())));
                }
            }
            if let Some(v) = card.video_id() {
                acts.push(("Save to Watch later".to_string(), Act::User(UserAction::WatchLater(v.to_string()))));
            }
            // The card's own YouTube menu (Not interested, Don't recommend channel, Remove from
            // history, ...). Lazy "More" panels and plain navigation entries are left out.
            for m in &card.item.menu {
                use yt_core::actions::{MenuCommand, MenuKind};
                let useful = matches!(
                    m.kind,
                    MenuKind::NotInterested | MenuKind::DontRecommendChannel | MenuKind::RemoveFromHistory | MenuKind::RemoveFromWatchLater
                ) || matches!(m.command, MenuCommand::Feedback { .. });
                if useful && !m.label.is_empty() {
                    acts.push((m.label.clone(), Act::User(UserAction::Menu(m.clone()))));
                }
            }
            if let Some(ch) = &card.item.channel {
                acts.push((format!("More from {ch}"), Act::SearchFor(ch.clone())));
                acts.push((format!("Block {ch}"), Act::BlockChannel(ch.clone())));
            }
        }
        acts.push(("Close".into(), Act::Close));
        Self::make(entry, title, acts)
    }

    pub fn player(entry: EntryId, cx: &Cx<'_, YtHost>) -> Self {
        let st = cx.views.player;
        let mut acts = Vec::new();
        if !st.video_id.is_empty() {
            use yt_core::actions::Rating;
            let liked = st.extras.watch.as_ref().map(|w| w.like_status);
            let is_like = matches!(liked, Some(yt_core::watch::LikeStatus::Like));
            let is_dislike = matches!(liked, Some(yt_core::watch::LikeStatus::Dislike));
            acts.push((
                if is_like { "Remove like".to_string() } else { "Like".to_string() },
                Act::User(UserAction::Rate(st.video_id.clone(), if is_like { Rating::None } else { Rating::Like })),
            ));
            acts.push((
                if is_dislike { "Remove dislike".to_string() } else { "Dislike".to_string() },
                Act::User(UserAction::Rate(st.video_id.clone(), if is_dislike { Rating::None } else { Rating::Dislike })),
            ));
            if !st.channel_id.is_empty() {
                let subscribed = st.extras.watch.as_ref().and_then(|w| w.subscribed).unwrap_or(false);
                acts.push((
                    if subscribed { format!("Unsubscribe from {}", st.author) } else { format!("Subscribe to {}", st.author) },
                    Act::User(UserAction::Subscribe { channel_id: st.channel_id.clone(), subscribe: !subscribed }),
                ));
            }
            acts.push(("Save to Watch later".to_string(), Act::User(UserAction::WatchLater(st.video_id.clone()))));
            acts.push(("Comments".to_string(), Act::Comments(st.video_id.clone())));
            if !st.channel_id.is_empty() {
                acts.push((format!("Open {}", st.author), Act::OpenChannel(st.channel_id.clone())));
            }
        }
        acts.extend(vec![
            ("Playback speed".to_string(), Act::Speed),
            ("Maximum quality".to_string(), Act::MaxQuality),
            ("Subtitles".to_string(), Act::Subtitles),
            ("When the video ends".to_string(), Act::RepeatMode),
            ("Close".to_string(), Act::Close),
        ]);
        Self::make(entry, "Player options".into(), acts)
    }

    fn value(&self, act: &Act, cx: &Cx<'_, YtHost>) -> Option<String> {
        let s = &cx.views.store.settings;
        Some(match act {
            Act::Speed => {
                let r = if cx.views.player.rate > 0.0 { cx.views.player.rate } else { 1.0 };
                format!("\u{2039}  {r}x  \u{203A}")
            }
            Act::MaxQuality => format!("\u{2039}  {}p  \u{203A}", s.max_height),
            Act::Subtitles => format!("\u{2039}  {}  \u{203A}", if s.subtitles { "On" } else { "Off" }),
            Act::RepeatMode => format!(
                "\u{2039}  {}  \u{203A}",
                match s.repeat {
                    Repeat::Autoplay => "Play next",
                    Repeat::Off => "Stop",
                    Repeat::One => "Repeat",
                    Repeat::List => "Repeat list",
                }
            ),
            _ => return None,
        })
    }

    fn run(&self, act: &Act, dir: i32, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        match act {
            Act::Play(v) => {
                fx.push(Fx::Nav(NavOp::Pop));
                fx.push(Fx::Nav(NavOp::Push(YtArg::Player { video_id: v.clone(), playlist: None })));
            }
            Act::OpenChannel(id) | Act::OpenPlaylist(id) => {
                fx.push(Fx::Nav(NavOp::Pop));
                fx.push(Fx::Nav(NavOp::Push(YtArg::Feed(FeedKey::Browse { id: id.clone(), params: None }))));
            }
            Act::SearchFor(q) => {
                fx.push(Fx::Nav(NavOp::Pop));
                fx.push(Fx::Nav(NavOp::Push(YtArg::Feed(FeedKey::Search { query: q.clone(), params: None }))));
            }
            Act::BlockChannel(ch) => {
                let ch = ch.clone();
                fx.push(Fx::App(YtFx::Toast(format!("{ch} blocked (Settings \u{203A} Content filters)"))));
                fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| {
                    if !s.blocked_channels.contains(&ch) {
                        s.blocked_channels.push(ch);
                    }
                }))));
                fx.push(Fx::Nav(NavOp::Pop));
            }
            Act::Speed => {
                let cur = if cx.views.player.rate > 0.0 { cx.views.player.rate } else { 1.0 };
                let i = SPEEDS.iter().position(|s| (*s - cur).abs() < 0.01).unwrap_or(3) as i32;
                let r = SPEEDS[((i + dir).clamp(0, SPEEDS.len() as i32 - 1)) as usize];
                fx.push(Fx::App(YtFx::SetSpeed(r)));
            }
            Act::MaxQuality => {
                fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| {
                    const Q: &[u32] = &[480, 720, 1080, 1440, 2160];
                    let i = Q.iter().position(|q| *q == s.max_height).unwrap_or(2) as i32;
                    s.max_height = Q[((i + dir).clamp(0, Q.len() as i32 - 1)) as usize];
                    s.min_height = s.min_height.min(s.max_height);
                }))));
                fx.push(Fx::App(YtFx::Toast("Applies to the next video".into())));
            }
            Act::Subtitles => fx.push(Fx::App(YtFx::EditSettings(Box::new(|s| s.subtitles = !s.subtitles)))),
            Act::RepeatMode => fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| {
                const R: &[Repeat] = &[Repeat::Autoplay, Repeat::Off, Repeat::One, Repeat::List];
                let i = R.iter().position(|r| *r == s.repeat).unwrap_or(0) as i32;
                s.repeat = R[((i + dir).rem_euclid(R.len() as i32)) as usize];
            })))),
            Act::User(a) => {
                fx.push(Fx::App(YtFx::Act(a.clone())));
                fx.push(Fx::Nav(NavOp::Pop));
            }
            Act::Comments(v) => {
                fx.push(Fx::Nav(NavOp::Pop));
                fx.push(Fx::Nav(NavOp::Push(YtArg::Comments(v.clone()))));
            }
            Act::Close => fx.push(Fx::Nav(NavOp::Pop)),
        }
        fx.invalidate(plx_machine::present::Provenance::Input);
    }
}

impl Machine<YtHost> for MenuScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        let n = self.acts.len();
        match ev {
            ScreenEvent::Activate(e) => {
                if let Some(i) = self.list.index(*e, n) {
                    let act = self.acts[i].1.clone();
                    self.run(&act, 1, cx, fx);
                }
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: key @ (Key::Left | Key::Right), edge: Edge::Down | Edge::Repeat, .. }, .. }) => {
                if let Some(i) = self.list.focused(cx, n) {
                    let act = self.acts[i].1.clone();
                    if self.value(&act, cx).is_some() {
                        self.run(&act, if *key == Key::Left { -1 } else { 1 }, cx, fx);
                    }
                }
                Handled::Yes
            }
            ScreenEvent::Input(_) => Handled::No,
            _ => Handled::Yes,
        }
    }
}

impl Focusable<YtHost> for MenuScreen {
    fn groups(&self, _cx: &Cx<'_, YtHost>, out: &mut Vec<GroupSpec>) {
        out.push(self.list.spec(self.acts.len()));
    }
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        Some(G)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, YtHost>) -> Step<u32> {
        self.list.neighbour(key, dir, self.acts.len())
    }
    fn place(&self, key: &u32, cx: &Cx<'_, YtHost>, _at: At) -> Option<Placed> {
        let n = self.acts.len();
        self.list.place(*key, n, self.list.focused(cx, n))
    }
    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        if self.list.index(want.elem, self.acts.len()).is_some() { want } else { self.list.key(0) }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        self.list.key(0)
    }
}

impl Screen<YtHost> for MenuScreen {
    fn name(&self) -> &'static str {
        "menu"
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
        ui::fill(p, PANEL, 0.0, ui::PANEL);
        ui::text(p, &ui::ellipsize(&self.title, 30), PANEL.x + 60.0, 90.0, 36, ui::WHITE, ui::ALIGN_LEFT, true);
        let rows: Vec<Row> = self
            .acts
            .iter()
            .map(|(label, act)| {
                let r = Row::new(label.clone());
                match self.value(act, cx) {
                    Some(v) => r.value(v),
                    None => r,
                }
            })
            .collect();
        self.list.draw(p, &rows, self.list.focused(cx, rows.len()));
        super::common::toast(p, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

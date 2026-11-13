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
const PANEL: Rect = Rect::new(1300.0, 0.0, 620.0, 1080.0);

#[derive(Clone, Debug)]
enum Act {
    Play(String),
    OpenChannel(String),
    OpenPlaylist(String),
    BlockChannel(String),
    Pin(String, String, Option<String>),
    AddToGroup(Option<String>, String, String, Option<String>),
    Unpin(String),
    SearchFor(String),
    User(UserAction),
    Comments(String),
    Info,
    Filter(u8),
    PickPlaylist(String, String),
    TogglePlaylist(String, String, bool),
    NewPlaylist(String, String),
    ApplyFilters(String),
    Speed,
    AudioTrack,
    ZoomMode,
    Sleep,
    MaxQuality,
    Subtitles,
    SubSize,
    SubColor,
    SubStyle,
    SubFont,
    SubBold,
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
    /// Set on the playlist picker: rows are rebuilt when the playlists arrive.
    picker_for: Option<String>,
    title: String,
    acts: Vec<(String, Act)>,
    list: VList,
    /// An option's drop-down list, while open.
    choice: Option<ui::ChoiceBox>,
    closed_row: std::cell::Cell<usize>,
    /// Row to put focus on at the next reconcile (the playlist picker after its list changed).
    refocus: std::cell::Cell<Option<usize>>,
    state: State,
}

const SUB_SIZES: &[u32] = &[32, 38, 44, 52, 60, 72];
const SUB_COLORS: &[&str] = &["white", "light gray", "gray", "dark gray", "yellow", "light yellow", "cyan", "green", "orange", "pink"];
const SUB_STYLES: &[&str] = &["outline", "shadow", "box"];
const SUB_FONTS: &[&str] = &["default", "sans", "serif", "mono", "roboto"];
const SPEEDS: &[f32] = &[0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0];

/// Card-menu item kinds, in the default order, with their settings labels.
pub const CARD_ITEMS: [(&str, &str); 9] = [
    ("play", "Card menu: Play"),
    ("open", "Card menu: Open channel / playlist"),
    ("groups", "Card menu: Channel groups"),
    ("pin", "Card menu: Pin to sidebar"),
    ("watch_later", "Card menu: Save to Watch later"),
    ("playlist", "Card menu: Save to playlist"),
    ("youtube", "Card menu: YouTube's items"),
    ("more_from", "Card menu: More from channel"),
    ("block", "Card menu: Block channel"),
];

impl MenuScreen {
    fn make(entry: EntryId, title: String, acts: Vec<(String, Act)>) -> Self {
        MenuScreen {
            picker_for: None,
            choice: None,
            closed_row: std::cell::Cell::new(0),
            refocus: std::cell::Cell::new(None),
            state: State(title.clone()),
            title,
            acts,
            list: VList { entry, group: G, base: BASE, rect: Rect::new(PANEL.x + 24.0, 130.0, PANEL.w - 48.0, 920.0), row_h: 54.0, text_size: 26 },
        }
    }

    pub fn card(entry: EntryId, elem: u32, cx: &Cx<'_, YtHost>) -> Self {
        // (kind, label, action): kinds order and hide the items per the settings.
        let mut tagged: Vec<(&'static str, String, Act)> = Vec::new();
        let mut acts = Vec::new();
        let mut kind = "play";
        let mut title = "Options".to_string();
        if let Some(card) = cx.views.store.card(elem) {
            title = card.title.clone();
            if let Some(v) = card.video_id() {
                kind = "play";
                tagged.push((kind, "Play".to_string(), Act::Play(v.to_string())));
            }
            if let Some(Endpoint::Browse { browse_id, params }) = &card.item.endpoint {
                if browse_id.starts_with("UC") {
                    tagged.push(("open", "Open channel".into(), Act::OpenChannel(browse_id.clone())));
                } else {
                    tagged.push(("open", "Open".into(), Act::OpenPlaylist(browse_id.clone())));
                }
                if browse_id.starts_with("UC") {
                    for (gid, gt) in &cx.views.store.groups {
                        tagged.push(("groups", format!("Add to group \u{201C}{gt}\u{201D}"), Act::AddToGroup(Some(gid.clone()), browse_id.clone(), card.title.clone(), card.item.thumbnail.clone())));
                    }
                    tagged.push(("groups", "Add to a new group".into(), Act::AddToGroup(None, browse_id.clone(), card.title.clone(), card.item.thumbnail.clone())));
                }
                if cx.views.store.settings.pinned.iter().any(|p| &p.1 == browse_id) {
                    tagged.push(("pin", "Unpin from sidebar".into(), Act::Unpin(browse_id.clone())));
                } else {
                    tagged.push(("pin", "Pin to sidebar".into(), Act::Pin(card.title.clone(), browse_id.clone(), params.clone())));
                }
            }
            if let Some(v) = card.video_id() {
                tagged.push(("watch_later", "Save to Watch later".to_string(), Act::User(UserAction::WatchLater(v.to_string()))));
                tagged.push(("playlist", "Save to playlist\u{2026}".to_string(), Act::PickPlaylist(v.to_string(), card.title.clone())));
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
                    tagged.push(("youtube", m.label.clone(), Act::User(UserAction::Menu(m.clone()))));
                }
            }
            if let Some(ch) = card.item.channel.as_ref().filter(|_| !cx.views.store.settings.child_mode) {
                tagged.push(("more_from", format!("More from {ch}"), Act::SearchFor(ch.clone())));
                tagged.push(("block", format!("Block {ch}"), Act::BlockChannel(ch.clone())));
            }
        }
        let s = &cx.views.store.settings;
        let rank = |k: &str| s.card_menu.iter().position(|x| x == k).unwrap_or(usize::MAX);
        tagged.sort_by_key(|(k, _, _)| rank(k)); // stable: items of one kind keep their order
        let _ = kind;
        acts.extend(tagged.into_iter().filter(|(k, _, _)| !s.card_menu_hidden.iter().any(|h| h == k)).map(|(_, l, a)| (l, a)));
        acts.push(("Close".into(), Act::Close));
        Self::make(entry, title, acts)
    }

    /// "Save to playlist": one row per playlist (ticked when it already holds the video).
    pub fn playlists(entry: EntryId, video_id: String, title: String, cx: &Cx<'_, YtHost>) -> Self {
        let mut acts = Vec::new();
        for o in cx.views.store.playlist_options.get(&video_id).into_iter().flatten() {
            let mark = if o.contains { "\u{2713}  " } else { "     " };
            acts.push((format!("{mark}{}", o.title), Act::TogglePlaylist(o.playlist_id.clone(), video_id.clone(), !o.contains)));
        }
        acts.push(("+  New private playlist".to_string(), Act::NewPlaylist(title.clone(), video_id.clone())));
        acts.push(("Close".to_string(), Act::Close));
        let mut m = Self::make(entry, "Save to playlist".into(), acts);
        m.picker_for = Some(video_id);
        m
    }

    pub fn filters(entry: EntryId, query: String) -> Self {
        let acts = vec![
            ("Upload date".to_string(), Act::Filter(0)),
            ("Type".to_string(), Act::Filter(1)),
            ("Duration".to_string(), Act::Filter(2)),
            ("Sort by".to_string(), Act::Filter(3)),
            ("Live".to_string(), Act::Filter(4)),
            ("4K".to_string(), Act::Filter(5)),
            ("HDR".to_string(), Act::Filter(6)),
            ("Reset filters".to_string(), Act::Filter(7)),
            ("Apply".to_string(), Act::ApplyFilters(query)),
        ];
        Self::make(entry, "Search filters".into(), acts)
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
            acts.push(("Save to playlist\u{2026}".to_string(), Act::PickPlaylist(st.video_id.clone(), st.title.clone())));
            acts.push(("Video info & chapters".to_string(), Act::Info));
            if !cx.views.store.settings.child_mode {
                acts.push(("Comments".to_string(), Act::Comments(st.video_id.clone())));
            }
            if !st.channel_id.is_empty() {
                acts.push((format!("Open {}", st.author), Act::OpenChannel(st.channel_id.clone())));
            }
        }
        acts.extend(vec![
            ("Playback speed".to_string(), Act::Speed),
            ("Audio language".to_string(), Act::AudioTrack),
            ("Sleep timer".to_string(), Act::Sleep),
            ("Maximum quality".to_string(), Act::MaxQuality),
            ("Subtitles".to_string(), Act::Subtitles),
            ("Subtitle size".to_string(), Act::SubSize),
            ("Subtitle colour".to_string(), Act::SubColor),
            ("Subtitle style".to_string(), Act::SubStyle),
            ("Subtitle font".to_string(), Act::SubFont),
            ("Bold subtitles".to_string(), Act::SubBold),
            ("When the video ends".to_string(), Act::RepeatMode),
            ("Close".to_string(), Act::Close),
        ]);
        Self::make(entry, "Player options".into(), acts)
    }

    fn value(&self, act: &Act, cx: &Cx<'_, YtHost>) -> Option<String> {
        let s = &cx.views.store.settings;
        Some(match act {
            Act::Filter(i) => {
                use yt_core::search_filters::*;
                let f = &cx.views.store.search_filters;
                let v = match i {
                    0 => crate::i18n::tr(match f.upload_date {
                        UploadDate::Any => "Any time",
                        UploadDate::Hour => "Last hour",
                        UploadDate::Today => "Today",
                        UploadDate::Week => "This week",
                        UploadDate::Month => "This month",
                        UploadDate::Year => "This year",
                    })
                    .to_string(),
                    1 => crate::i18n::tr(match f.kind {
                        Kind::Any => "Any type",
                        Kind::Video => "Videos",
                        Kind::Channel => "Channels",
                        Kind::Playlist => "Playlists",
                        Kind::Movie => "Movies",
                    })
                    .to_string(),
                    2 => crate::i18n::tr(match f.duration {
                        Duration::Any => "Any length",
                        Duration::Short => "Under 4 minutes",
                        Duration::Medium => "4 to 20 minutes",
                        Duration::Long => "Over 20 minutes",
                    })
                    .to_string(),
                    3 => crate::i18n::tr(match f.sort {
                        Sort::Relevance => "Relevance",
                        Sort::Rating => "Rating",
                        Sort::UploadDate => "Upload date",
                        Sort::ViewCount => "View count",
                    })
                    .to_string(),
                    4 => crate::i18n::tr(if f.live { "On" } else { "Off" }).to_string(),
                    5 => crate::i18n::tr(if f.hd_4k { "On" } else { "Off" }).to_string(),
                    6 => crate::i18n::tr(if f.hdr { "On" } else { "Off" }).to_string(),
                    _ => return None,
                };
                let _ = (UploadDate::Any, Kind::Any);
                format!("{v}  \u{203A}")
            }
            Act::Speed => {
                let r = if cx.views.player.rate > 0.0 { cx.views.player.rate } else { 1.0 };
                format!("{r}x  \u{203A}")
            }
            Act::MaxQuality => format!("{}p  \u{203A}", s.max_height),
            Act::AudioTrack => {
                let st = cx.views.player;
                if st.audio_tracks.len() < 2 {
                    return Some("only one".into());
                }
                let cur = st.audio_track.clone().or_else(|| st.audio_tracks.iter().find(|t| t.2).map(|t| t.0.clone()));
                let name = st.audio_tracks.iter().find(|t| Some(&t.0) == cur.as_ref()).map(|t| t.1.clone()).unwrap_or_else(|| "default".into());
                format!("{}  \u{203A}", ui::ellipsize(&name, 18))
            }
            Act::ZoomMode => format!(
                "\u{2039}  {}  \u{203A}",
                crate::i18n::tr(match cx.views.player.zoom {
                    crate::player::Zoom::Fit => "Fit to screen",
                    crate::player::Zoom::Fill => "Fill the screen",
                    crate::player::Zoom::Zoom => "Zoom in",
                })
            ),
            Act::Sleep => match cx.views.store.sleep_minutes {
                0 => format!("{}  \u{203A}", crate::i18n::tr("Off")),
                m => format!("{m} min  \u{203A}"),
            },
            Act::Subtitles => format!("{}  \u{203A}", crate::i18n::tr(if s.subtitles { "On" } else { "Off" })),
            Act::SubSize => format!("{}  \u{203A}", s.subtitle_size),
            Act::SubColor => format!("{}  \u{203A}", crate::i18n::tr(&s.subtitle_color)),
            Act::SubBold => format!("{}  \u{203A}", crate::i18n::tr(if s.subtitle_bold { "On" } else { "Off" })),
            Act::SubFont => format!("{}  \u{203A}", crate::i18n::tr(&s.subtitle_font)),
            Act::SubStyle => format!("{}  \u{203A}", crate::i18n::tr(&s.subtitle_style)),
            Act::RepeatMode => format!(
                "{}  \u{203A}",
                crate::i18n::tr(match s.repeat {
                    Repeat::Autoplay => "Play next",
                    Repeat::Off => "Stop",
                    Repeat::One => "Repeat",
                    Repeat::List => "Repeat list",
                })
            ),
            _ => return None,
        })
    }

    /// The values a valued row can take (drop-down list) and the current one; picking value `k`
    /// is `run(act, k - current)`, every valued row stepping by `dir`.
    fn choices(&self, act: &Act, cx: &Cx<'_, YtHost>) -> Option<(Vec<String>, usize)> {
        let s = &cx.views.store.settings;
        Some(match act {
            Act::Speed => {
                let r = if cx.views.player.rate > 0.0 { cx.views.player.rate } else { 1.0 };
                (SPEEDS.iter().map(|v| format!("{v}x")).collect(), SPEEDS.iter().position(|v| (v - r).abs() < 0.01).unwrap_or(3))
            }
            Act::MaxQuality => {
                const Q: &[u32] = &[480, 720, 1080, 1440, 2160];
                (Q.iter().map(|q| format!("{q}p")).collect(), Q.iter().position(|q| *q == s.max_height).unwrap_or(2))
            }
            Act::AudioTrack => {
                let st = cx.views.player;
                if st.audio_tracks.len() < 2 {
                    return None;
                }
                let cur = st.audio_track.clone().or_else(|| st.audio_tracks.iter().find(|t| t.2).map(|t| t.0.clone()));
                (st.audio_tracks.iter().map(|t| t.1.clone()).collect(), st.audio_tracks.iter().position(|t| Some(&t.0) == cur.as_ref()).unwrap_or(0))
            }
            Act::Sleep => {
                const T: &[u32] = &[0, 15, 30, 60, 90];
                (
                    T.iter().map(|m| if *m == 0 { "Off".to_string() } else { format!("{m} min") }).collect(),
                    T.iter().position(|m| *m == cx.views.store.sleep_minutes).unwrap_or(0),
                )
            }
            Act::SubSize => (SUB_SIZES.iter().map(|v| v.to_string()).collect(), SUB_SIZES.iter().position(|v| *v == s.subtitle_size).unwrap_or(2)),
            Act::SubColor => (SUB_COLORS.iter().map(|v| v.to_string()).collect(), SUB_COLORS.iter().position(|v| *v == s.subtitle_color).unwrap_or(0)),
            Act::SubFont => (SUB_FONTS.iter().map(|v| v.to_string()).collect(), SUB_FONTS.iter().position(|v| *v == s.subtitle_font).unwrap_or(0)),
            Act::SubStyle => (SUB_STYLES.iter().map(|v| v.to_string()).collect(), SUB_STYLES.iter().position(|v| *v == s.subtitle_style).unwrap_or(0)),
            Act::RepeatMode => {
                const R: &[Repeat] = &[Repeat::Autoplay, Repeat::Off, Repeat::One, Repeat::List];
                (
                    vec!["Play next".into(), "Stop".into(), "Repeat".into(), "Repeat list".into()],
                    R.iter().position(|x| *x == s.repeat).unwrap_or(0),
                )
            }
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
            Act::AddToGroup(g, ch, t, icon) => {
                fx.push(Fx::App(YtFx::Act(UserAction::AddToGroup { group_id: g.clone(), channel_id: ch.clone(), title: t.clone(), icon: icon.clone() })));
                fx.push(Fx::Nav(NavOp::Pop));
            }
            Act::Pin(t, id, p) => {
                let (t, id, p) = (t.clone(), id.clone(), p.clone());
                fx.push(Fx::App(YtFx::Toast(format!("Pinned {t}"))));
                fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| s.pinned.push((t, id, p))))));
                fx.push(Fx::Nav(NavOp::Pop));
            }
            Act::Unpin(id) => {
                let id = id.clone();
                fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| s.pinned.retain(|p| p.1 != id)))));
                fx.push(Fx::App(YtFx::Toast("Unpinned".into())));
                fx.push(Fx::Nav(NavOp::Pop));
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
            Act::AudioTrack => {
                let st = cx.views.player;
                if st.audio_tracks.len() >= 2 {
                    let cur = st.audio_track.clone().or_else(|| st.audio_tracks.iter().find(|t| t.2).map(|t| t.0.clone()));
                    let i = st.audio_tracks.iter().position(|t| Some(&t.0) == cur.as_ref()).unwrap_or(0) as i32;
                    let n = st.audio_tracks.len() as i32;
                    let next = st.audio_tracks[((i + dir).rem_euclid(n)) as usize].0.clone();
                    fx.push(Fx::App(YtFx::SetAudioTrack(next)));
                }
            }
            Act::ZoomMode => {
                use crate::player::Zoom;
                const Z: &[Zoom] = &[Zoom::Fit, Zoom::Fill, Zoom::Zoom];
                let i = Z.iter().position(|z| *z == cx.views.player.zoom).unwrap_or(0) as i32;
                fx.push(Fx::App(YtFx::SetZoom(Z[((i + dir).rem_euclid(3)) as usize])));
            }
            Act::Sleep => {
                const M: &[u32] = &[0, 15, 30, 60, 90];
                let i = M.iter().position(|m| *m == cx.views.store.sleep_minutes).unwrap_or(0) as i32;
                fx.push(Fx::App(YtFx::SleepTimer(M[((i + dir).rem_euclid(M.len() as i32)) as usize])));
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
            Act::SubSize => fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| {
                let i = SUB_SIZES.iter().position(|v| *v == s.subtitle_size).unwrap_or(2) as i32;
                s.subtitle_size = SUB_SIZES[(i + dir).clamp(0, SUB_SIZES.len() as i32 - 1) as usize];
            })))),
            Act::SubColor => fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| {
                let i = SUB_COLORS.iter().position(|v| *v == s.subtitle_color).unwrap_or(0) as i32;
                s.subtitle_color = SUB_COLORS[(i + dir).rem_euclid(SUB_COLORS.len() as i32) as usize].into();
            })))),
            Act::SubBold => fx.push(Fx::App(YtFx::EditSettings(Box::new(|s| s.subtitle_bold = !s.subtitle_bold)))),
            Act::SubFont => fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| {
                let i = SUB_FONTS.iter().position(|v| *v == s.subtitle_font).unwrap_or(0) as i32;
                s.subtitle_font = SUB_FONTS[(i + dir).rem_euclid(SUB_FONTS.len() as i32) as usize].into();
            })))),
            Act::SubStyle => fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| {
                let i = SUB_STYLES.iter().position(|v| *v == s.subtitle_style).unwrap_or(0) as i32;
                s.subtitle_style = SUB_STYLES[(i + dir).rem_euclid(SUB_STYLES.len() as i32) as usize].into();
            })))),
            Act::RepeatMode => fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| {
                const R: &[Repeat] = &[Repeat::Autoplay, Repeat::Off, Repeat::One, Repeat::List];
                let i = R.iter().position(|r| *r == s.repeat).unwrap_or(0) as i32;
                s.repeat = R[((i + dir).rem_euclid(R.len() as i32)) as usize];
            })))),
            Act::User(a) => {
                fx.push(Fx::App(YtFx::Act(a.clone())));
                fx.push(Fx::Nav(NavOp::Pop));
            }
            Act::PickPlaylist(v, t) => {
                fx.push(Fx::App(YtFx::LoadPlaylists(v.clone())));
                fx.push(Fx::Nav(NavOp::Pop));
                fx.push(Fx::Nav(NavOp::Present(YtArg::PlaylistPicker(v.clone(), t.clone()))));
            }
            Act::TogglePlaylist(pl, v, add) => {
                fx.push(Fx::App(YtFx::Act(UserAction::Playlist { playlist_id: pl.clone(), video_id: v.clone(), add: *add })));
                fx.push(Fx::Nav(NavOp::Pop));
            }
            Act::NewPlaylist(t, v) => {
                fx.push(Fx::App(YtFx::Act(UserAction::NewPlaylist { title: t.clone(), video_id: v.clone() })));
                fx.push(Fx::Nav(NavOp::Pop));
            }
            Act::Info => {
                fx.push(Fx::Nav(NavOp::Pop));
                fx.push(Fx::Nav(NavOp::Push(YtArg::Info)));
            }
            Act::Comments(v) => {
                fx.push(Fx::Nav(NavOp::Pop));
                fx.push(Fx::Nav(NavOp::Present(YtArg::Comments(v.clone()))));
            }
            Act::Filter(i) => {
                let i = *i;
                fx.push(Fx::App(YtFx::EditFilters(Box::new(move |f| {
                    use yt_core::search_filters::*;
                    fn step<T: Copy + PartialEq>(l: &[T], c: T, d: i32) -> T {
                        let i = l.iter().position(|x| *x == c).unwrap_or(0) as i32;
                        l[((i + d).rem_euclid(l.len() as i32)) as usize]
                    }
                    match i {
                        0 => f.upload_date = step(&[UploadDate::Any, UploadDate::Hour, UploadDate::Today, UploadDate::Week, UploadDate::Month, UploadDate::Year], f.upload_date, dir),
                        1 => f.kind = step(&[Kind::Any, Kind::Video, Kind::Channel, Kind::Playlist, Kind::Movie], f.kind, dir),
                        2 => f.duration = step(&[Duration::Any, Duration::Short, Duration::Medium, Duration::Long], f.duration, dir),
                        3 => f.sort = step(&[Sort::Relevance, Sort::UploadDate, Sort::ViewCount, Sort::Rating], f.sort, dir),
                        4 => f.live = !f.live,
                        5 => f.hd_4k = !f.hd_4k,
                        6 => f.hdr = !f.hdr,
                        _ => *f = Filters::default(),
                    }
                }))));
            }
            Act::ApplyFilters(q) => {
                let params = cx.views.store.search_filters.params();
                fx.push(Fx::Nav(NavOp::Pop));
                fx.push(Fx::Nav(NavOp::Replace(YtArg::Feed(FeedKey::Search { query: q.clone(), params }))));
            }
            Act::Close => fx.push(Fx::Nav(NavOp::Pop)),
        }
        fx.invalidate(plx_machine::present::Provenance::Input);
    }
}

impl Machine<YtHost> for MenuScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                ui::MENUS_OPEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            ScreenEvent::Unmount => {
                ui::MENUS_OPEN.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            }
            _ => {}
        }
        if let Some(a) = super::common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        if let (Some(v), ScreenEvent::Async(..)) = (&self.picker_for, ev) {
            // The row under focus, by label: it must stay under focus when rows are inserted
            // above it (an OK right then must not hit a different playlist).
            let focused_label = self.list.focused(cx, self.acts.len()).and_then(|i| self.acts.get(i)).map(|a| a.0.clone());
            let fresh = Self::playlists(self.list.entry, v.clone(), String::new(), cx);
            // keep the title used for "New playlist"
            let keep_title = self.acts.iter().find_map(|a| if let Act::NewPlaylist(t, _) = &a.1 { Some(t.clone()) } else { None });
            self.acts = fresh.acts;
            if let Some(l) = focused_label {
                self.refocus.set(self.acts.iter().position(|a| a.0 == l));
            }
            if let Some(t) = keep_title {
                for a in self.acts.iter_mut() {
                    if let Act::NewPlaylist(tt, _) = &mut a.1 {
                        *tt = t.clone();
                    }
                }
            }
            fx.invalidate(plx_machine::present::Provenance::Lifecycle);
        }
        let n = self.acts.len();
        match ev {
            ScreenEvent::Activate(e) if self.choice.as_ref().is_some_and(|c| c.owns(*e)) => {
                let c = self.choice.take().unwrap();
                self.closed_row.set(c.row);
                let k = c.list.index(*e, c.options.len()).unwrap_or(c.current) as i32;
                let act = self.acts[c.row].1.clone();
                if k != c.current as i32 {
                    self.run(&act, k - c.current as i32, cx, fx);
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Activate(e) => {
                if let Some(i) = self.list.index(*e, n) {
                    let act = self.acts[i].1.clone();
                    match self.choices(&act, cx) {
                        Some((v, cur)) => {
                            let anchor = self.list.place(*e, n, Some(i)).map(|p| p.rect).unwrap_or(self.list.rect);
                            self.choice = Some(ui::ChoiceBox::new(self.list.entry, i, v, cur, anchor));
                            fx.invalidate(plx_machine::present::Provenance::Input);
                        }
                        None => self.run(&act, 1, cx, fx),
                    }
                }
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Back, edge, .. }, .. }) if self.choice.is_some() => {
                if *edge == Edge::Down {
                    if let Some(c) = self.choice.take() {
                        self.closed_row.set(c.row);
                    }
                    fx.invalidate(plx_machine::present::Provenance::Input);
                }
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Left | Key::Right, .. }, .. }) if self.choice.is_some() => Handled::Yes,
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
        if let Some(c) = &self.choice {
            out.push(c.list.spec(c.options.len()));
            return;
        }
        out.push(self.list.spec(self.acts.len()));
    }
    fn group_of(&self, key: &u32, _cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        Some(if self.choice.as_ref().is_some_and(|c| c.owns(*key)) { ui::CHOICE_GROUP } else { G })
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, YtHost>) -> Step<u32> {
        if let Some(c) = &self.choice {
            return c.list.neighbour(key, dir, c.options.len());
        }
        self.list.neighbour(key, dir, self.acts.len())
    }
    fn place(&self, key: &u32, cx: &Cx<'_, YtHost>, _at: At) -> Option<Placed> {
        if let Some(c) = self.choice.as_ref().filter(|c| c.owns(*key)) {
            let n = c.options.len();
            return c.list.place(*key, n, c.list.focused(cx, n));
        }
        let n = self.acts.len();
        self.list.place(*key, n, self.list.focused(cx, n))
    }
    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        if let Some(c) = &self.choice {
            return if c.owns(want.elem) { want } else { c.list.key(c.current) };
        }
        if want.elem >= ui::CHOICE_BASE {
            return self.list.key(self.closed_row.get());
        }
        if let Some(i) = self.refocus.take().filter(|i| *i < self.acts.len()) {
            return self.list.key(i);
        }
        if self.list.index(want.elem, self.acts.len()).is_some() { want } else { self.list.key(0) }
    }
    fn seat(&self, g: GroupId, _from: Placed, _cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        if let (Some(c), true) = (&self.choice, g == ui::CHOICE_GROUP) {
            return c.list.key(c.current);
        }
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
        let _dim = ui::HdrDim::begin(ui::hdr_playing(cx.views.player));
        ui::fill(p, PANEL, 0.0, ui::PANEL);
        ui::text(p, &ui::ellipsize(&self.title, 30), PANEL.x + 52.0, 62.0, 30, ui::WHITE, ui::ALIGN_LEFT, true);
        if let Some(v) = &self.picker_for {
            if !cx.views.store.playlist_options.contains_key(v) {
                ui::text(p, "Loading your playlists\u{2026}", PANEL.x + 60.0, 150.0, 24, ui::FAINT, ui::ALIGN_LEFT, false);
            }
        }
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
        if let Some(c) = &self.choice {
            c.draw(f);
        } else {
            self.list.stops(f, rows.len());
        }
        super::common::toast(p, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

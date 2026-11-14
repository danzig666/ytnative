//! Modal menus: a card's actions (hold OK on a card) and the player options (Up in the player).
//! A panel on the right; OK runs an action, Left/Right change a player option in place.

use crate::host::{UserAction, YtArg, YtFx, YtHost};
use crate::i18n::tr;
use crate::settings::{PlaybackMode, SLEEP_MINS, SPEEDS};
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
    UnblockChannel(String),
    /// Play (video id) from the start and / or without watch history.
    PlayWith(String, bool, bool),
    /// Add to / remove from "Play next": (video id, title, add).
    PlayNext(String, String, bool),
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
    /// SponsorBlock on / off for a channel (by name).
    SbChannel(String),
    Close,
    /// Play at this speed (the speed list).
    SetSpeed(f32),
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

/// Card-menu item kinds, in the default order, with their settings labels.
pub const CARD_ITEMS: [(&str, &str); 15] = [
    ("play", "Card menu: Play"),
    ("play_start", "Card menu: Play from the start"),
    ("play_private", "Card menu: Play without history"),
    ("play_next", "Card menu: Play next"),
    ("open", "Card menu: Open channel / playlist"),
    ("groups", "Card menu: Channel groups"),
    ("pin", "Card menu: Pin to sidebar"),
    ("watch_later", "Card menu: Save to Watch later"),
    ("playlist", "Card menu: Save to playlist"),
    ("youtube", "Card menu: YouTube's items"),
    ("more_from", "Card menu: More from channel"),
    ("block", "Card menu: Block channel"),
    ("sponsorblock", "Card menu: SponsorBlock on this channel"),
    ("subscribe", "Card menu: Subscribe / unsubscribe"),
    ("comments", "Card menu: Comments"),
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
        let mut title = tr("Options").to_string();
        if let Some(card) = cx.views.store.card(elem) {
            title = card.title.clone();
            if let Some(v) = card.video_id() {
                kind = "play";
                tagged.push((kind, tr("Play").to_string(), Act::Play(v.to_string())));
                tagged.push(("play_start", tr("Play from the start").to_string(), Act::PlayWith(v.to_string(), true, false)));
                tagged.push(("play_private", tr("Play without history").to_string(), Act::PlayWith(v.to_string(), false, true)));
                if cx.views.store.play_next.iter().any(|(q, _)| q == v) {
                    tagged.push(("play_next", tr("Remove from Play next").to_string(), Act::PlayNext(v.to_string(), card.title.clone(), false)));
                } else {
                    tagged.push(("play_next", tr("Play next").to_string(), Act::PlayNext(v.to_string(), card.title.clone(), true)));
                }
                if !cx.views.store.settings.child_mode {
                    tagged.push(("comments", tr("Comments").to_string(), Act::Comments(v.to_string())));
                }
            }
            if let Some(Endpoint::Browse { browse_id, params }) = &card.item.endpoint {
                if browse_id.starts_with("UC") {
                    tagged.push(("open", tr("Open channel").to_string(), Act::OpenChannel(browse_id.clone())));
                } else {
                    tagged.push(("open", tr("Open").to_string(), Act::OpenPlaylist(browse_id.clone())));
                }
                if browse_id.starts_with("UC") && cx.views.store.signed_in {
                    // Subscribed: as YouTube said, or it is on the subscribed channels' pages.
                    let in_subs = [FeedKey::Section(crate::store::Section::Channels), FeedKey::Section(crate::store::Section::Subscriptions)]
                        .iter()
                        .any(|k| cx.views.store.cards(k).iter().any(|c| c.elem == card.elem));
                    let subscribed = cx.views.store.subscribed.get(browse_id).copied().unwrap_or(in_subs);
                    let label = tr(if subscribed { "Unsubscribe from {}" } else { "Subscribe to {}" }).replace("{}", &card.title);
                    tagged.push(("subscribe", label, Act::User(UserAction::Subscribe { channel_id: browse_id.clone(), subscribe: !subscribed })));
                }
                if browse_id.starts_with("UC") {
                    for (gid, gt) in &cx.views.store.groups {
                        tagged.push(("groups", tr("Add to group \u{201C}{}\u{201D}").replace("{}", gt), Act::AddToGroup(Some(gid.clone()), browse_id.clone(), card.title.clone(), card.item.thumbnail.clone())));
                    }
                    tagged.push(("groups", tr("Add to a new group").to_string(), Act::AddToGroup(None, browse_id.clone(), card.title.clone(), card.item.thumbnail.clone())));
                }
                if cx.views.store.settings.pinned.iter().any(|p| &p.1 == browse_id) {
                    tagged.push(("pin", tr("Unpin from sidebar").to_string(), Act::Unpin(browse_id.clone())));
                } else {
                    tagged.push(("pin", tr("Pin to sidebar").to_string(), Act::Pin(card.title.clone(), browse_id.clone(), params.clone())));
                }
            }
            if let Some(v) = card.video_id() {
                tagged.push(("watch_later", tr("Save to Watch later").to_string(), Act::User(UserAction::WatchLater(v.to_string()))));
                tagged.push(("playlist", tr("Save to playlist\u{2026}").to_string(), Act::PickPlaylist(v.to_string(), card.title.clone())));
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
                    tagged.push(("youtube", tr(&m.label).to_string(), Act::User(UserAction::Menu(m.clone()))));
                }
            }
            if let Some(ch) = card.item.channel.as_ref().filter(|_| !cx.views.store.settings.child_mode) {
                tagged.push(("more_from", tr("More from {}").replace("{}", ch), Act::SearchFor(ch.clone())));
                if cx.views.store.settings.blocked_channels.contains(ch) {
                    tagged.push(("block", tr("Unblock {}").replace("{}", ch), Act::UnblockChannel(ch.clone())));
                } else {
                    tagged.push(("block", tr("Block {}").replace("{}", ch), Act::BlockChannel(ch.clone())));
                }
                if cx.views.store.settings.sponsorblock && card.video_id().is_some() {
                    tagged.push(("sponsorblock", tr("SponsorBlock on this channel").to_string(), Act::SbChannel(ch.clone())));
                }
            }
        }
        let s = &cx.views.store.settings;
        let rank = |k: &str| s.card_menu.iter().position(|x| x == k).unwrap_or(usize::MAX);
        tagged.sort_by_key(|(k, _, _)| rank(k)); // stable: items of one kind keep their order
        let _ = kind;
        acts.extend(tagged.into_iter().filter(|(k, _, _)| !s.card_menu_hidden.iter().any(|h| h == k)).map(|(_, l, a)| (l, a)));
        acts.push((tr("Close").to_string(), Act::Close));
        Self::make(entry, title, acts)
    }

    /// "Save to playlist": one row per playlist (ticked when it already holds the video).
    pub fn playlists(entry: EntryId, video_id: String, title: String, cx: &Cx<'_, YtHost>) -> Self {
        let mut acts = Vec::new();
        for o in cx.views.store.playlist_options.get(&video_id).into_iter().flatten() {
            let mark = if o.contains { "\u{2713}  " } else { "     " };
            acts.push((format!("{mark}{}", o.title), Act::TogglePlaylist(o.playlist_id.clone(), video_id.clone(), !o.contains)));
        }
        acts.push((tr("+  New private playlist").to_string(), Act::NewPlaylist(title.clone(), video_id.clone())));
        acts.push((tr("Close").to_string(), Act::Close));
        let mut m = Self::make(entry, tr("Save to playlist").to_string(), acts);
        m.picker_for = Some(video_id);
        m
    }

    pub fn filters(entry: EntryId, query: String) -> Self {
        let acts = vec![
            (tr("Upload date").to_string(), Act::Filter(0)),
            (tr("Type").to_string(), Act::Filter(1)),
            (tr("Duration").to_string(), Act::Filter(2)),
            (tr("Sort by").to_string(), Act::Filter(3)),
            (tr("Live").to_string(), Act::Filter(4)),
            (tr("4K").to_string(), Act::Filter(5)),
            (tr("HDR").to_string(), Act::Filter(6)),
            (tr("Reset filters").to_string(), Act::Filter(7)),
            (tr("Apply").to_string(), Act::ApplyFilters(query)),
        ];
        Self::make(entry, tr("Search filters").to_string(), acts)
    }

    /// Every speed SmartTube offers, the current one selected.
    pub fn speeds(entry: EntryId, cx: &Cx<'_, YtHost>) -> Self {
        let cur = if cx.views.player.rate > 0.0 { cx.views.player.rate } else { 1.0 };
        let acts: Vec<(String, Act)> = SPEEDS
            .iter()
            .map(|&r| {
                let label = if (r - 1.0).abs() < 0.001 { tr("Normal").to_string() } else { format!("{r}x") };
                let label = if (r - cur).abs() < 0.001 { format!("{label}  \u{2713}") } else { label };
                (label, Act::SetSpeed(r))
            })
            .collect();
        let m = Self::make(entry, tr("Playback speed").to_string(), acts);
        m.refocus.set(Some(crate::settings::speed_index(cur)));
        m
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
                if is_like { tr("Remove like").to_string() } else { tr("Like").to_string() },
                Act::User(UserAction::Rate(st.video_id.clone(), if is_like { Rating::None } else { Rating::Like })),
            ));
            acts.push((
                if is_dislike { tr("Remove dislike").to_string() } else { tr("Dislike").to_string() },
                Act::User(UserAction::Rate(st.video_id.clone(), if is_dislike { Rating::None } else { Rating::Dislike })),
            ));
            if !st.channel_id.is_empty() {
                let subscribed = st.extras.watch.as_ref().and_then(|w| w.subscribed).unwrap_or(false);
                acts.push((
                    tr(if subscribed { "Unsubscribe from {}" } else { "Subscribe to {}" }).replace("{}", &st.author),
                    Act::User(UserAction::Subscribe { channel_id: st.channel_id.clone(), subscribe: !subscribed }),
                ));
            }
            acts.push((tr("Save to Watch later").to_string(), Act::User(UserAction::WatchLater(st.video_id.clone()))));
            acts.push((tr("Save to playlist\u{2026}").to_string(), Act::PickPlaylist(st.video_id.clone(), st.title.clone())));
            acts.push((tr("Video info & chapters").to_string(), Act::Info));
            if !cx.views.store.settings.child_mode {
                acts.push((tr("Comments").to_string(), Act::Comments(st.video_id.clone())));
            }
            if !st.channel_id.is_empty() {
                acts.push((tr("Open {}").replace("{}", &st.author), Act::OpenChannel(st.channel_id.clone())));
            }
        }
        acts.extend(vec![
            (tr("Playback speed").to_string(), Act::Speed),
            (tr("Audio language").to_string(), Act::AudioTrack),
            (tr("Video zoom").to_string(), Act::ZoomMode),
            (tr("Sleep timer").to_string(), Act::Sleep),
            (tr("Maximum quality").to_string(), Act::MaxQuality),
            (tr("Subtitles").to_string(), Act::Subtitles),
            (tr("Subtitle size").to_string(), Act::SubSize),
            (tr("Subtitle colour").to_string(), Act::SubColor),
            (tr("Subtitle style").to_string(), Act::SubStyle),
            (tr("Subtitle font").to_string(), Act::SubFont),
            (tr("Bold subtitles").to_string(), Act::SubBold),
            (tr("When the video ends").to_string(), Act::RepeatMode),
        ]);
        if cx.views.store.settings.sponsorblock && !st.author.is_empty() {
            acts.push((tr("SponsorBlock on this channel").to_string(), Act::SbChannel(st.author.clone())));
        }
        acts.extend(vec![
            (tr("Close").to_string(), Act::Close),
        ]);
        Self::make(entry, tr("Player options").to_string(), acts)
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
                    return Some(tr("only one").to_string());
                }
                let cur = st.audio_track.clone().or_else(|| st.audio_tracks.iter().find(|t| t.2).map(|t| t.0.clone()));
                let name = st.audio_tracks.iter().find(|t| Some(&t.0) == cur.as_ref()).map(|t| t.1.clone()).unwrap_or_else(|| tr("default").to_string());
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
            Act::Sleep => format!("{}  \u{203A}", crate::settings::sleep_label(s.sleep_timer_mins)),
            Act::Subtitles => format!("{}  \u{203A}", crate::i18n::tr(if s.subtitles { "On" } else { "Off" })),
            Act::SubSize => format!("{}  \u{203A}", s.subtitle_size),
            Act::SubColor => format!("{}  \u{203A}", crate::i18n::tr(&s.subtitle_color)),
            Act::SubBold => format!("{}  \u{203A}", crate::i18n::tr(if s.subtitle_bold { "On" } else { "Off" })),
            Act::SubFont => format!("{}  \u{203A}", crate::i18n::tr(&s.subtitle_font)),
            Act::SubStyle => format!("{}  \u{203A}", crate::i18n::tr(&s.subtitle_style)),
            Act::RepeatMode => format!("{}  \u{203A}", crate::i18n::tr(s.playback_mode.label())),
            Act::SbChannel(ch) => crate::i18n::tr(if s.sb_excluded_channels.contains(ch) { "Off" } else { "On" }).to_string(),
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
                (SPEEDS.iter().map(|v| format!("{v}x")).collect(), crate::settings::speed_index(r))
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
            Act::Sleep => (
                SLEEP_MINS.iter().map(|m| crate::settings::sleep_label(*m)).collect(),
                SLEEP_MINS.iter().position(|m| *m == s.sleep_timer_mins).unwrap_or(0),
            ),
            Act::SubSize => (SUB_SIZES.iter().map(|v| v.to_string()).collect(), SUB_SIZES.iter().position(|v| *v == s.subtitle_size).unwrap_or(2)),
            Act::SubColor => (SUB_COLORS.iter().map(|v| v.to_string()).collect(), SUB_COLORS.iter().position(|v| *v == s.subtitle_color).unwrap_or(0)),
            Act::SubFont => (SUB_FONTS.iter().map(|v| v.to_string()).collect(), SUB_FONTS.iter().position(|v| *v == s.subtitle_font).unwrap_or(0)),
            Act::SubStyle => (SUB_STYLES.iter().map(|v| v.to_string()).collect(), SUB_STYLES.iter().position(|v| *v == s.subtitle_style).unwrap_or(0)),
            Act::RepeatMode => (
                PlaybackMode::ALL.iter().map(|m| m.label().to_string()).collect(),
                PlaybackMode::ALL.iter().position(|x| *x == s.playback_mode).unwrap_or(0),
            ),
            _ => return None,
        })
    }

    fn run(&self, act: &Act, dir: i32, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        match act {
            Act::Play(v) => {
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
                fx.push(Fx::Nav(NavOp::Push(YtArg::Player { video_id: v.clone(), playlist: None })));
            }
            Act::PlayWith(v, from_start, no_history) => {
                fx.push(Fx::App(YtFx::PlayOptions { video_id: v.clone(), from_start: *from_start, no_history: *no_history }));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
                fx.push(Fx::Nav(NavOp::Push(YtArg::Player { video_id: v.clone(), playlist: None })));
            }
            Act::UnblockChannel(ch) => {
                let ch = ch.clone();
                fx.push(Fx::App(YtFx::Toast(crate::i18n::tr("{} unblocked").replace("{}", &ch))));
                fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| s.blocked_channels.retain(|c| *c != ch)))));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
            }
            Act::OpenChannel(id) | Act::OpenPlaylist(id) => {
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
                fx.push(Fx::Nav(NavOp::Push(YtArg::Feed(FeedKey::Browse { id: id.clone(), params: None }))));
            }
            Act::SearchFor(q) => {
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
                fx.push(Fx::Nav(NavOp::Push(YtArg::Feed(FeedKey::Search { query: q.clone(), params: None }))));
            }
            Act::AddToGroup(g, ch, t, icon) => {
                fx.push(Fx::App(YtFx::Act(UserAction::AddToGroup { group_id: g.clone(), channel_id: ch.clone(), title: t.clone(), icon: icon.clone() })));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
            }
            Act::Pin(t, id, p) => {
                let (t, id, p) = (t.clone(), id.clone(), p.clone());
                fx.push(Fx::App(YtFx::Toast(tr("Pinned {}").replace("{}", &t))));
                fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| s.pinned.push((t, id, p))))));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
            }
            Act::Unpin(id) => {
                let id = id.clone();
                fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| s.pinned.retain(|p| p.1 != id)))));
                fx.push(Fx::App(YtFx::Toast("Unpinned".into())));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
            }
            Act::BlockChannel(ch) => {
                let ch = ch.clone();
                fx.push(Fx::App(YtFx::Toast(tr("{} blocked (Settings \u{203A} Content filters)").replace("{}", &ch))));
                fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| {
                    if !s.blocked_channels.contains(&ch) {
                        s.blocked_channels.push(ch);
                    }
                }))));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
            }
            Act::Speed => {
                let cur = if cx.views.player.rate > 0.0 { cx.views.player.rate } else { 1.0 };
                let i = crate::settings::speed_index(cur) as i32;
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
            Act::Sleep => fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| {
                let i = SLEEP_MINS.iter().position(|m| *m == s.sleep_timer_mins).unwrap_or(0) as i32;
                s.sleep_timer_mins = SLEEP_MINS[((i + dir).rem_euclid(SLEEP_MINS.len() as i32)) as usize];
            })))),
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
                let all = PlaybackMode::ALL;
                let i = all.iter().position(|r| *r == s.playback_mode).unwrap_or(0) as i32;
                s.playback_mode = all[((i + dir).rem_euclid(all.len() as i32)) as usize];
            })))),
            Act::SbChannel(ch) => {
                let ch = ch.clone();
                fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| {
                    if let Some(i) = s.sb_excluded_channels.iter().position(|c| *c == ch) {
                        s.sb_excluded_channels.remove(i);
                    } else {
                        s.sb_excluded_channels.push(ch);
                    }
                }))));
            }
            Act::User(a) => {
                fx.push(Fx::App(YtFx::Act(a.clone())));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
            }
            Act::PickPlaylist(v, t) => {
                fx.push(Fx::App(YtFx::LoadPlaylists(v.clone())));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
                fx.push(Fx::Nav(NavOp::Present(YtArg::PlaylistPicker(v.clone(), t.clone()))));
            }
            Act::TogglePlaylist(pl, v, add) => {
                fx.push(Fx::App(YtFx::Act(UserAction::Playlist { playlist_id: pl.clone(), video_id: v.clone(), add: *add })));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
            }
            Act::NewPlaylist(t, v) => {
                fx.push(Fx::App(YtFx::Act(UserAction::NewPlaylist { title: t.clone(), video_id: v.clone() })));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
            }
            Act::Info => {
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
                fx.push(Fx::Nav(NavOp::Push(YtArg::Info)));
            }
            Act::PlayNext(v, title, add) => {
                fx.push(Fx::App(YtFx::PlayNext { video_id: v.clone(), title: title.clone(), add: *add }));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
            }
            Act::Comments(v) => {
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
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
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
                fx.push(Fx::Nav(NavOp::Replace(YtArg::Feed(FeedKey::Search { query: q.clone(), params }))));
            }
            Act::Close => fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry))),
            Act::SetSpeed(r) => {
                fx.push(Fx::App(YtFx::SetSpeed(*r)));
                let label = if (*r - 1.0).abs() < 0.001 { crate::i18n::tr("Normal").to_string() } else { format!("{r}x") };
                fx.push(Fx::App(YtFx::Toast(label)));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
            }
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
                            self.choice = Some(ui::ChoiceBox::new(self.list.entry, i, v, cur, anchor).raw(matches!(act, Act::AudioTrack)));
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
        ui::text_raw(p, &f.measure.fit_line(&self.title, PANEL.w - 104.0, 30, true).to_string_lossy(), PANEL.x + 52.0, 62.0, 30, ui::WHITE, ui::ALIGN_LEFT, true);
        if let Some(v) = &self.picker_for {
            if !cx.views.store.playlist_options.contains_key(v) {
                ui::text(p, "Loading your playlists\u{2026}", PANEL.x + 60.0, 150.0, 24, ui::FAINT, ui::ALIGN_LEFT, false);
            }
        }
        let rows: Vec<Row> = self
            .acts
            .iter()
            .map(|(label, act)| {
                let r = Row::new(label.clone()).raw();
                match self.value(act, cx) {
                    Some(v) => r.value(v),
                    None => r,
                }
            })
            .collect();
        self.list.draw(p, f.measure, &rows, self.list.focused(cx, rows.len()));
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

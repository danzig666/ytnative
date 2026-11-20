//! Modal menus: a card's actions (hold OK on a card) and the player options (Up in the player).
//! A panel on the right; OK runs an action, Left/Right change a player option in place.

use crate::host::{LocalOp, NamePurpose, UserAction, YtArg, YtFx, YtHost};
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
use yt_core::local_playlists::{Move, Video};
use yt_core::model::Endpoint;

const G: GroupId = GroupId(1);
const BASE: u32 = 0x5000_0000;
const PANEL: Rect = Rect::new(1300.0, 0.0, 620.0, 1080.0);

/// The bell's choices in YouTube's order, with the names they are shown by.
const BELL_CHOICES: [(&str, &str); 3] = [
    ("SUBSCRIPTION_NOTIFICATION_STATE_ALL", "All videos"),
    ("SUBSCRIPTION_NOTIFICATION_STATE_OCCASIONAL", "Personalized"),
    ("SUBSCRIPTION_NOTIFICATION_STATE_OFF", "None"),
];

#[derive(Clone, Debug)]
enum Act {
    Play(String),
    OpenChannel(String),
    OpenPlaylist(String),
    BlockChannel(String),
    UnblockChannel(String),
    /// Play (video id) from the start and / or without watch history.
    PlayWith(String, bool, bool),
    /// Add to / remove from "Play next": (video, add).
    PlayNext(Video, bool),
    Pin(String, String, Option<String>),
    AddToGroup(Option<String>, String, String, Option<String>),
    Unpin(String),
    SearchFor(String),
    User(UserAction),
    Comments(String),
    /// The live chat of the live stream playing.
    LiveChat,
    Info,
    Filter(u8),
    PickPlaylist(Video),
    TogglePlaylist(String, String, bool),
    NewPlaylist(String, String),
    /// A row that only labels the rows under it ("On this TV").
    Header,
    /// A row that only tells something ("Loading your playlists").
    Note,
    /// Add a video to a playlist on this TV (true) or take it out: (list id, video, add).
    ToggleLocal(String, Video, bool),
    /// A new playlist on this TV holding this video (asks for its name).
    NewLocal(Video),
    /// In a playlist on this TV: take a video out, or move it: (list id, video id).
    LocalRemove(String, String),
    LocalMove(String, String, Move),
    /// Play a playlist on this TV from its first video, or shuffled: (list id, shuffle).
    PlayList(String, bool),
    /// (list id, current title).
    RenameLocal(String, String),
    /// (list id, title): a second OK deletes.
    DeleteLocal(String, String),
    /// A YouTube playlist (browse id, title) becomes a playlist on this TV.
    CopyToTv(String, String),
    /// A playlist on this TV (list id) becomes a private YouTube playlist.
    CopyToYouTube(String),
    /// "Play next" becomes a playlist on this TV (asks for its name).
    SaveQueue,
    ApplyFilters(String),
    Speed,
    AudioTrack,
    ZoomMode,
    Sleep,
    /// The playing video's quality (for this video only; it restarts at the same position).
    Quality,
    MaxQuality,
    Subtitles,
    SubSize,
    SubColor,
    SubStyle,
    SubFont,
    SubBold,
    RepeatMode,
    /// Sound alone over a black screen (it restarts the video at the same position).
    AudioOnly,
    /// The "Video details" panel on / off.
    VideoDetails,
    /// "Notify me of new videos from this channel" on / off for the playing video's channel.
    Bell,
    /// SponsorBlock on / off for a channel (by name).
    SbChannel(String),
    Close,
    /// Play at this speed (the speed list).
    SetSpeed(f32),
    /// The bell menu: this notification choice (`SUBSCRIPTION_NOTIFICATION_STATE_*`) and the
    /// name it is shown by.
    SetBell(String, &'static str),
    /// The bell menu: leave the channel.
    Unsubscribe,
    /// Install the release the automatic check found (closes the app).
    InstallUpdate,
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
    picker_for: Option<Video>,
    title: String,
    acts: Vec<(String, Act)>,
    list: VList,
    /// An option's drop-down list, while open.
    choice: Option<ui::ChoiceBox>,
    closed_row: std::cell::Cell<usize>,
    /// Row to put focus on at the next reconcile (the playlist picker after its list changed).
    refocus: std::cell::Cell<Option<usize>>,
    /// The row of a "Delete" the first OK was pressed on (the second one deletes).
    armed: Option<usize>,
    /// Opened over the player (a suggestion's menu): playing replaces the video that plays.
    over_player: bool,
    state: State,
}

/// The playing video's quality choices: "Automatic" (the setting's maximum), then each quality
/// it offers, highest first; and the current one.
fn video_quality_choices(st: &crate::player::Status) -> (Vec<String>, usize) {
    let mut v = vec![tr("Automatic").to_string()];
    v.extend(st.qualities.iter().map(|(h, fps)| quality_label(*h, *fps)));
    let cur = st.qualities.iter().position(|q| q.0 == st.quality_pick).map_or(0, |i| i + 1);
    (v, cur)
}

/// "1080p", or "1080p60" above 30 frames a second.
fn quality_label(h: u32, fps: u32) -> String {
    if fps > 30 {
        format!("{h}p{fps}")
    } else {
        format!("{h}p")
    }
}

/// The maximum video heights the quality row offers.
const QUALITIES: &[u32] = &[480, 720, 1080, 1440, 2160];

/// The running sleep timer's length in [`SLEEP_MINS`] (0: off).
fn sleep_index(cx: &Cx<'_, YtHost>) -> usize {
    cx.views.store.sleep.and_then(|t| SLEEP_MINS.iter().position(|m| *m == t.mins)).unwrap_or(0)
}

/// The values of a row that edits a setting, and the current one (`None` for other rows).
fn setting_choices(act: &Act, s: &crate::settings::Settings) -> Option<(Vec<String>, usize)> {
    Some(match act {
        Act::MaxQuality => (QUALITIES.iter().map(|q| format!("{q}p")).collect(), QUALITIES.iter().position(|q| *q == s.max_height).unwrap_or(2)),
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

/// A row that edits a setting, stepped `dir` values (LEFT / RIGHT, or a pick from its list).
fn step_setting(act: &Act, s: &mut crate::settings::Settings, dir: i32) {
    fn wrap(i: usize, dir: i32, n: usize) -> usize {
        (i as i32 + dir).rem_euclid(n as i32) as usize
    }
    fn clamp(i: usize, dir: i32, n: usize) -> usize {
        (i as i32 + dir).clamp(0, n as i32 - 1) as usize
    }
    match act {
        Act::MaxQuality => {
            let i = QUALITIES.iter().position(|q| *q == s.max_height).unwrap_or(2);
            s.max_height = QUALITIES[clamp(i, dir, QUALITIES.len())];
            s.min_height = s.min_height.min(s.max_height);
        }
        Act::Subtitles => s.subtitles = !s.subtitles,
        Act::SubBold => s.subtitle_bold = !s.subtitle_bold,
        Act::VideoDetails => s.video_details = !s.video_details,
        Act::SubSize => {
            let i = SUB_SIZES.iter().position(|v| *v == s.subtitle_size).unwrap_or(2);
            s.subtitle_size = SUB_SIZES[clamp(i, dir, SUB_SIZES.len())];
        }
        Act::SubColor => {
            let i = SUB_COLORS.iter().position(|v| *v == s.subtitle_color).unwrap_or(0);
            s.subtitle_color = SUB_COLORS[wrap(i, dir, SUB_COLORS.len())].into();
        }
        Act::SubFont => {
            let i = SUB_FONTS.iter().position(|v| *v == s.subtitle_font).unwrap_or(0);
            s.subtitle_font = SUB_FONTS[wrap(i, dir, SUB_FONTS.len())].into();
        }
        Act::SubStyle => {
            let i = SUB_STYLES.iter().position(|v| *v == s.subtitle_style).unwrap_or(0);
            s.subtitle_style = SUB_STYLES[wrap(i, dir, SUB_STYLES.len())].into();
        }
        Act::RepeatMode => {
            let all = PlaybackMode::ALL;
            let i = all.iter().position(|r| *r == s.playback_mode).unwrap_or(0);
            s.playback_mode = all[wrap(i, dir, all.len())];
        }
        _ => {}
    }
}

/// The speed `dir` steps from `current` in the speed list (stops at its ends).
/// Whether a held Left / Right keeps stepping the row. A switch does not: held, it would flip on
/// and off with every repeat, and "Audio only" restarts the stream at each flip.
fn repeats(act: &Act) -> bool {
    !matches!(act, Act::AudioOnly | Act::VideoDetails | Act::Bell)
}

fn step_speed(current: f32, dir: i32) -> f32 {
    let i = crate::settings::speed_index(current) as i32;
    SPEEDS[(i + dir).clamp(0, SPEEDS.len() as i32 - 1) as usize]
}

const SUB_SIZES: &[u32] = &[32, 38, 44, 52, 60, 72];
const SUB_COLORS: &[&str] = &["white", "light gray", "gray", "dark gray", "yellow", "light yellow", "cyan", "green", "orange", "pink"];
const SUB_STYLES: &[&str] = &["outline", "shadow", "box"];
const SUB_FONTS: &[&str] = &["default", "sans", "serif", "mono", "roboto"];

/// Card-menu item kinds, in the default order, with their settings labels.
pub const CARD_ITEMS: [(&str, &str); 19] = [
    ("play", "Card menu: Play"),
    ("play_start", "Card menu: Play from the start"),
    ("play_private", "Card menu: Play without history"),
    ("play_next", "Card menu: Play next"),
    ("play_all", "Card menu: Play all and Shuffle"),
    ("tv_order", "Card menu: Remove and move videos in a playlist on this TV"),
    ("open", "Card menu: Open channel / playlist"),
    ("tv_manage", "Card menu: Rename and delete a playlist on this TV"),
    ("groups", "Card menu: Channel groups"),
    ("pin", "Card menu: Pin to sidebar"),
    ("watch_later", "Card menu: Save to Watch later"),
    ("playlist", "Card menu: Save to playlist"),
    ("copy_list", "Card menu: Copy a playlist"),
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
            armed: None,
            over_player: false,
            state: State(title.clone()),
            title,
            acts,
            list: VList { entry, group: G, base: BASE, rect: Rect::new(PANEL.x + 24.0, 130.0, PANEL.w - 48.0, 920.0), row_h: 54.0, text_size: 26 },
        }
    }

    pub fn card(entry: EntryId, elem: u32, cx: &Cx<'_, YtHost>) -> Self {
        Self::of_card(entry, cx.views.store.card(elem), cx)
    }

    /// The card menu for one of the playing video's suggestions (hold OK on it in the player).
    pub fn suggestion(entry: EntryId, video_id: &str, cx: &Cx<'_, YtHost>) -> Self {
        let item = cx.views.player.extras.watch.as_ref().and_then(|w| {
            w.suggestions.iter().flat_map(|s| s.items.iter()).find(|it| it.id == video_id).cloned()
        });
        let card = item.map(|it| crate::store::Card {
            elem: 0, // no feed's card: not in a playlist on this TV, not a subscribed channel
            title: it.title.clone(),
            detail: it.lines.join(" \u{2022} "),
            thumb: String::new(),
            item: it,
        });
        let mut m = Self::of_card(entry, card.as_ref(), cx);
        m.over_player = true;
        m
    }

    fn of_card(entry: EntryId, card: Option<&crate::store::Card>, cx: &Cx<'_, YtHost>) -> Self {
        let elem = card.map_or(0, |c| c.elem);
        // (kind, label, action): kinds order and hide the items per the settings.
        let mut tagged: Vec<(&'static str, String, Act)> = Vec::new();
        let mut acts = Vec::new();
        let mut kind = "play";
        let mut title = tr("Options").to_string();
        if let Some(card) = card {
            title = card.title.clone();
            if let Some(v) = card.video_id() {
                kind = "play";
                tagged.push((kind, tr("Play").to_string(), Act::Play(v.to_string())));
                tagged.push(("play_start", tr("Play from the start").to_string(), Act::PlayWith(v.to_string(), true, false)));
                tagged.push(("play_private", tr("Play without history").to_string(), Act::PlayWith(v.to_string(), false, true)));
                let video = card.local_video().unwrap_or_else(|| Video::new(v));
                if cx.views.store.play_next.iter().any(|q| q.video_id == v) {
                    tagged.push(("play_next", tr("Remove from Play next").to_string(), Act::PlayNext(video, false)));
                } else {
                    tagged.push(("play_next", tr("Play next").to_string(), Act::PlayNext(video, true)));
                }
                // A video of a playlist on this TV: take it out, or move it in the list.
                let store = cx.views.store;
                let in_list = store.feeds.iter().find_map(|(k, f)| match k {
                    FeedKey::Local(id) if f.cards.iter().any(|c| c.elem == elem) => Some(id.clone()),
                    _ => None,
                });
                if let Some(list) = in_list.as_ref().and_then(|id| store.local.get(id)) {
                    if let Some(i) = list.items.iter().position(|x| x.video_id == v) {
                        let (id, vid) = (list.id.clone(), v.to_string());
                        tagged.push(("tv_order", tr("Remove from this playlist").to_string(), Act::LocalRemove(id.clone(), vid.clone())));
                        if i > 0 {
                            tagged.push(("tv_order", tr("Move to the top").to_string(), Act::LocalMove(id.clone(), vid.clone(), Move::Top)));
                            tagged.push(("tv_order", tr("Move up").to_string(), Act::LocalMove(id.clone(), vid.clone(), Move::Up)));
                        }
                        if i + 1 < list.items.len() {
                            tagged.push(("tv_order", tr("Move down").to_string(), Act::LocalMove(id, vid, Move::Down)));
                        }
                    }
                }
                if !cx.views.store.settings.child_mode {
                    tagged.push(("comments", tr("Comments").to_string(), Act::Comments(v.to_string())));
                }
            }
            // A playlist on this TV (a card of the Library's "On this TV" row).
            if let Some(list) = card.local_list().and_then(|id| cx.views.store.local.get(id)) {
                if !list.items.is_empty() {
                    tagged.push(("play_all", tr("Play all").to_string(), Act::PlayList(list.id.clone(), false)));
                    tagged.push(("play_all", tr("Shuffle").to_string(), Act::PlayList(list.id.clone(), true)));
                }
                tagged.push(("tv_manage", tr("Rename").to_string(), Act::RenameLocal(list.id.clone(), list.title.clone())));
                tagged.push(("tv_manage", tr("Delete").to_string(), Act::DeleteLocal(list.id.clone(), list.title.clone())));
                if cx.views.store.signed_in && !list.items.is_empty() {
                    tagged.push(("copy_list", tr("Copy to YouTube").to_string(), Act::CopyToYouTube(list.id.clone())));
                }
            } else if let Some(browse_id) = youtube_playlist(card) {
                tagged.push(("copy_list", tr("Copy to this TV").to_string(), Act::CopyToTv(browse_id, card.title.clone())));
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
                tagged.push(("playlist", tr("Save to playlist\u{2026}").to_string(), Act::PickPlaylist(card.local_video().unwrap_or_else(|| Video::new(v)))));
            }
            // The card's own YouTube menu (Not interested, Don't recommend channel, Remove from
            // history, ...). Lazy "More" panels and plain navigation entries are left out.
            for m in &card.item.menu {
                use yt_core::actions::{MenuCommand, MenuKind};
                let useful = matches!(
                    m.kind,
                    MenuKind::NotInterested
                        | MenuKind::DontRecommendChannel
                        | MenuKind::RemoveFromHistory
                        | MenuKind::RemoveFromWatchLater
                        | MenuKind::RemoveFromPlaylist
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

    /// Opens the player on `v`: over the player it takes the playing video's place.
    fn play_op(&self, v: &str) -> NavOp<YtArg> {
        let arg = YtArg::Player { video_id: v.to_string(), playlist: None };
        if self.over_player { NavOp::Replace(arg) } else { NavOp::Push(arg) }
    }

    /// "Save to playlist": one row per playlist (ticked when it already holds the video): the
    /// YouTube playlists first (signed in), then the playlists on this TV.
    pub fn playlists(entry: EntryId, video: Video, cx: &Cx<'_, YtHost>) -> Self {
        let store = cx.views.store;
        let mut acts = Vec::new();
        if store.signed_in {
            match store.playlist_options.get(&video.video_id) {
                None => acts.push((tr("Loading your playlists\u{2026}").to_string(), Act::Note)),
                Some(opts) => {
                    for o in opts {
                        let mark = if o.contains { "\u{2713}  " } else { "     " };
                        acts.push((format!("{mark}{}", o.title), Act::TogglePlaylist(o.playlist_id.clone(), video.video_id.clone(), !o.contains)));
                    }
                }
            }
            acts.push((tr("+  New private playlist").to_string(), Act::NewPlaylist(video.title.clone(), video.video_id.clone())));
        }
        acts.push((tr("On this TV").to_string(), Act::Header));
        for l in &store.local.lists {
            let has = l.contains(&video.video_id);
            let mark = if has { "\u{2713}  " } else { "     " };
            acts.push((format!("{mark}{}", l.title), Act::ToggleLocal(l.id.clone(), video.clone(), !has)));
        }
        acts.push((tr("+  New playlist on this TV").to_string(), Act::NewLocal(video.clone())));
        acts.push((tr("Close").to_string(), Act::Close));
        let mut m = Self::make(entry, tr("Save to playlist").to_string(), acts);
        // Focus starts on the first row that does something.
        m.refocus.set(m.acts.iter().position(|a| !matches!(a.1, Act::Header | Act::Note)));
        m.picker_for = Some(video);
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

    /// The automatic check found a newer version: install it now, or later from Settings.
    pub fn update(entry: EntryId) -> Self {
        let version = crate::update::found_version().unwrap_or_default();
        let acts = vec![
            (tr("Version {} is available").replace("{}", &version), Act::Note),
            (tr("Install now (closes the app)").to_string(), Act::InstallUpdate),
            (tr("Later").to_string(), Act::Close),
        ];
        let m = Self::make(entry, tr("Update available").to_string(), acts);
        m.refocus.set(Some(1));
        m
    }

    /// The subscribe button of a subscribed channel, as YouTube's: who to be notified of (all
    /// videos, personalized, none; the current choice ticked) and Unsubscribe.
    pub fn bell(entry: EntryId, cx: &Cx<'_, YtHost>) -> Self {
        let st = cx.views.player;
        let w = st.extras.watch.as_ref();
        let options = w.map(|w| w.notifications.as_slice()).unwrap_or_default();
        let current = cx.views.store.bell.get(&st.channel_id).cloned().or_else(|| options.iter().find(|o| o.selected).map(|o| o.state.clone()));
        let mut acts = Vec::new();
        let mut focus = 0;
        for (state, label) in BELL_CHOICES {
            if !options.iter().any(|o| o.state == state) {
                continue;
            }
            if current.as_deref() == Some(state) {
                focus = acts.len();
            }
            let shown = if current.as_deref() == Some(state) { format!("{}  \u{2713}", tr(label)) } else { tr(label).to_string() };
            acts.push((shown, Act::SetBell(state.to_string(), label)));
        }
        acts.push((tr("Unsubscribe").to_string(), Act::Unsubscribe));
        let name = w.map(|w| w.channel_name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| st.author.clone());
        let m = Self::make(entry, name, acts);
        m.refocus.set(Some(focus));
        m
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
            let liked = crate::screens::player::rating(cx);
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
            acts.push((tr("Save to playlist\u{2026}").to_string(), Act::PickPlaylist(super::player::playing_video(st))));
            if !cx.views.store.play_next.is_empty() {
                acts.push((tr("Save the queue as a playlist on this TV").to_string(), Act::SaveQueue));
            }
            acts.push((tr("Video info & chapters").to_string(), Act::Info));
            if !cx.views.store.settings.child_mode {
                acts.push((tr("Comments").to_string(), Act::Comments(st.video_id.clone())));
            }
            if super::live_chat::available(cx).is_some() {
                acts.push((tr("Live chat").to_string(), Act::LiveChat));
            }
            if !st.channel_id.is_empty() {
                acts.push((tr("Open {}").replace("{}", &st.author), Act::OpenChannel(st.channel_id.clone())));
            }
        }
        acts.extend(vec![
            (tr("Playback speed").to_string(), Act::Speed),
            (tr("Audio language").to_string(), Act::AudioTrack),
            (tr("Audio only").to_string(), Act::AudioOnly),
            (tr("Video zoom").to_string(), Act::ZoomMode),
            (tr("Sleep timer").to_string(), Act::Sleep),
            (tr("Quality").to_string(), Act::Quality),
            (tr("Maximum quality").to_string(), Act::MaxQuality),
            (tr("Subtitles").to_string(), Act::Subtitles),
            (tr("Subtitle size").to_string(), Act::SubSize),
            (tr("Subtitle colour").to_string(), Act::SubColor),
            (tr("Subtitle style").to_string(), Act::SubStyle),
            (tr("Subtitle font").to_string(), Act::SubFont),
            (tr("Bold subtitles").to_string(), Act::SubBold),
            (tr("When the video ends").to_string(), Act::RepeatMode),
            (tr("Video details").to_string(), Act::VideoDetails),
        ]);
        if cx.views.store.signed_in && !st.video_id.is_empty() && !st.channel_id.is_empty() {
            acts.push((tr("Notify me of new videos from this channel").to_string(), Act::Bell));
        }
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
                        // YouTube no longer sorts by rating: an old saved choice is relevance.
                        Sort::Relevance | Sort::Rating => "Relevance",
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
            Act::Quality => {
                let st = cx.views.player;
                if st.qualities.len() < 2 {
                    return Some(tr("only one").to_string());
                }
                let (v, cur) = video_quality_choices(st);
                format!("{}  \u{203A}", v[cur])
            }
            Act::AudioTrack => {
                let st = cx.views.player;
                if st.audio_tracks.len() < 2 {
                    return Some(tr("only one").to_string());
                }
                let cur = st.audio_track.clone().or_else(|| st.audio_tracks.iter().find(|t| t.2).map(|t| t.0.clone()));
                let name = st.audio_tracks.iter().find(|t| Some(&t.0) == cur.as_ref()).map(|t| t.1.clone()).unwrap_or_else(|| tr("default").to_string());
                format!("{name}  \u{203A}")
            }
            Act::ZoomMode => format!(
                "\u{2039}  {}  \u{203A}",
                crate::i18n::tr(match cx.views.player.zoom {
                    crate::player::Zoom::Fit => "Fit to screen",
                    crate::player::Zoom::Fill => "Fill the screen",
                    crate::player::Zoom::Zoom => "Zoom in",
                })
            ),
            Act::Sleep => match cx.views.store.sleep {
                Some(t) => format!("{}  \u{203A}", t.left_label(cx.views.now_ms)),
                None => format!("{}  \u{203A}", crate::i18n::tr("Off")),
            },
            Act::Subtitles => format!("{}  \u{203A}", crate::i18n::tr(if s.subtitles { "On" } else { "Off" })),
            Act::AudioOnly => format!("{}  \u{203A}", crate::i18n::tr(if cx.views.player.audio_only { "On" } else { "Off" })),
            Act::SubSize => format!("{}  \u{203A}", s.subtitle_size),
            Act::SubColor => format!("{}  \u{203A}", crate::i18n::tr(&s.subtitle_color)),
            Act::SubBold => format!("{}  \u{203A}", crate::i18n::tr(if s.subtitle_bold { "On" } else { "Off" })),
            Act::SubFont => format!("{}  \u{203A}", crate::i18n::tr(&s.subtitle_font)),
            Act::SubStyle => format!("{}  \u{203A}", crate::i18n::tr(&s.subtitle_style)),
            Act::RepeatMode => format!("{}  \u{203A}", crate::i18n::tr(s.playback_mode.label())),
            Act::VideoDetails => format!("{}  \u{203A}", crate::i18n::tr(if s.video_details { "On" } else { "Off" })),
            Act::Bell => {
                let on = yt_core::bell_channels::contains(&cx.views.player.channel_id);
                format!("{}  \u{203A}", crate::i18n::tr(if on { "On" } else { "Off" }))
            }
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
            Act::Quality => {
                let st = cx.views.player;
                if st.qualities.len() < 2 {
                    return None;
                }
                video_quality_choices(st)
            }
            Act::AudioTrack => {
                let st = cx.views.player;
                if st.audio_tracks.len() < 2 {
                    return None;
                }
                let cur = st.audio_track.clone().or_else(|| st.audio_tracks.iter().find(|t| t.2).map(|t| t.0.clone()));
                (st.audio_tracks.iter().map(|t| t.1.clone()).collect(), st.audio_tracks.iter().position(|t| Some(&t.0) == cur.as_ref()).unwrap_or(0))
            }
            // The timer's lengths; the one running is shown as its length.
            Act::Sleep => (SLEEP_MINS.iter().map(|m| crate::settings::sleep_label(*m)).collect(), sleep_index(cx)),
            _ => return setting_choices(act, s),
        })
    }

    fn run(&self, act: &Act, dir: i32, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        match act {
            Act::Play(v) => {
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
                fx.push(Fx::Nav(self.play_op(v)));
            }
            Act::PlayWith(v, from_start, no_history) => {
                fx.push(Fx::App(YtFx::PlayOptions { video_id: v.clone(), from_start: *from_start, no_history: *no_history }));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
                fx.push(Fx::Nav(self.play_op(v)));
            }
            Act::UnblockChannel(ch) => {
                let ch = ch.clone();
                fx.push(Fx::App(YtFx::Toast(crate::i18n::tr("{} unblocked").replace("{}", &ch))));
                fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| s.blocked_channels.retain(|c| *c != ch)))));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
            }
            Act::OpenChannel(id) | Act::OpenPlaylist(id) => {
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
                fx.push(Fx::Nav(NavOp::Push(YtArg::Feed(FeedKey::browse(id, None)))));
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
                fx.push(Fx::App(YtFx::SetSpeed(step_speed(cx.views.player.rate, dir))));
            }
            Act::Quality => {
                let st = cx.views.player;
                if st.qualities.len() >= 2 {
                    let (v, cur) = video_quality_choices(st);
                    let next = (cur as i32 + dir).clamp(0, v.len() as i32 - 1) as usize;
                    if next != cur {
                        fx.push(Fx::App(YtFx::SetQuality(if next == 0 { 0 } else { st.qualities[next - 1].0 })));
                    }
                }
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
            Act::AudioOnly => fx.push(Fx::App(YtFx::SetAudioOnly(!cx.views.player.audio_only))),
            Act::Bell => {
                let st = cx.views.player;
                let on = !yt_core::bell_channels::contains(&st.channel_id);
                if on && !super::player::subscribed_now(cx) {
                    // YouTube notifies only of subscribed channels, and the next video of this
                    // channel would take it off the list again.
                    fx.push(Fx::App(YtFx::Toast(tr("Subscribe to {} first").replace("{}", &st.author))));
                } else {
                    let options = st.extras.watch.as_ref().map(|w| w.notifications.clone()).unwrap_or_default();
                    let name = st.extras.watch.as_ref().map(|w| w.channel_name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| st.author.clone());
                    fx.push(Fx::App(YtFx::Act(UserAction::Bell { channel_id: st.channel_id.clone(), name, on, options })));
                }
            }
            Act::ZoomMode => {
                use crate::player::Zoom;
                const Z: &[Zoom] = &[Zoom::Fit, Zoom::Fill, Zoom::Zoom];
                let i = Z.iter().position(|z| *z == cx.views.player.zoom).unwrap_or(0) as i32;
                fx.push(Fx::App(YtFx::SetZoom(Z[((i + dir).rem_euclid(3)) as usize])));
            }
            Act::MaxQuality => {
                let act = act.clone();
                fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| step_setting(&act, s, dir)))));
                fx.push(Fx::App(YtFx::Toast("Applies to the next video".into())));
            }
            Act::Sleep => {
                let n = SLEEP_MINS.len() as i32;
                let mins = SLEEP_MINS[(sleep_index(cx) as i32 + dir).rem_euclid(n) as usize];
                fx.push(Fx::App(YtFx::SetSleep(mins)));
                fx.push(Fx::App(YtFx::Toast(if mins == 0 { tr("Sleep timer off").to_string() } else { tr("Sleep timer: {}").replace("{}", &crate::settings::sleep_label(mins)) })));
            }
            Act::Subtitles | Act::SubSize | Act::SubColor | Act::SubBold | Act::SubFont | Act::SubStyle | Act::RepeatMode | Act::VideoDetails => {
                let act = act.clone();
                fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| step_setting(&act, s, dir)))));
            }
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
            Act::PickPlaylist(v) => {
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
                fx.push(Fx::Nav(NavOp::Present(YtArg::PlaylistPicker(v.clone()))));
            }
            Act::Header | Act::Note => {}
            Act::ToggleLocal(list_id, video, add) => {
                fx.push(Fx::App(YtFx::Local(LocalOp::Toggle { list_id: list_id.clone(), video: video.clone(), add: *add })));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
            }
            Act::NewLocal(video) => {
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
                fx.push(Fx::Nav(NavOp::Push(YtArg::NameEntry(NamePurpose::NewLocal(vec![video.clone()])))));
            }
            Act::SaveQueue => {
                if cx.views.player.phase == crate::player::Phase::Playing {
                    fx.push(Fx::App(YtFx::TogglePause));
                }
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
                fx.push(Fx::Nav(NavOp::Push(YtArg::NameEntry(NamePurpose::NewLocal(cx.views.store.play_next.clone())))));
            }
            Act::LocalRemove(list_id, video_id) => {
                fx.push(Fx::App(YtFx::Local(LocalOp::Remove { list_id: list_id.clone(), video_id: video_id.clone() })));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
            }
            Act::LocalMove(list_id, video_id, to) => {
                fx.push(Fx::App(YtFx::Local(LocalOp::Move { list_id: list_id.clone(), video_id: video_id.clone(), to: *to })));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
            }
            Act::PlayList(list_id, shuffle) => {
                let list = cx.views.store.local.get(list_id);
                let mut q = crate::play_queue::PlayQueue::playlist(None, list.map(|l| l.title.clone()).unwrap_or_default(), Vec::new());
                q.add_items(list.map(|l| l.items.iter().map(yt_core::local_playlists::Video::to_item).collect()).unwrap_or_default(), None, 0);
                if *shuffle {
                    q.shuffle(None, crate::play_queue::seed(cx.views.now_ms));
                }
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
                if let Some(first) = q.at(0).cloned() {
                    fx.push(Fx::App(YtFx::SetQueue(q)));
                    fx.push(Fx::Nav(NavOp::Push(YtArg::Player { video_id: first, playlist: None })));
                }
            }
            Act::RenameLocal(list_id, title) => {
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
                fx.push(Fx::Nav(NavOp::Push(YtArg::NameEntry(NamePurpose::RenameLocal(list_id.clone(), title.clone())))));
            }
            Act::DeleteLocal(list_id, _) => {
                fx.push(Fx::App(YtFx::Local(LocalOp::Delete(list_id.clone()))));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
            }
            Act::CopyToTv(browse_id, title) => {
                fx.push(Fx::App(YtFx::Act(UserAction::CopyToTv { browse_id: browse_id.clone(), title: title.clone() })));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
            }
            Act::CopyToYouTube(list_id) => {
                if let Some(l) = cx.views.store.local.get(list_id) {
                    fx.push(Fx::App(YtFx::Toast(tr("Copying the playlist\u{2026}").to_string())));
                    fx.push(Fx::App(YtFx::Act(UserAction::CopyToYouTube { title: l.title.clone(), video_ids: l.video_ids() })));
                }
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
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
            Act::PlayNext(v, add) => {
                fx.push(Fx::App(YtFx::PlayNext { video: v.clone(), add: *add }));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
            }
            Act::Comments(v) => {
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
                fx.push(Fx::Nav(NavOp::Present(YtArg::Comments(v.clone()))));
            }
            Act::LiveChat => {
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
                fx.push(Fx::Nav(NavOp::Present(YtArg::LiveChat)));
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
                        3 => f.sort = step(&[Sort::Relevance, Sort::UploadDate, Sort::ViewCount], f.sort, dir),
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
            Act::InstallUpdate => {
                crate::update::install();
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
            }
            Act::SetBell(state, label) => {
                let st = cx.views.player;
                let options = st.extras.watch.as_ref().map(|w| w.notifications.clone()).unwrap_or_default();
                let name = st.extras.watch.as_ref().map(|w| w.channel_name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| st.author.clone());
                fx.push(Fx::App(YtFx::Act(UserAction::BellChoice { channel_id: st.channel_id.clone(), name, state: state.clone(), label: label.to_string(), options })));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
            }
            Act::Unsubscribe => {
                let channel_id = cx.views.player.channel_id.clone();
                if let Err(e) = yt_core::bell_channels::set(&channel_id, "", false) {
                    crate::log(&format!("bell channels: {e:#}"));
                }
                fx.push(Fx::App(YtFx::Act(UserAction::Subscribe { channel_id, subscribe: false })));
                fx.push(Fx::Nav(NavOp::Dismiss(self.list.entry)));
            }
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

/// The YouTube playlist a card opens, as a browse id ("VL..."); not mixes, which never end.
fn youtube_playlist(card: &crate::store::Card) -> Option<String> {
    use yt_core::model::ItemKind;
    match &card.item.endpoint {
        Some(Endpoint::Browse { browse_id, .. }) if browse_id.starts_with("VL") => Some(browse_id.clone()),
        Some(Endpoint::Watch { playlist_id: Some(pl), .. }) if card.item.kind == ItemKind::Playlist && !pl.starts_with("RD") => Some(format!("VL{pl}")),
        None if card.item.kind == ItemKind::Playlist && !card.item.id.is_empty() => Some(format!("VL{}", card.item.id)),
        _ => None,
    }
}

impl MenuScreen {
    /// The playlist picker: rebuilds its rows when the playlists they show changed (YouTube's
    /// arrived, or one on this TV changed). Checked before each draw, since a worker's answer
    /// reaches the store and wakes a frame but is not delivered to this screen.
    fn refresh_picker(&mut self, cx: &Cx<'_, YtHost>) {
        let Some(v) = self.picker_for.clone() else { return };
        let fresh = Self::playlists(self.list.entry, v, cx);
        if fresh.acts.iter().map(|a| &a.0).eq(self.acts.iter().map(|a| &a.0)) {
            return;
        }
        // The row under focus, by label: it must stay under focus when rows are inserted above
        // it (an OK right then must not hit a different playlist).
        let focused_label = self.list.focused(cx, self.acts.len()).and_then(|i| self.acts.get(i)).map(|a| a.0.clone());
        self.acts = fresh.acts;
        match focused_label.and_then(|l| self.acts.iter().position(|a| a.0 == l)) {
            Some(i) => self.refocus.set(Some(i)),
            // Focus was on the loading row, which is gone: the first row that does something.
            None => self.refocus.set(fresh.refocus.take()),
        }
    }
}

impl Machine<YtHost> for MenuScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                ui::MENUS_OPEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                // The picker asks for the YouTube playlists itself: the answer comes back to the
                // screen that asked, and only the picker rebuilds its rows with it.
                if let Some(v) = self.picker_for.as_ref().filter(|_| cx.views.store.signed_in) {
                    fx.push(Fx::App(YtFx::LoadPlaylists(v.video_id.clone())));
                }
            }
            ScreenEvent::Unmount => {
                ui::MENUS_OPEN.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            }
            _ => {}
        }
        if let Some(a) = super::common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
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
                    // Delete asks for a second OK (as removing an account does).
                    if let Act::DeleteLocal(_, title) = &act {
                        if self.armed != Some(i) {
                            self.armed = Some(i);
                            fx.push(Fx::App(YtFx::Toast(tr("Press OK again to delete {}").replace("{}", &format!("\u{201C}{title}\u{201D}")))));
                            fx.invalidate(plx_machine::present::Provenance::Input);
                            return Handled::Yes;
                        }
                    }
                    self.armed = None;
                    match self.choices(&act, cx) {
                        Some((v, cur)) => {
                            let anchor = self.list.place(*e, n, Some(i)).map(|p| p.rect).unwrap_or(self.list.rect);
                            self.choice = Some(ui::ChoiceBox::new(self.list.entry, i, v, cur, anchor).raw(matches!(act, Act::AudioTrack | Act::Quality)));
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
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: key @ (Key::Left | Key::Right), edge: edge @ (Edge::Down | Edge::Repeat), .. }, .. }) => {
                if let Some(i) = self.list.focused(cx, n) {
                    let act = self.acts[i].1.clone();
                    if self.value(&act, cx).is_some() && (*edge == Edge::Down || repeats(&act)) {
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
    fn prepare(&mut self, _b: &mut Budget, cx: &Cx<'_, YtHost>) {
        self.refresh_picker(cx);
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, YtHost>) {
        let p = f.painter;
        let cx = f.cx;
        let _dim = ui::HdrDim::begin(ui::hdr_playing(cx.views.player));
        ui::fill(p, PANEL, 0.0, ui::PANEL);
        ui::text_cut(p, f.measure, &self.title, PANEL.x + 52.0, 62.0, PANEL.w - 104.0, 30, ui::WHITE, ui::ALIGN_LEFT, true);
        let rows: Vec<Row> = self
            .acts
            .iter()
            .map(|(label, act)| {
                // A heading row is marked like the active choice of a list.
                let r = Row::new(label.clone()).raw().active(matches!(act, Act::Header));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    #[test]
    fn a_held_key_does_not_flip_switches() {
        assert!(!repeats(&Act::AudioOnly));
        assert!(!repeats(&Act::VideoDetails));
        assert!(!repeats(&Act::Bell));
        assert!(repeats(&Act::SubSize));
    }

    const VALUED: [Act; 6] = [Act::MaxQuality, Act::SubSize, Act::SubColor, Act::SubFont, Act::SubStyle, Act::RepeatMode];

    /// Picking value `k` from a row's list runs the row `k - current` steps: it must land on `k`
    /// for every row and every value, from every starting value.
    #[test]
    fn picking_a_value_from_a_list_sets_exactly_that_value() {
        for act in VALUED {
            let (values, _) = setting_choices(&act, &Settings::default()).unwrap();
            for from in 0..values.len() {
                let mut s = Settings::default();
                let cur = setting_choices(&act, &s).unwrap().1;
                step_setting(&act, &mut s, from as i32 - cur as i32);
                assert_eq!(setting_choices(&act, &s).unwrap().1, from, "{act:?}: could not reach {from}");
                for to in 0..values.len() {
                    let mut t = s.clone();
                    step_setting(&act, &mut t, to as i32 - from as i32);
                    assert_eq!(setting_choices(&act, &t).unwrap().1, to, "{act:?}: {from} -> {to}");
                }
            }
        }
    }

    #[test]
    fn the_settings_defaults_are_values_the_rows_offer() {
        let s = Settings::default();
        assert!(QUALITIES.contains(&s.max_height), "max_height {}", s.max_height);
        assert!(SUB_SIZES.contains(&s.subtitle_size), "subtitle_size {}", s.subtitle_size);
        assert!(SUB_COLORS.contains(&s.subtitle_color.as_str()), "subtitle_color {}", s.subtitle_color);
        assert!(SUB_FONTS.contains(&s.subtitle_font.as_str()), "subtitle_font {}", s.subtitle_font);
        assert!(SUB_STYLES.contains(&s.subtitle_style.as_str()), "subtitle_style {}", s.subtitle_style);
        assert!(SPEEDS.contains(&s.speed));
    }

    #[test]
    fn wrapping_rows_wrap_and_clamping_rows_stop() {
        let mut s = Settings::default();
        s.max_height = 2160;
        step_setting(&Act::MaxQuality, &mut s, 1);
        assert_eq!(s.max_height, 2160, "no quality above 2160p");
        s.min_height = 1080;
        step_setting(&Act::MaxQuality, &mut s, -3);
        assert_eq!((s.max_height, s.min_height), (720, 720), "the minimum follows the maximum down");
        s.playback_mode = PlaybackMode::ALL[PlaybackMode::ALL.len() - 1];
        step_setting(&Act::RepeatMode, &mut s, 1);
        assert_eq!(s.playback_mode, PlaybackMode::ALL[0]);
    }

    #[test]
    fn toggles_flip() {
        let mut s = Settings::default();
        let (sub, bold) = (s.subtitles, s.subtitle_bold);
        step_setting(&Act::Subtitles, &mut s, 1);
        step_setting(&Act::SubBold, &mut s, -1);
        assert_eq!((s.subtitles, s.subtitle_bold), (!sub, !bold));
        assert!(!s.video_details, "Video details is off by default");
        step_setting(&Act::VideoDetails, &mut s, 1);
        assert!(s.video_details);
    }

    #[test]
    fn the_speed_row_steps_through_the_list_and_stops_at_its_ends() {
        assert_eq!(step_speed(1.0, 1), 1.05);
        assert_eq!(step_speed(1.0, -1), 0.95);
        assert_eq!(step_speed(0.0, 1), 1.05, "an unknown rate counts as normal");
        assert_eq!(step_speed(4.0, 1), 4.0);
        assert_eq!(step_speed(0.25, -1), 0.25);
        assert_eq!(step_speed(1.33, 0), 1.3, "a rate between two steps snaps to the nearest");
        let one = crate::settings::speed_index(1.0) as i32;
        for (k, v) in SPEEDS.iter().enumerate() {
            assert_eq!(step_speed(1.0, k as i32 - one), *v);
        }
    }
}

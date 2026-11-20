//! Everything the screens read: feeds (cards + paging state), settings, account and transient UI
//! state. Owned by the Rig on the main thread; workers only ever post results back.

use crate::settings::Settings;
use std::collections::HashMap;
use yt_core::model::{Endpoint, Item, ItemKind, Page};

/// Sidebar sections (the TV guide's own list, as SmartTube shows it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    Home,
    Subscriptions,
    Channels,
    History,
    Library,
    Music,
    Gaming,
    News,
    Podcasts,
    Live,
    Movies,
    Sports,
    Trending,
    /// The account's notifications inbox (signed in only; `yt_core::notifications`).
    Notifications,
}

impl Section {
    pub const ALL: &'static [Section] = &[
        Section::Home,
        Section::Subscriptions,
        Section::Channels,
        Section::History,
        Section::Library,
        Section::Music,
        Section::Gaming,
        Section::News,
        Section::Podcasts,
        Section::Live,
        Section::Movies,
        Section::Sports,
        Section::Trending,
        Section::Notifications,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Section::Home => "home",
            Section::Subscriptions => "subscriptions",
            Section::Channels => "channels",
            Section::History => "history",
            Section::Library => "library",
            Section::Music => "music",
            Section::Gaming => "gaming",
            Section::News => "news",
            Section::Podcasts => "podcasts",
            Section::Live => "live",
            Section::Movies => "movies",
            Section::Sports => "sports",
            Section::Trending => "trending",
            Section::Notifications => "notifications",
        }
    }

    pub fn from_key(k: &str) -> Option<Section> {
        Section::ALL.iter().copied().find(|s| s.key() == k)
    }

    pub fn title(self) -> &'static str {
        match self {
            Section::Home => "Home",
            Section::Subscriptions => "Subscriptions",
            Section::Channels => "Channels",
            Section::History => "History",
            Section::Library => "Library",
            Section::Music => "Music",
            Section::Gaming => "Gaming",
            Section::News => "News",
            Section::Podcasts => "Podcasts",
            Section::Live => "Live",
            Section::Movies => "Movies",
            Section::Sports => "Sports",
            Section::Trending => "Trending",
            Section::Notifications => "Notifications",
        }
    }

    /// Shown only to a signed-in viewer (the section has nothing without an account).
    pub fn needs_account(self) -> bool {
        self == Section::Notifications
    }

    /// Left out in child mode (the inbox shows other people's comment replies).
    pub fn hidden_in_child_mode(self) -> bool {
        self == Section::Notifications
    }

    /// The section can be in the sidebar for this viewer (signed in or not, child mode or not).
    pub fn allowed(self, signed_in: bool, child_mode: bool) -> bool {
        (signed_in || !self.needs_account()) && !(child_mode && self.hidden_in_child_mode())
    }

    /// The section to open at start (the "Start in" choice): Home in place of a section the
    /// viewer cannot have now. Notifications also has to be turned on in the sidebar: an inbox
    /// the viewer took out of it is not opened on its own.
    pub fn boot(key: &str, s: &crate::settings::Settings, signed_in: bool) -> Section {
        match Section::from_key(key) {
            Some(sec) if sec.allowed(signed_in, s.child_mode) && (!sec.needs_account() || s.sections.iter().any(|k| k == sec.key())) => sec,
            _ => Section::Home,
        }
    }

    pub fn browse_id(self) -> &'static str {
        match self {
            Section::Home => "FEwhat_to_watch",
            Section::Subscriptions | Section::Channels => "FEsubscriptions",
            Section::History => "FEhistory",
            Section::Library => "FElibrary",
            Section::Music => "FEtopics_music",
            Section::Gaming => "FEtopics_gaming",
            Section::News => "FEtopics_news",
            Section::Podcasts => "FEtopics_podcasts",
            Section::Live => "FEtopics_live",
            Section::Movies => "FEtopics_movies",
            Section::Sports => "FEtopics_sports",
            Section::Trending => "FEtrending",
            // Not a browse page: loaded from `notification/get_notification_menu` instead.
            Section::Notifications => "FEnotifications",
        }
    }
}

/// What a grid of cards is showing.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FeedKey {
    Section(Section),
    Browse { id: String, params: Option<String> },
    Search { query: String, params: Option<String> },
    /// Suggestions / up-next for a video.
    Related(String),
    /// A local channel group (RSS-fed): group id.
    Group(String),
    /// A playlist on this TV (`yt_core::local_playlists`): list id.
    Local(String),
}

impl FeedKey {
    /// The page a browse id opens: a playlist on this TV, or YouTube's page.
    pub fn browse(id: &str, params: Option<String>) -> FeedKey {
        if yt_core::local_playlists::is_local_id(id) {
            FeedKey::Local(id.to_string())
        } else {
            FeedKey::Browse { id: id.to_string(), params }
        }
    }

    pub fn endpoint(&self) -> &'static str {
        match self {
            FeedKey::Search { .. } => "search",
            _ => "browse",
        }
    }
}

/// One card's data as the screens read it.
#[derive(Clone, Debug, Default)]
pub struct Card {
    pub elem: u32,
    pub item: Item,
    pub title: String,
    pub detail: String,
    pub thumb: String,
}

impl Card {
    pub fn video_id(&self) -> Option<&str> {
        match &self.item.endpoint {
            Some(Endpoint::Watch { video_id, .. }) if !video_id.is_empty() => Some(video_id),
            _ if matches!(self.item.kind, ItemKind::Video | ItemKind::Short | ItemKind::Live) && !self.item.id.is_empty() => {
                Some(&self.item.id)
            }
            _ => None,
        }
    }

    /// The card's video as a record for a playlist on this TV (None for channels, playlists).
    pub fn local_video(&self) -> Option<yt_core::local_playlists::Video> {
        let id = self.video_id()?;
        let it = &self.item;
        Some(yt_core::local_playlists::Video {
            video_id: id.to_string(),
            title: if it.title.is_empty() { self.title.clone() } else { it.title.clone() },
            channel: it.channel.clone().or_else(|| it.lines.first().cloned()).unwrap_or_default(),
            duration_secs: it.badge.as_deref().and_then(parse_duration).map_or(0, |ms| ms / 1000),
            thumbnail: it.thumbnail.clone().filter(|t| !t.is_empty()),
        })
    }

    /// The playlist on this TV this card stands for (a card of the Library's "On this TV" row).
    pub fn local_list(&self) -> Option<&str> {
        match &self.item.endpoint {
            Some(Endpoint::Browse { browse_id, .. }) if yt_core::local_playlists::is_local_id(browse_id) => Some(browse_id),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Feed {
    pub title: String,
    pub cards: Vec<Card>,
    pub continuation: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
    /// Bumped whenever cards change, so screens can notice.
    pub gen: u32,
    /// Rows of `cards`, in order. One untitled row = a plain grid.
    pub shelves: Vec<Row>,
}

/// One row (shelf) of a feed: its title, its cards' range in `Feed::cards`, and the token for
/// more of THIS row (YouTube's rows continue to the right).
#[derive(Clone, Debug, Default)]
pub struct Row {
    pub title: String,
    pub range: std::ops::Range<usize>,
    pub continuation: Option<String>,
    pub loading: bool,
}

impl Feed {
    /// Several titled rows: show as shelves (Home, topics) rather than one grid.
    pub fn has_rows(&self) -> bool {
        self.shelves.len() > 1 && self.shelves.iter().filter(|s| !s.title.is_empty()).count() >= 2
    }
}

#[derive(Clone, Debug, Default)]
pub struct AccountRow {
    pub id: String,
    pub name: String,
    pub current: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Comments {
    pub items: Vec<yt_core::comments::Comment>,
    pub next: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct Store {
    pub accounts: Vec<AccountRow>,
    /// Local channel groups: (id, title).
    pub groups: Vec<(String, String)>,
    /// Subscribe state per channel id (from YouTube, updated optimistically).
    pub subscribed: HashMap<String, bool>,
    /// The viewer's rating per video id, set when they press like / dislike (YouTube's own reads
    /// lag behind).
    pub rated: HashMap<String, yt_core::watch::LikeStatus>,
    /// The notification bell picked for a channel here (its `SUBSCRIPTION_NOTIFICATION_STATE_*`),
    /// over what the video's page said, until YouTube's pages catch up.
    pub bell: HashMap<String, String>,
    /// A SponsorBlock segment is offered for skipping (OK skips it).
    pub sb_offered: bool,
    /// Playlists for the "Save to playlist" picker: video id -> options.
    pub playlist_options: HashMap<String, Vec<yt_core::actions::PlaylistOption>>,
    /// The list the playing video was started from ([`crate::play_queue::PlayQueue`]): what plays
    /// next, Previous / Next, the player's playlist row. It ends when the viewer leaves the player.
    pub play_queue: Option<crate::play_queue::PlayQueue>,
    /// "Play next": videos the viewer queued, played in order before YouTube's up next. For
    /// this run only, unless "Keep Play next after closing" is on.
    pub play_next: Vec<yt_core::local_playlists::Video>,
    /// The playlists on this TV (saved by the Rig after every change).
    pub local: yt_core::local_playlists::Store,
    /// Filters of the last search (the filters panel edits these).
    pub search_filters: yt_core::search_filters::Filters,
    pub comments: HashMap<String, Comments>,
    /// The live chat of the video playing (while its panel is open).
    pub live_chat: crate::live_chat::Chat,
    pub feeds: HashMap<FeedKey, Feed>,
    pub settings: Settings,
    pub account: Option<String>,
    pub signed_in: bool,
    pub toast: Option<(String, u32)>,
    /// The sleep timer, while it runs ([`SleepTimer`]).
    pub sleep: Option<SleepTimer>,
    pub suggestions: Vec<String>,
    pub lounge_code: Option<String>,
    next_elem: u32,
}

impl Store {
    pub fn new(settings: Settings) -> Store {
        Store { settings, next_elem: 1, ..Default::default() }
    }

    pub fn feed(&self, k: &FeedKey) -> Option<&Feed> {
        self.feeds.get(k)
    }

    pub fn cards(&self, k: &FeedKey) -> &[Card] {
        self.feeds.get(k).map(|f| f.cards.as_slice()).unwrap_or(&[])
    }

    pub fn card(&self, elem: u32) -> Option<&Card> {
        self.feeds.values().flat_map(|f| f.cards.iter()).find(|c| c.elem == elem)
    }

    pub fn mark_loading(&mut self, k: &FeedKey) {
        let f = self.feeds.entry(k.clone()).or_default();
        f.loading = true;
        f.error = None;
    }

    /// Replace (first page) or append (continuation) a page of results.
    pub fn apply(&mut self, k: &FeedKey, page: Page, append: bool) {
        let filter = Filter::from(&self.settings, k);
        let mut seen: std::collections::HashSet<String> = if append {
            self.cards(k).iter().map(|c| c.item.id.clone()).collect()
        } else {
            Default::default()
        };
        let mut fresh = Vec::new();
        let channel_items: Vec<Item>;
        let groups: Vec<(String, Vec<&Item>, Option<String>)> = if *k == FeedKey::Section(Section::Channels) {
            let mut items: Vec<Item> = page
                .tabs
                .iter()
                .filter(|t| matches!(&t.endpoint, Some(Endpoint::Browse { params: Some(_), .. })))
                .map(|t| Item {
                    kind: ItemKind::Channel,
                    id: t.title.clone(),
                    title: t.title.clone(),
                    thumbnail: t.thumbnail.clone(),
                    endpoint: t.endpoint.clone(),
                    ..Default::default()
                })
                .collect();
            if self.settings.channels_alphabetical {
                items.sort_by_cached_key(|i| i.title.to_lowercase());
            }
            channel_items = items;
            vec![(String::new(), channel_items.iter().collect(), None)]
        } else {
            page.shelves.iter().map(|s| (s.title.clone(), s.items.iter().collect(), s.continuation.clone())).collect()
        };
        let base = if append { self.cards(k).len() } else { 0 };
        // Lists rebuilt after an edit on this TV keep their cards' elements, so focus stays on
        // the same video (or list) when the order changes.
        let reuse: HashMap<String, u32> = if !append && matches!(k, FeedKey::Local(_) | FeedKey::Section(Section::Library)) {
            self.cards(k).iter().map(|c| (c.item.id.clone(), c.elem)).collect()
        } else {
            Default::default()
        };
        let mut rows = Vec::new();
        for (title, items, continuation) in groups {
            let start = fresh.len();
            for it in items {
                if !seen.insert(it.id.clone()) || !filter.keep(it) {
                    continue;
                }
                let elem = match reuse.get(&it.id) {
                    Some(e) => *e,
                    None => {
                        self.next_elem += 1;
                        self.next_elem
                    }
                };
                fresh.push(Card {
                    elem,
                    title: it.title.clone(),
                    detail: it.lines.join(" \u{2022} "),
                    thumb: card_thumb(&self.settings, it),
                    item: it.clone(),
                });
            }
            if fresh.len() > start {
                rows.push(Row { title, range: base + start..base + fresh.len(), continuation, loading: false });
            }
        }
        let f = self.feeds.entry(k.clone()).or_default();
        if append {
            f.cards.extend(fresh);
            f.shelves.extend(rows);
        } else {
            f.cards = fresh;
            f.shelves = rows;
            if let Some(t) = page.title.clone() {
                f.title = t;
            }
        }
        f.continuation = page.continuation;
        f.loading = false;
        f.error = None;
        f.gen += 1;
    }

    /// Marks row `row` of feed `k` as loading more; returns its token when there is one to load.
    pub fn begin_shelf(&mut self, k: &FeedKey, row: usize) -> Option<String> {
        let r = self.feeds.get_mut(k)?.shelves.get_mut(row)?;
        if r.loading {
            return None;
        }
        let t = r.continuation.clone()?;
        r.loading = true;
        Some(t)
    }

    /// The next items of row `row` (asked with `token`): inserted at the row's end, later rows
    /// shifted. Returns the range of the new cards. A stale answer (the feed reloaded, the row
    /// moved on) is dropped.
    pub fn apply_shelf(&mut self, k: &FeedKey, row: usize, token: &str, items: Vec<Item>, next: Option<String>) -> std::ops::Range<usize> {
        let filter = Filter::from(&self.settings, k);
        let Some(f) = self.feeds.get(k) else { return 0..0 };
        let Some(r) = f.shelves.get(row) else { return 0..0 };
        if r.continuation.as_deref() != Some(token) {
            return 0..0;
        }
        let seen: std::collections::HashSet<String> = f.cards[r.range.clone()].iter().map(|c| c.item.id.clone()).collect();
        let at = r.range.end;
        let mut fresh = Vec::new();
        for it in items {
            if seen.contains(&it.id) || !filter.keep(&it) || fresh.iter().any(|c: &Card| c.item.id == it.id) {
                continue;
            }
            self.next_elem += 1;
            fresh.push(Card {
                elem: self.next_elem,
                title: it.title.clone(),
                detail: it.lines.join(" \u{2022} "),
                thumb: card_thumb(&self.settings, &it),
                item: it,
            });
        }
        let n = fresh.len();
        let f = self.feeds.get_mut(k).unwrap();
        f.cards.splice(at..at, fresh);
        for (i, s) in f.shelves.iter_mut().enumerate() {
            if i == row {
                s.range.end += n;
                // The same token again with nothing new: the row has ended.
                s.continuation = if n == 0 && next.as_deref() == Some(token) { None } else { next.clone() };
                s.loading = false;
            } else if i > row {
                s.range.start += n;
                s.range.end += n;
            }
        }
        f.gen += 1;
        at..at + n
    }

    /// Takes the cards that carry menu action `m` out of every feed (the entry it removed),
    /// keeping each row's range in step.
    pub fn remove_cards_with_menu(&mut self, m: &yt_core::actions::MenuAction) {
        for f in self.feeds.values_mut() {
            while let Some(i) = f.cards.iter().position(|c| c.item.menu.contains(m)) {
                f.cards.remove(i);
                for s in f.shelves.iter_mut() {
                    if s.range.start > i {
                        s.range.start -= 1;
                    }
                    if s.range.end > i {
                        s.range.end -= 1;
                    }
                }
                f.gen += 1;
            }
        }
    }

    /// Loading more of row `row` failed: it stops asking (until the feed reloads).
    pub fn fail_shelf(&mut self, k: &FeedKey, row: usize) {
        if let Some(r) = self.feeds.get_mut(k).and_then(|f| f.shelves.get_mut(row)) {
            r.loading = false;
            r.continuation = None;
        }
    }

    pub fn fail(&mut self, k: &FeedKey, e: String) {
        let f = self.feeds.entry(k.clone()).or_default();
        f.loading = false;
        f.error = Some(e);
        f.gen += 1;
    }

    /// The channel name of a video's card, if any card shows it.
    pub fn card_channel(&self, video_id: &str) -> Option<String> {
        self.feeds.values().flat_map(|f| f.cards.iter()).find(|c| c.video_id() == Some(video_id)).and_then(|c| c.item.channel.clone())
    }

    /// A video's length from its card's duration badge (ms).
    pub fn card_duration_ms(&self, video_id: &str) -> Option<u64> {
        let card = self.feeds.values().flat_map(|f| f.cards.iter()).find(|c| c.video_id() == Some(video_id))?;
        parse_duration(card.item.badge.as_deref()?)
    }

    /// A card shows `video_id` as a live stream.
    pub fn card_is_live(&self, video_id: &str) -> bool {
        self.feeds.values().flat_map(|f| f.cards.iter()).any(|c| c.video_id() == Some(video_id) && c.item.kind == ItemKind::Live)
    }

    /// Where to resume a partly watched video: its card's watched percentage of its duration
    /// badge. Nothing for unwatched or (nearly) finished videos.
    pub fn resume_point(&self, video_id: &str) -> Option<u64> {
        let card = self.feeds.values().flat_map(|f| f.cards.iter()).find(|c| c.video_id() == Some(video_id))?;
        let pct = card.item.progress? as u64;
        if !(2..95).contains(&pct) {
            return None;
        }
        let dur = parse_duration(card.item.badge.as_deref()?)?;
        Some(dur * pct / 100)
    }

    pub fn toast(&mut self, msg: impl Into<String>, now_ms: u32) {
        self.toast = Some((msg.into(), now_ms + 3500));
    }
}

/// The sleep timer: set once in the player's options, it runs across the next videos and pauses
/// playback when it lapses; leaving the player for the lists, or setting it to Off, ends it.
/// A minute before, a notice says so, and a key then starts it over with the same length.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SleepTimer {
    /// Its length, minutes (one of `settings::SLEEP_MINS`).
    pub mins: u32,
    /// When it lapses (app time, ms).
    pub ends_ms: u32,
    /// When the "one minute left" notice was shown.
    pub warned_ms: Option<u32>,
}

impl SleepTimer {
    pub fn start(mins: u32, now_ms: u32) -> SleepTimer {
        SleepTimer { mins, ends_ms: now_ms.wrapping_add(mins.saturating_mul(60_000)), warned_ms: None }
    }

    /// Milliseconds left at `now_ms` (0 once it lapsed).
    pub fn left_ms(&self, now_ms: u32) -> u32 {
        let left = self.ends_ms.wrapping_sub(now_ms) as i32;
        left.max(0) as u32
    }

    /// "24 minutes left" (rounded up; translated).
    pub fn left_label(&self, now_ms: u32) -> String {
        let m = self.left_ms(now_ms).div_ceil(60_000).max(1);
        if m == 1 {
            crate::i18n::tr("1 minute left").to_string()
        } else {
            crate::i18n::tr("{} minutes left").replace("{}", &m.to_string())
        }
    }
}

/// What the sleep timer does at `now_ms` ([`SleepStep`]); `key_ms`: the last key press.
pub fn sleep_step(t: &SleepTimer, now_ms: u32, key_ms: u32) -> SleepStep {
    if t.left_ms(now_ms) == 0 {
        return SleepStep::Lapse;
    }
    match t.warned_ms {
        // A key after the notice: the viewer is watching.
        Some(w) if (key_ms.wrapping_sub(w) as i32) > 0 => SleepStep::Restart,
        None if t.left_ms(now_ms) <= 60_000 => SleepStep::Warn,
        _ => SleepStep::Wait,
    }
}

#[derive(Debug, PartialEq)]
pub enum SleepStep {
    Wait,
    /// Show the "pauses in a minute" notice.
    Warn,
    /// A key answered the notice: start over with the same length.
    Restart,
    /// Time is up: pause, and the timer ends.
    Lapse,
}

#[cfg(test)]
mod sleep_tests {
    use super::*;

    #[test]
    fn the_sleep_timer_warns_restarts_and_lapses() {
        let t = SleepTimer::start(30, 1_000);
        assert_eq!(sleep_step(&t, 1_000, 0), SleepStep::Wait);
        assert_eq!(t.left_label(1_000), "30 minutes left");
        let warn_at = 1_000 + 29 * 60_000;
        assert_eq!(sleep_step(&t, warn_at, 0), SleepStep::Warn);
        assert_eq!(t.left_label(warn_at + 1), "1 minute left");
        let warned = SleepTimer { warned_ms: Some(warn_at), ..t };
        assert_eq!(sleep_step(&warned, warn_at + 10_000, warn_at - 5), SleepStep::Wait, "a key before the notice does not count");
        assert_eq!(sleep_step(&warned, warn_at + 10_000, warn_at + 9_000), SleepStep::Restart);
        assert_eq!(sleep_step(&warned, 1_000 + 30 * 60_000, 0), SleepStep::Lapse);
    }
}

/// Content filters from settings (SmartTube's "hide Shorts / watched / upcoming / mixes",
/// blocked channels).
struct Filter<'a> {
    s: &'a Settings,
    hide_shorts: bool,
    hide_watched: bool,
    hide_upcoming: bool,
    hide_streams: bool,
    /// A playlist on this TV shows every video the viewer put in it.
    everything: bool,
}

impl<'a> Filter<'a> {
    fn from(s: &'a Settings, k: &FeedKey) -> Self {
        let home = *k == FeedKey::Section(Section::Home);
        let subs = *k == FeedKey::Section(Section::Subscriptions);
        let channel = matches!(k, FeedKey::Browse { id, .. } if id.starts_with("UC"));
        let hide_shorts = s.hide_shorts_everywhere
            || match k {
                FeedKey::Section(Section::Home) => s.hide_shorts_home,
                FeedKey::Section(Section::Subscriptions) => s.hide_shorts_subscriptions,
                FeedKey::Search { .. } => s.hide_shorts_search,
                _ => false,
            };
        Filter {
            s,
            hide_shorts,
            hide_watched: (home && s.hide_watched_home) || (subs && s.hide_watched_subscriptions),
            hide_upcoming: (home && s.hide_upcoming_home) || (subs && s.hide_upcoming_subscriptions) || (channel && s.hide_upcoming_channels),
            hide_streams: subs && s.hide_streams_subscriptions,
            everything: matches!(k, FeedKey::Local(_)),
        }
    }

    fn keep(&self, it: &Item) -> bool {
        if self.everything {
            return true;
        }
        if self.hide_shorts && it.kind == ItemKind::Short {
            return false;
        }
        if self.hide_upcoming && it.kind == ItemKind::Upcoming {
            return false;
        }
        // Live now, or a past live stream ("Streamed 2 days ago"; requests are made in English).
        if self.hide_streams && (it.kind == ItemKind::Live || it.lines.iter().any(|l| l.contains("Streamed "))) {
            return false;
        }
        if self.s.hide_mixes && it.kind == ItemKind::Mix {
            return false;
        }
        if self.hide_watched && it.progress.is_some_and(|p| p >= 90) {
            return false;
        }
        if let Some(ch) = &it.channel {
            if self.s.blocked_channels.iter().any(|b| b == ch) {
                return false;
            }
        }
        true
    }
}

/// A card's picture: the item's thumbnail, or YouTube's frame from the start / middle / end of
/// a video ("Thumbnail picture").
fn card_thumb(s: &Settings, it: &Item) -> String {
    let t = it.thumbnail.clone().unwrap_or_default();
    if it.kind == ItemKind::Video {
        crate::settings::thumbnail_frame(&s.thumbnail_frame).rewrite(&t)
    } else {
        t
    }
}

/// "1:02:03" / "12:34" -> milliseconds.
pub fn parse_duration(s: &str) -> Option<u64> {
    let mut total = 0u64;
    for part in s.trim().split(':') {
        total = total * 60 + part.parse::<u64>().ok()?;
    }
    Some(total * 1000)
}

#[cfg(test)]
mod tests {
    #[test]
    fn durations() {
        assert_eq!(super::parse_duration("12:34"), Some(754_000));
        assert_eq!(super::parse_duration("1:02:03"), Some(3_723_000));
        assert_eq!(super::parse_duration("LIVE"), None);
    }

    #[test]
    fn notifications_section_availability() {
        use super::Section;
        assert!(Section::Notifications.allowed(true, false));
        assert!(!Section::Notifications.allowed(false, false), "signed out");
        assert!(!Section::Notifications.allowed(true, true), "child mode");
        assert!(Section::Home.allowed(false, true));
    }

    #[test]
    fn start_in_falls_back_to_home() {
        use super::Section;
        let mut s = crate::settings::Settings::default();
        s.sections.retain(|k| k != "notifications");
        s.sections.push("notifications".into());
        assert_eq!(Section::boot("notifications", &s, true), Section::Notifications);
        assert_eq!(Section::boot("notifications", &s, false), Section::Home, "signed out");
        s.child_mode = true;
        assert_eq!(Section::boot("notifications", &s, true), Section::Home, "child mode");
        s.child_mode = false;
        s.sections.retain(|k| k != "notifications");
        assert_eq!(Section::boot("notifications", &s, true), Section::Home, "turned off");
        // Other sections open as before, in the sidebar or not.
        assert_eq!(Section::boot("music", &s, false), Section::Music);
        assert_eq!(Section::boot("nonsense", &s, true), Section::Home);
    }
}

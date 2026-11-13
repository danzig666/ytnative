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
}

impl FeedKey {
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
    /// A SponsorBlock segment is offered for skipping (OK skips it).
    pub sb_offered: bool,
    /// Playlists for the "Save to playlist" picker: video id -> options.
    pub playlist_options: HashMap<String, Vec<yt_core::actions::PlaylistOption>>,
    /// Videos of the list the current video was opened from (for next/previous), and whether
    /// they are Shorts (Up/Down then switch videos, like YouTube's Shorts player).
    pub queue: Vec<String>,
    pub queue_shorts: bool,
    /// Filters of the last search (the filters panel edits these).
    pub search_filters: yt_core::search_filters::Filters,
    pub comments: HashMap<String, Comments>,
    pub feeds: HashMap<FeedKey, Feed>,
    pub settings: Settings,
    pub account: Option<String>,
    pub signed_in: bool,
    pub toast: Option<(String, u32)>,
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
        let mut rows = Vec::new();
        for (title, items, continuation) in groups {
            let start = fresh.len();
            for it in items {
                if !seen.insert(it.id.clone()) || !filter.keep(it) {
                    continue;
                }
                self.next_elem += 1;
                fresh.push(Card {
                    elem: self.next_elem,
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

/// Content filters from settings (SmartTube's "hide Shorts / watched / upcoming / mixes",
/// blocked channels).
struct Filter<'a> {
    s: &'a Settings,
    hide_shorts: bool,
    hide_watched: bool,
    hide_upcoming: bool,
    hide_streams: bool,
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
        }
    }

    fn keep(&self, it: &Item) -> bool {
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
}

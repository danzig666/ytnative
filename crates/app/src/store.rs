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
    /// Titled rows of `cards` (title, index range), in order. One untitled entry = a plain grid.
    pub shelves: Vec<(String, std::ops::Range<usize>)>,
}

impl Feed {
    /// Several titled rows: show as shelves (Home, topics) rather than one grid.
    pub fn has_rows(&self) -> bool {
        self.shelves.len() > 1 && self.shelves.iter().filter(|s| !s.0.is_empty()).count() >= 2
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
    /// Sleep timer length chosen (0 = off), for the menu.
    pub sleep_minutes: u32,
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
        let groups: Vec<(String, Vec<&Item>)> = if *k == FeedKey::Section(Section::Channels) {
            channel_items = page
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
            vec![(String::new(), channel_items.iter().collect())]
        } else {
            page.shelves.iter().map(|s| (s.title.clone(), s.items.iter().collect())).collect()
        };
        let base = if append { self.cards(k).len() } else { 0 };
        let mut rows = Vec::new();
        for (title, items) in groups {
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
                    thumb: it.thumbnail.clone().unwrap_or_default(),
                    item: it.clone(),
                });
            }
            if fresh.len() > start {
                rows.push((title, base + start..base + fresh.len()));
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
}

impl<'a> Filter<'a> {
    fn from(s: &'a Settings, k: &FeedKey) -> Self {
        let hide_shorts = match k {
            FeedKey::Section(Section::Home) => s.hide_shorts_home,
            FeedKey::Section(Section::Subscriptions) => s.hide_shorts_subscriptions,
            FeedKey::Search { .. } => s.hide_shorts_search,
            _ => false,
        };
        Filter { s, hide_shorts }
    }

    fn keep(&self, it: &Item) -> bool {
        if self.hide_shorts && it.kind == ItemKind::Short {
            return false;
        }
        if self.s.hide_upcoming && it.kind == ItemKind::Upcoming {
            return false;
        }
        if self.s.hide_mixes && it.kind == ItemKind::Mix {
            return false;
        }
        if self.s.hide_watched && it.progress.is_some_and(|p| p >= 90) {
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

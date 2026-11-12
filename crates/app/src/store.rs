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
            Section::Subscriptions => "FEsubscriptions",
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
}

#[derive(Default)]
pub struct Store {
    pub feeds: HashMap<FeedKey, Feed>,
    pub settings: Settings,
    pub account: Option<String>,
    pub signed_in: bool,
    pub toast: Option<(String, u32)>,
    pub suggestions: Vec<String>,
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
        for it in page.shelves.iter().flat_map(|s| s.items.iter()) {
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
        let f = self.feeds.entry(k.clone()).or_default();
        if append {
            f.cards.extend(fresh);
        } else {
            f.cards = fresh;
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

//! Client-side content model: what screens render. Built from InnerTube responses by `parse`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ItemKind {
    #[default]
    Video,
    Short,
    Live,
    Upcoming,
    Channel,
    Playlist,
    Mix,
    Unknown,
}

/// Where activating an item leads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Endpoint {
    Watch { video_id: String, playlist_id: Option<String>, params: Option<String> },
    Browse { browse_id: String, params: Option<String> },
    Search { query: String, params: Option<String> },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Item {
    pub kind: ItemKind,
    /// videoId / channelId / playlistId depending on `kind`.
    pub id: String,
    pub title: String,
    /// Secondary lines as shown under the title ("Channel", "1K views • 2 days ago").
    pub lines: Vec<String>,
    /// Channel name when it can be told apart from the other metadata.
    pub channel: Option<String>,
    pub thumbnail: Option<String>,
    /// "12:34", "LIVE", "SHORTS", "UPCOMING".
    pub badge: Option<String>,
    /// 0..=100 when the user has partially watched it.
    pub progress: Option<u8>,
    /// Only the channel's members can watch it (YouTube's "Members only" badge).
    #[serde(default)]
    pub members_only: bool,
    pub endpoint: Option<Endpoint>,
    /// The card's own menu (Watch later, Not interested, Remove from history...), as YouTube
    /// sent it. Home tiles often carry only a lazy "More" entry resolved with `menu_panel`.
    #[serde(default)]
    pub menu: Vec<crate::actions::MenuAction>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Shelf {
    pub title: String,
    pub items: Vec<Item>,
    /// Token to fetch more items of this shelf (horizontal continuation).
    pub continuation: Option<String>,
    /// "More" target of the shelf header, if any.
    pub endpoint: Option<Endpoint>,
}

/// A navigation tab inside a page (Subscriptions' channel list, a channel's Videos/Playlists...).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Tab {
    pub title: String,
    pub thumbnail: Option<String>,
    pub selected: bool,
    pub endpoint: Option<Endpoint>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Page {
    pub title: Option<String>,
    pub shelves: Vec<Shelf>,
    pub tabs: Vec<Tab>,
    /// Token to fetch more shelves (vertical continuation).
    pub continuation: Option<String>,
}

impl Page {
    pub fn item_count(&self) -> usize {
        self.shelves.iter().map(|s| s.items.len()).sum()
    }
}

//! Playlists kept on this TV ("On this TV"): lists of videos the viewer makes without a YouTube
//! account, saved in `local_playlists.json` in the data dir. Each list has a stable id starting
//! with [`ID_PREFIX`], a title, the time it was made and its videos in order.
//!
//! The file is written atomically and a damaged file is moved aside to `.bad` (see
//! [`crate::fsutil`]). A list holds at most [`MAX_ITEMS`] videos; a video is in a list at most
//! once. Videos that later become unavailable stay in the list: nothing is removed silently.
//!
//! The "Play next" queue can be kept between runs in `play_next.json` ([`load_queue`] /
//! [`save_queue`]); it uses the same [`Video`] records.

use crate::model::{Endpoint, Item, ItemKind, Page, Shelf};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "local_playlists.json";
/// The kept "Play next" queue.
pub const QUEUE_FILE_NAME: &str = "play_next.json";
const STORE_VERSION: u32 = 1;
/// Videos one list can hold.
pub const MAX_ITEMS: usize = 5000;
/// Every list id starts with this, which tells a list on this TV apart from a YouTube id.
pub const ID_PREFIX: &str = "tv-";

/// Whether `id` names a list on this TV.
pub fn is_local_id(id: &str) -> bool {
    id.starts_with(ID_PREFIX)
}

/// One video in a list: enough to draw its card without asking YouTube.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Video {
    pub video_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub channel: String,
    /// Length in seconds (0 when it is not known, for example a live stream).
    #[serde(default)]
    pub duration_secs: u64,
    #[serde(default)]
    pub thumbnail: Option<String>,
}

impl Video {
    pub fn new(video_id: impl Into<String>) -> Self {
        Video { video_id: video_id.into(), ..Default::default() }
    }

    /// The thumbnail to show: the stored one, else YouTube's standard picture of the video.
    pub fn thumbnail_url(&self) -> String {
        self.thumbnail.clone().filter(|t| !t.is_empty()).unwrap_or_else(|| format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", self.video_id))
    }

    /// The video as a card item, like one of a YouTube playlist.
    pub fn to_item(&self) -> Item {
        Item {
            kind: ItemKind::Video,
            id: self.video_id.clone(),
            title: self.title.clone(),
            lines: if self.channel.is_empty() { Vec::new() } else { vec![self.channel.clone()] },
            channel: Some(self.channel.clone()).filter(|c| !c.is_empty()),
            thumbnail: Some(self.thumbnail_url()),
            badge: (self.duration_secs > 0).then(|| duration_text(self.duration_secs)),
            endpoint: Some(Endpoint::Watch { video_id: self.video_id.clone(), playlist_id: None, params: None }),
            ..Default::default()
        }
    }
}

/// "12:34" or "1:02:03".
pub fn duration_text(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct List {
    pub id: String,
    pub title: String,
    /// Unix seconds.
    #[serde(default)]
    pub created: u64,
    #[serde(default)]
    pub items: Vec<Video>,
}

/// What [`List::add`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Added {
    Added,
    /// The video is already in the list: nothing changed.
    AlreadyThere,
    /// The list holds [`MAX_ITEMS`] videos already.
    Full,
}

/// Where [`List::move_item`] moves a video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Top,
    Up,
    Down,
}

impl List {
    pub fn new(title: impl Into<String>) -> Self {
        List { id: new_id(), title: title.into(), created: crate::fsutil::now_secs(), items: Vec::new() }
    }

    pub fn contains(&self, video_id: &str) -> bool {
        self.items.iter().any(|v| v.video_id == video_id)
    }

    /// Adds a video at the end.
    pub fn add(&mut self, v: Video) -> Added {
        if v.video_id.is_empty() || self.contains(&v.video_id) {
            return Added::AlreadyThere;
        }
        if self.items.len() >= MAX_ITEMS {
            return Added::Full;
        }
        self.items.push(v);
        Added::Added
    }

    pub fn remove(&mut self, video_id: &str) -> bool {
        let n = self.items.len();
        self.items.retain(|v| v.video_id != video_id);
        n != self.items.len()
    }

    /// Moves a video to the top or one place up or down. Returns true if it moved.
    pub fn move_item(&mut self, video_id: &str, to: Move) -> bool {
        let Some(i) = self.items.iter().position(|v| v.video_id == video_id) else { return false };
        let j = match to {
            Move::Top => 0,
            Move::Up => i.saturating_sub(1),
            Move::Down => (i + 1).min(self.items.len() - 1),
        };
        if i == j {
            return false;
        }
        let v = self.items.remove(i);
        self.items.insert(j, v);
        true
    }

    pub fn video_ids(&self) -> Vec<String> {
        self.items.iter().map(|v| v.video_id.clone()).collect()
    }

    /// The list's videos as a page for the card grid.
    pub fn to_page(&self) -> Page {
        Page {
            title: Some(self.title.clone()),
            shelves: vec![Shelf { items: self.items.iter().map(Video::to_item).collect(), ..Default::default() }],
            ..Default::default()
        }
    }

    /// The list as one card (its first video's picture); `count` words the number of videos
    /// ("12 videos").
    pub fn to_item(&self, count: &dyn Fn(usize) -> String) -> Item {
        Item {
            kind: ItemKind::Playlist,
            id: self.id.clone(),
            title: self.title.clone(),
            lines: vec![count(self.items.len())],
            thumbnail: self.items.first().map(Video::thumbnail_url),
            endpoint: Some(Endpoint::Browse { browse_id: self.id.clone(), params: None }),
            ..Default::default()
        }
    }
}

/// A fresh list id: the prefix and 16 random hexadecimal digits.
fn new_id() -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    format!("{ID_PREFIX}{}", &u[..16])
}

/// All lists, in display order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Store {
    #[serde(default = "store_version")]
    pub version: u32,
    #[serde(default)]
    pub lists: Vec<List>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

fn store_version() -> u32 {
    STORE_VERSION
}

impl Default for Store {
    fn default() -> Self {
        Store { version: STORE_VERSION, lists: Vec::new(), path: None }
    }
}

impl Store {
    /// `local_playlists.json` in the data dir. A missing or damaged file gives no lists.
    pub fn load() -> Store {
        Self::load_from(&crate::paths::data_file(FILE_NAME))
    }

    pub fn load_from(path: &Path) -> Store {
        let mut s: Store = crate::fsutil::load_json_or_default(path);
        for l in &mut s.lists {
            l.items.truncate(MAX_ITEMS);
        }
        s.path = Some(path.to_path_buf());
        s
    }

    /// Where [`save`](Self::save) writes.
    pub fn path(&self) -> PathBuf {
        self.path.clone().unwrap_or_else(|| crate::paths::data_file(FILE_NAME))
    }

    /// Writes back to where it was loaded from (the data dir by default).
    pub fn save(&self) -> Result<()> {
        crate::fsutil::save_json(&self.path(), self)
    }

    pub fn get(&self, id: &str) -> Option<&List> {
        self.lists.iter().find(|l| l.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut List> {
        self.lists.iter_mut().find(|l| l.id == id)
    }

    /// Makes an empty list at the end; returns its id. A blank title becomes "Playlist".
    pub fn create(&mut self, title: &str) -> String {
        let t = title.trim();
        let l = List::new(if t.is_empty() { "Playlist" } else { t });
        let id = l.id.clone();
        self.lists.push(l);
        id
    }

    pub fn rename(&mut self, id: &str, title: &str) -> bool {
        let t = title.trim();
        if t.is_empty() {
            return false;
        }
        self.get_mut(id).map(|l| l.title = t.to_string()).is_some()
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let n = self.lists.len();
        self.lists.retain(|l| l.id != id);
        n != self.lists.len()
    }

    /// Adds a video to a list (None when there is no such list).
    pub fn add(&mut self, id: &str, v: Video) -> Option<Added> {
        self.get_mut(id).map(|l| l.add(v))
    }

    pub fn remove_item(&mut self, id: &str, video_id: &str) -> bool {
        self.get_mut(id).is_some_and(|l| l.remove(video_id))
    }

    pub fn move_item(&mut self, id: &str, video_id: &str, to: Move) -> bool {
        self.get_mut(id).is_some_and(|l| l.move_item(video_id, to))
    }

    /// Ids of the lists holding `video_id` (the check marks of "Save to playlist").
    pub fn lists_of(&self, video_id: &str) -> Vec<String> {
        self.lists.iter().filter(|l| l.contains(video_id)).map(|l| l.id.clone()).collect()
    }

    /// Makes a list from `videos` (duplicates dropped, at most [`MAX_ITEMS`]); returns its id.
    pub fn create_with(&mut self, title: &str, videos: impl IntoIterator<Item = Video>) -> String {
        let id = self.create(title);
        if let Some(l) = self.get_mut(&id) {
            for v in videos {
                if l.add(v) == Added::Full {
                    break;
                }
            }
        }
        id
    }

    /// The Library's "On this TV" row: one card per list.
    pub fn shelf(&self, title: &str, count: &dyn Fn(usize) -> String) -> Shelf {
        Shelf { title: title.to_string(), items: self.lists.iter().map(|l| l.to_item(count)).collect(), ..Default::default() }
    }
}

/// The kept "Play next" queue (empty when none was kept).
pub fn load_queue() -> Vec<Video> {
    crate::fsutil::load_json_or_default(&crate::paths::data_file(QUEUE_FILE_NAME))
}

/// Keeps the "Play next" queue for the next run; an empty queue removes the file.
pub fn save_queue(q: &[Video]) -> Result<()> {
    let path = crate::paths::data_file(QUEUE_FILE_NAME);
    if q.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    crate::fsutil::save_json(&path, &q)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(id: &str) -> Video {
        Video { video_id: id.into(), title: format!("Title {id}"), channel: "Channel".into(), duration_secs: 75, thumbnail: None }
    }

    #[test]
    fn add_remove_and_dedupe() {
        let mut s = Store::default();
        let id = s.create("  Music  ");
        assert!(is_local_id(&id) && id.len() == ID_PREFIX.len() + 16);
        assert_eq!(s.get(&id).unwrap().title, "Music");
        assert_eq!(s.add(&id, v("a")), Some(Added::Added));
        assert_eq!(s.add(&id, v("b")), Some(Added::Added));
        assert_eq!(s.add(&id, v("a")), Some(Added::AlreadyThere));
        assert_eq!(s.add("tv-missing", v("c")), None);
        assert_eq!(s.get(&id).unwrap().video_ids(), ["a", "b"]);
        assert_eq!(s.lists_of("a"), [id.clone()]);
        assert!(s.remove_item(&id, "a"));
        assert!(!s.remove_item(&id, "a"));
        assert!(s.lists_of("a").is_empty());
        assert!(s.rename(&id, "Songs"));
        assert!(!s.rename(&id, "   "));
        assert_eq!(s.get(&id).unwrap().title, "Songs");
        assert!(s.remove(&id));
        assert!(s.lists.is_empty());
    }

    #[test]
    fn moves() {
        let mut l = List::new("x");
        for id in ["a", "b", "c", "d"] {
            l.add(v(id));
        }
        assert!(l.move_item("c", Move::Top));
        assert_eq!(l.video_ids(), ["c", "a", "b", "d"]);
        assert!(l.move_item("b", Move::Up));
        assert_eq!(l.video_ids(), ["c", "b", "a", "d"]);
        assert!(l.move_item("c", Move::Down));
        assert_eq!(l.video_ids(), ["b", "c", "a", "d"]);
        assert!(!l.move_item("d", Move::Down));
        assert!(!l.move_item("b", Move::Up));
        assert!(!l.move_item("b", Move::Top));
        assert!(!l.move_item("zz", Move::Up));
    }

    #[test]
    fn limit() {
        let mut l = List::new("big");
        for i in 0..MAX_ITEMS {
            assert_eq!(l.add(v(&i.to_string())), Added::Added);
        }
        assert_eq!(l.add(v("one more")), Added::Full);
        assert_eq!(l.add(v("0")), Added::AlreadyThere);
        assert_eq!(l.items.len(), MAX_ITEMS);
        let mut s = Store::default();
        let id = s.create_with("copy", (0..MAX_ITEMS + 10).map(|i| v(&i.to_string())));
        assert_eq!(s.get(&id).unwrap().items.len(), MAX_ITEMS);
    }

    #[test]
    fn roundtrip_and_damaged_file() {
        let d = crate::fsutil::temp_dir("local-playlists");
        let p = d.join(FILE_NAME);
        let mut s = Store::load_from(&p);
        let id = s.create("Kept");
        s.add(&id, v("a"));
        s.save().unwrap();
        let back = Store::load_from(&p);
        assert_eq!(back.lists, s.lists);
        std::fs::write(&p, b"{broken").unwrap();
        assert!(Store::load_from(&p).lists.is_empty());
        assert!(crate::fsutil::bad_path(&p).exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn cards() {
        let mut l = List::new("x");
        l.add(v("a"));
        let it = l.items[0].to_item();
        assert_eq!(it.badge.as_deref(), Some("1:15"));
        assert_eq!(it.thumbnail.as_deref(), Some("https://i.ytimg.com/vi/a/hqdefault.jpg"));
        assert_eq!(l.to_page().item_count(), 1);
        let card = l.to_item(&|n| format!("{n} videos"));
        assert_eq!(card.lines, ["1 videos"]);
        assert!(matches!(card.endpoint, Some(Endpoint::Browse { ref browse_id, .. }) if browse_id == &l.id));
        assert_eq!(duration_text(3723), "1:02:03");
    }
}

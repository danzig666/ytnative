//! Local channel groups (SmartTube's "channel groups"): user-made lists of channels, kept on the
//! device in `channel_groups.json`, each with a feed built from YouTube's public RSS feeds. No
//! sign-in is needed, so groups also work for signed-out users and as a subscriptions replacement.
//!
//! - [`Store`]: load/save + CRUD on groups and their channels.
//! - [`group_feed`]: fetches every channel's RSS feed in parallel (with a per-channel cache, see
//!   [`FeedCache`]) and merges the videos newest-first into a [`Page`].
//! - [`parse_import`] / [`import_file`]: groups from NewPipe, Grayjay and PocketTube exports.

use crate::model::{Endpoint, Item, ItemKind, Page, Shelf};
use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const FILE_NAME: &str = "channel_groups.json";
const STORE_VERSION: u32 = 1;
pub const RSS_URL: &str = "https://www.youtube.com/feeds/videos.xml?channel_id=";
/// Channels fetched at once by [`group_feed`].
pub const MAX_PARALLEL: usize = 8;
/// Like SmartTube: very large groups make the feed slow; only the first N channels are fetched.
pub const MAX_CHANNELS_PER_FEED: usize = 150;
/// Items kept in a merged group feed.
pub const MAX_FEED_ITEMS: usize = 300;
/// Default per-channel RSS cache lifetime of [`FeedCache::global`].
pub const DEFAULT_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelRef {
    /// "UC..." channel id.
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub icon: Option<String>,
}

impl ChannelRef {
    pub fn new(id: impl Into<String>) -> Self {
        ChannelRef { id: id.into(), ..Default::default() }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub icon_url: Option<String>,
    #[serde(default)]
    pub channels: Vec<ChannelRef>,
}

impl Group {
    pub fn new(title: impl Into<String>) -> Self {
        Group { id: uuid::Uuid::new_v4().to_string(), title: title.into(), icon_url: None, channels: Vec::new() }
    }

    pub fn contains(&self, channel_id: &str) -> bool {
        self.channels.iter().any(|c| c.id == channel_id)
    }

    /// Adds a channel, or fills in a missing title/icon if it is already there. Returns true if
    /// anything changed.
    pub fn add_channel(&mut self, c: ChannelRef) -> bool {
        if let Some(e) = self.channels.iter_mut().find(|e| e.id == c.id) {
            let mut changed = false;
            if e.title.is_empty() && !c.title.is_empty() {
                e.title = c.title;
                changed = true;
            }
            if e.icon.is_none() && c.icon.is_some() {
                e.icon = c.icon;
                changed = true;
            }
            return changed;
        }
        if c.id.is_empty() {
            return false;
        }
        self.channels.push(c);
        true
    }

    pub fn remove_channel(&mut self, channel_id: &str) -> bool {
        let n = self.channels.len();
        self.channels.retain(|c| c.id != channel_id);
        n != self.channels.len()
    }

    pub fn channel_ids(&self) -> Vec<String> {
        self.channels.iter().map(|c| c.id.clone()).collect()
    }
}

/// All groups, in display order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Store {
    #[serde(default = "store_version")]
    pub version: u32,
    #[serde(default)]
    pub groups: Vec<Group>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

fn store_version() -> u32 {
    STORE_VERSION
}

impl Default for Store {
    fn default() -> Self {
        Store { version: STORE_VERSION, groups: Vec::new(), path: None }
    }
}

impl Store {
    /// `channel_groups.json` in the data dir. Missing/corrupt -> empty.
    pub fn load() -> Store {
        Self::load_from(&crate::paths::data_file(FILE_NAME))
    }

    pub fn load_from(path: &Path) -> Store {
        let mut s: Store = crate::fsutil::load_json_or_default(path);
        s.path = Some(path.to_path_buf());
        s
    }

    /// Writes back to where it was loaded from (the data dir by default).
    pub fn save(&self) -> Result<()> {
        let path = self.path.clone().unwrap_or_else(|| crate::paths::data_file(FILE_NAME));
        crate::fsutil::save_json(&path, self)
    }

    pub fn get(&self, id: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Group> {
        self.groups.iter_mut().find(|g| g.id == id)
    }

    pub fn find_by_title(&self, title: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.title.eq_ignore_ascii_case(title))
    }

    /// Creates an empty group at the end; returns its id.
    pub fn create(&mut self, title: &str) -> String {
        let g = Group::new(title.trim());
        let id = g.id.clone();
        self.groups.push(g);
        id
    }

    pub fn rename(&mut self, id: &str, title: &str) -> bool {
        self.get_mut(id).map(|g| g.title = title.trim().to_string()).is_some()
    }

    pub fn set_icon(&mut self, id: &str, icon_url: Option<String>) -> bool {
        self.get_mut(id).map(|g| g.icon_url = icon_url).is_some()
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let n = self.groups.len();
        self.groups.retain(|g| g.id != id);
        n != self.groups.len()
    }

    /// Moves group `id` to position `to` (clamped).
    pub fn move_to(&mut self, id: &str, to: usize) -> bool {
        let Some(i) = self.groups.iter().position(|g| g.id == id) else { return false };
        let g = self.groups.remove(i);
        self.groups.insert(to.min(self.groups.len()), g);
        true
    }

    pub fn add_channel(&mut self, group_id: &str, c: ChannelRef) -> bool {
        self.get_mut(group_id).map(|g| g.add_channel(c)).unwrap_or(false)
    }

    pub fn remove_channel(&mut self, group_id: &str, channel_id: &str) -> bool {
        self.get_mut(group_id).map(|g| g.remove_channel(channel_id)).unwrap_or(false)
    }

    /// Ids of the groups containing `channel_id` (for a "Add to group" checklist).
    pub fn groups_of(&self, channel_id: &str) -> Vec<String> {
        self.groups.iter().filter(|g| g.contains(channel_id)).map(|g| g.id.clone()).collect()
    }

    /// Fills empty channel titles from feeds (e.g. after an import that had ids only).
    pub fn fill_titles(&mut self, titles: &HashMap<String, String>) -> usize {
        let mut n = 0;
        for c in self.groups.iter_mut().flat_map(|g| g.channels.iter_mut()) {
            if c.title.is_empty() {
                if let Some(t) = titles.get(&c.id).filter(|t| !t.is_empty()) {
                    c.title = t.clone();
                    n += 1;
                }
            }
        }
        n
    }

    /// Adds imported groups. A group whose title matches an existing one (case-insensitive) is
    /// merged into it; others are appended. Returns (groups added, channels added).
    pub fn merge(&mut self, imported: Vec<Group>) -> (usize, usize) {
        let (mut groups, mut channels) = (0, 0);
        for g in imported {
            if let Some(existing) = self.groups.iter_mut().find(|e| e.title.eq_ignore_ascii_case(&g.title)) {
                for c in g.channels {
                    let new = !existing.contains(&c.id);
                    if existing.add_channel(c) && new {
                        channels += 1;
                    }
                }
                if existing.icon_url.is_none() {
                    existing.icon_url = g.icon_url;
                }
            } else {
                let mut g = g;
                if g.id.is_empty() || self.get(&g.id).is_some() {
                    g.id = uuid::Uuid::new_v4().to_string();
                }
                channels += g.channels.len();
                groups += 1;
                self.groups.push(g);
            }
        }
        (groups, channels)
    }
}

// ---------------------------------------------------------------------------------------------
// Importers

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportFormat {
    /// NewPipe "Export subscriptions" (`{"subscriptions":[{"service_id":0,"url":..,"name":..}]}`).
    NewPipe,
    /// Grayjay subscription groups: a JSON array of (stringified) `{id,name,image:{url},urls}`.
    Grayjay,
    /// PocketTube export: `{"ysc_collection":{name:..}, "<name>":["UC..",..], ...}`.
    PocketTube,
}

fn channel_id_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|/channel/)(UC[0-9A-Za-z_-]{22})(?:$|[/?#])").unwrap())
}

/// "UC..." from a channel URL (`.../channel/UC...`) or a bare id. Handles (`/@name`) and
/// non-YouTube URLs yield None.
pub fn channel_id_from_url(s: &str) -> Option<String> {
    channel_id_re().captures(s.trim()).map(|c| c[1].to_string())
}

/// Guesses the export format from its content.
pub fn detect_format(v: &Value) -> Option<ImportFormat> {
    if v.is_array() {
        Some(ImportFormat::Grayjay)
    } else if v.get("subscriptions").is_some_and(Value::is_array) {
        Some(ImportFormat::NewPipe)
    } else if v.get("ysc_collection").is_some_and(Value::is_object) {
        Some(ImportFormat::PocketTube)
    } else {
        None
    }
}

/// Parses an export of any supported format into groups (with fresh ids unless the export has
/// its own). Channels that are not YouTube `/channel/UC...` URLs are skipped.
pub fn parse_import(content: &str) -> Result<(ImportFormat, Vec<Group>)> {
    let v: Value = serde_json::from_str(content.trim_start_matches('\u{feff}')).context("not a JSON file")?;
    let fmt = detect_format(&v).context("unknown export format (expected NewPipe, Grayjay or PocketTube)")?;
    let groups = match fmt {
        ImportFormat::NewPipe => import_newpipe(&v),
        ImportFormat::Grayjay => import_grayjay(&v)?,
        ImportFormat::PocketTube => import_pockettube(&v),
    };
    Ok((fmt, groups))
}

pub fn import_file(path: &Path) -> Result<(ImportFormat, Vec<Group>)> {
    let s = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse_import(&s)
}

fn import_newpipe(v: &Value) -> Vec<Group> {
    let mut g = Group::new("NewPipe subscriptions");
    for s in v["subscriptions"].as_array().into_iter().flatten() {
        // service_id 0 is YouTube; other services (SoundCloud, PeerTube...) are skipped.
        if s.get("service_id").and_then(Value::as_i64).unwrap_or(0) != 0 {
            continue;
        }
        if let Some(id) = s["url"].as_str().and_then(channel_id_from_url) {
            g.add_channel(ChannelRef { id, title: s["name"].as_str().unwrap_or_default().to_string(), icon: None });
        }
    }
    vec![g]
}

fn import_grayjay(v: &Value) -> Result<Vec<Group>> {
    let mut out = Vec::new();
    for e in v.as_array().into_iter().flatten() {
        // Grayjay stores each group as a JSON string inside the array.
        let obj: Value = match e {
            Value::String(s) => serde_json::from_str(s).context("bad Grayjay group entry")?,
            Value::Object(_) => e.clone(),
            _ => continue,
        };
        let title = obj["name"].as_str().unwrap_or("Grayjay group").to_string();
        let mut g = Group::new(title);
        if let Some(id) = obj["id"].as_str().filter(|s| !s.is_empty()) {
            g.id = id.to_string();
        }
        g.icon_url = obj["image"]["url"].as_str().map(str::to_string);
        for u in obj["urls"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if let Some(id) = channel_id_from_url(u) {
                g.add_channel(ChannelRef::new(id));
            }
        }
        out.push(g);
    }
    Ok(out)
}

fn import_pockettube(v: &Value) -> Vec<Group> {
    let mut out = Vec::new();
    for name in v["ysc_collection"].as_object().into_iter().flat_map(|m| m.keys()) {
        let mut g = Group::new(name.clone());
        for id in v[name.as_str()].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if let Some(id) = channel_id_from_url(id) {
                g.add_channel(ChannelRef::new(id));
            }
        }
        out.push(g);
    }
    out
}

// ---------------------------------------------------------------------------------------------
// RSS feeds

/// One video from a channel's RSS feed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FeedEntry {
    pub video_id: String,
    pub title: String,
    pub channel_id: String,
    pub channel_title: String,
    /// Unix seconds.
    pub published: u64,
    pub thumbnail: Option<String>,
    pub views: Option<u64>,
    /// The feed links it as `/shorts/ID`.
    pub short: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChannelFeed {
    pub channel_id: String,
    pub title: String,
    /// Newest first, as YouTube sends them (up to 15).
    pub entries: Vec<FeedEntry>,
}

fn xml_unescape(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix("<![CDATA[").and_then(|s| s.strip_suffix("]]>")).unwrap_or(s);
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|&e| e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..end];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32)
            }
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Text of the first `<tag ...>text</tag>` in `s`.
fn tag_text(s: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let mut from = 0;
    while let Some(i) = s[from..].find(&open) {
        let start = from + i + open.len();
        // Make sure it is `<tag>` / `<tag attr..>`, not `<tagOther>`.
        let next = s[start..].chars().next()?;
        if next == '>' || next.is_whitespace() {
            let gt = start + s[start..].find('>')?;
            if s[..gt].ends_with('/') {
                return Some(String::new());
            }
            let close = format!("</{tag}>");
            let end = gt + 1 + s[gt + 1..].find(&close)?;
            return Some(xml_unescape(&s[gt + 1..end]));
        }
        from = start;
    }
    None
}

/// Attribute `attr` of the first `<tag ...>` in `s`.
fn tag_attr(s: &str, tag: &str, attr: &str) -> Option<String> {
    let open = format!("<{tag}");
    let mut from = 0;
    while let Some(i) = s[from..].find(&open) {
        let start = from + i + open.len();
        let next = s[start..].chars().next()?;
        if next == '>' || next == '/' || next.is_whitespace() {
            let el = &s[start..start + s[start..].find('>')?];
            for q in ['"', '\''] {
                let key = format!("{attr}={q}");
                let mut p = 0;
                while let Some(k) = el[p..].find(&key) {
                    let ks = p + k;
                    // Whole attribute name only.
                    if ks == 0 || el.as_bytes()[ks - 1].is_ascii_whitespace() {
                        let vs = ks + key.len();
                        let ve = vs + el[vs..].find(q)?;
                        return Some(xml_unescape(&el[vs..ve]));
                    }
                    p = ks + key.len();
                }
            }
            return None;
        }
        from = start;
    }
    None
}

/// "2024-05-01T12:34:56+00:00" / "...Z" / "...000+02:00" -> unix seconds.
pub fn parse_rfc3339(s: &str) -> Option<u64> {
    let s = s.trim();
    let num = |a: usize, b: usize| -> Option<i64> { s.get(a..b)?.parse().ok() };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)? as u32, num(8, 10)? as u32);
    let (h, mi, se) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let mut t = crate::fsutil::days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se;
    // Skip fractional seconds, then apply the offset.
    let tail = s.get(19..)?;
    let tail = tail.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    if let Some(sign) = tail.chars().next().filter(|c| *c == '+' || *c == '-') {
        let oh: i64 = tail.get(1..3)?.parse().ok()?;
        let om: i64 = tail.get(4..6).and_then(|m| m.parse().ok()).unwrap_or(0);
        let off = oh * 3600 + om * 60;
        t -= if sign == '+' { off } else { -off };
    }
    u64::try_from(t).ok()
}

/// Parses a YouTube channel RSS (Atom) document.
pub fn parse_feed(xml: &str, channel_id: &str) -> Result<ChannelFeed> {
    if !xml.contains("<feed") {
        bail!("not an Atom feed");
    }
    let head_end = xml.find("<entry").unwrap_or(xml.len());
    let head = &xml[..head_end];
    let title = tag_text(head, "title").or_else(|| tag_text(head, "name")).unwrap_or_default();
    let mut entries = Vec::new();
    for chunk in xml.split("<entry").skip(1) {
        let e = match chunk.find("</entry>") {
            Some(i) => &chunk[..i],
            None => chunk,
        };
        let Some(video_id) = tag_text(e, "yt:videoId").filter(|v| !v.is_empty()) else { continue };
        let link = tag_attr(e, "link", "href").unwrap_or_default();
        entries.push(FeedEntry {
            title: tag_text(e, "title").or_else(|| tag_text(e, "media:title")).unwrap_or_default(),
            channel_id: tag_text(e, "yt:channelId")
                .filter(|c| c.starts_with("UC"))
                .unwrap_or_else(|| channel_id.to_string()),
            channel_title: tag_text(e, "name").unwrap_or_else(|| title.clone()),
            published: tag_text(e, "published").and_then(|p| parse_rfc3339(&p)).unwrap_or(0),
            thumbnail: tag_attr(e, "media:thumbnail", "url"),
            views: tag_attr(e, "media:statistics", "views").and_then(|v| v.parse().ok()),
            short: link.contains("/shorts/"),
            video_id,
        });
    }
    Ok(ChannelFeed { channel_id: channel_id.to_string(), title, entries })
}

/// Downloads and parses one channel's feed (no cache).
pub fn fetch_channel(http: &reqwest::blocking::Client, channel_id: &str) -> Result<ChannelFeed> {
    let xml = http
        .get(format!("{RSS_URL}{channel_id}"))
        .timeout(Duration::from_secs(15))
        .send()?
        .error_for_status()
        .with_context(|| format!("RSS for {channel_id}"))?
        .text()?;
    parse_feed(&xml, channel_id)
}

/// Per-channel feed cache shared by all group feeds, so switching between groups that share
/// channels, or reopening one, does not refetch within the TTL.
pub struct FeedCache {
    ttl: Duration,
    map: Mutex<HashMap<String, (Instant, Arc<ChannelFeed>)>>,
}

impl FeedCache {
    pub fn new(ttl: Duration) -> Self {
        FeedCache { ttl, map: Mutex::new(HashMap::new()) }
    }

    /// Process-wide cache with [`DEFAULT_TTL`].
    pub fn global() -> &'static FeedCache {
        static C: OnceLock<FeedCache> = OnceLock::new();
        C.get_or_init(|| FeedCache::new(DEFAULT_TTL))
    }

    /// A cached feed younger than the TTL.
    pub fn fresh(&self, channel_id: &str) -> Option<Arc<ChannelFeed>> {
        let m = self.map.lock().unwrap();
        m.get(channel_id).filter(|(t, _)| t.elapsed() < self.ttl).map(|(_, f)| f.clone())
    }

    /// A cached feed of any age (fallback when a refetch fails).
    pub fn any(&self, channel_id: &str) -> Option<Arc<ChannelFeed>> {
        self.map.lock().unwrap().get(channel_id).map(|(_, f)| f.clone())
    }

    pub fn put(&self, feed: ChannelFeed) -> Arc<ChannelFeed> {
        let f = Arc::new(feed);
        self.map.lock().unwrap().insert(f.channel_id.clone(), (Instant::now(), f.clone()));
        f
    }

    pub fn invalidate(&self, channel_id: &str) {
        self.map.lock().unwrap().remove(channel_id);
    }

    pub fn clear(&self) {
        self.map.lock().unwrap().clear();
    }

    /// Channel titles of everything cached (for [`Store::fill_titles`]).
    pub fn titles(&self) -> HashMap<String, String> {
        self.map.lock().unwrap().iter().map(|(k, (_, f))| (k.clone(), f.title.clone())).collect()
    }
}

/// Feeds of `channel_ids`, from the cache when fresh, else fetched with up to [`MAX_PARALLEL`]
/// threads. A failed fetch falls back to a stale cached copy. Order follows `channel_ids`.
pub fn fetch_feeds(
    http: &reqwest::blocking::Client,
    channel_ids: &[String],
    cache: &FeedCache,
) -> Vec<(String, Result<Arc<ChannelFeed>>)> {
    let fetch = |id: &str| -> Result<Arc<ChannelFeed>> {
        if let Some(f) = cache.fresh(id) {
            return Ok(f);
        }
        match fetch_channel(http, id) {
            Ok(f) => Ok(cache.put(f)),
            Err(e) => cache.any(id).ok_or(e),
        }
    };
    let results: Vec<Mutex<Option<Result<Arc<ChannelFeed>>>>> = channel_ids.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..MAX_PARALLEL.min(channel_ids.len()) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(id) = channel_ids.get(i) else { break };
                *results[i].lock().unwrap() = Some(fetch(id));
            });
        }
    });
    channel_ids
        .iter()
        .cloned()
        .zip(results.into_iter().map(|m| m.into_inner().unwrap().unwrap_or_else(|| Err(anyhow::anyhow!("not fetched")))))
        .collect()
}

fn compact_count(n: u64) -> String {
    let f = |v: f64, suf: &str| {
        if v < 10.0 {
            let s = format!("{v:.1}");
            format!("{}{suf}", s.trim_end_matches(".0"))
        } else {
            format!("{}{suf}", v as u64)
        }
    };
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => f(n as f64 / 1e3, "K"),
        1_000_000..=999_999_999 => f(n as f64 / 1e6, "M"),
        _ => f(n as f64 / 1e9, "B"),
    }
}

/// "3 hours ago" style age of `t` relative to `now` (unix seconds).
pub fn relative_age(t: u64, now: u64) -> String {
    let d = now.saturating_sub(t);
    let (n, unit) = match d {
        0..=59 => (d, "second"),
        60..=3599 => (d / 60, "minute"),
        3600..=86_399 => (d / 3600, "hour"),
        86_400..=604_799 => (d / 86_400, "day"),
        604_800..=2_591_999 => (d / 604_800, "week"),
        2_592_000..=31_535_999 => (d / 2_592_000, "month"),
        _ => (d / 31_536_000, "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

/// A feed entry as a tile.
pub fn entry_item(e: &FeedEntry, now: u64) -> Item {
    let mut meta = Vec::new();
    if let Some(v) = e.views {
        meta.push(format!("{} view{}", compact_count(v), if v == 1 { "" } else { "s" }));
    }
    if e.published > 0 {
        meta.push(relative_age(e.published, now));
    }
    let mut lines = vec![e.channel_title.clone()];
    if !meta.is_empty() {
        lines.push(meta.join(" \u{2022} "));
    }
    Item {
        kind: if e.short { ItemKind::Short } else { ItemKind::Video },
        id: e.video_id.clone(),
        title: e.title.clone(),
        lines,
        channel: Some(e.channel_title.clone()).filter(|c| !c.is_empty()),
        thumbnail: Some(
            e.thumbnail.clone().unwrap_or_else(|| format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", e.video_id)),
        ),
        badge: e.short.then(|| "SHORTS".to_string()),
        progress: None,
        endpoint: Some(Endpoint::Watch { video_id: e.video_id.clone(), playlist_id: None, params: None }),
        menu: Vec::new(),
    }
}

/// Merges feeds newest-first (deduplicated by video id) into a one-shelf page.
pub fn merge_feeds<'a>(title: &str, feeds: impl IntoIterator<Item = &'a ChannelFeed>, now: u64) -> Page {
    let mut seen = HashSet::new();
    let mut entries: Vec<&FeedEntry> =
        feeds.into_iter().flat_map(|f| f.entries.iter()).filter(|e| seen.insert(e.video_id.as_str())).collect();
    entries.sort_by(|a, b| b.published.cmp(&a.published));
    entries.truncate(MAX_FEED_ITEMS);
    Page {
        title: Some(title.to_string()),
        shelves: vec![Shelf {
            title: title.to_string(),
            items: entries.into_iter().map(|e| entry_item(e, now)).collect(),
            continuation: None,
            endpoint: None,
        }],
        tabs: Vec::new(),
        continuation: None,
    }
}

/// The group's feed: every channel's latest uploads, newest first. Fails only when no channel
/// could be loaded at all (an empty group gives an empty page).
pub fn group_feed(http: &reqwest::blocking::Client, group: &Group, cache: &FeedCache) -> Result<Page> {
    let ids: Vec<String> = group.channel_ids().into_iter().take(MAX_CHANNELS_PER_FEED).collect();
    let results = fetch_feeds(http, &ids, cache);
    let mut feeds = Vec::new();
    let mut last_err = None;
    for (_, r) in results {
        match r {
            Ok(f) => feeds.push(f),
            Err(e) => last_err = Some(e),
        }
    }
    if feeds.is_empty() {
        if let Some(e) = last_err {
            return Err(e.context(format!("no feed of group \"{}\" could be loaded", group.title)));
        }
    }
    Ok(merge_feeds(&group.title, feeds.iter().map(|f| f.as_ref()), crate::fsutil::now_secs()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns:yt="http://www.youtube.com/xml/schemas/2015" xmlns:media="http://search.yahoo.com/mrss/" xmlns="http://www.w3.org/2005/Atom">
 <link rel="self" href="http://www.youtube.com/feeds/videos.xml?channel_id=UCaaaaaaaaaaaaaaaaaaaaaa"/>
 <id>yt:channel:aaaaaaaaaaaaaaaaaaaaaa</id>
 <yt:channelId>aaaaaaaaaaaaaaaaaaaaaa</yt:channelId>
 <title>Tom &amp; Jerry</title>
 <author><name>Tom &amp; Jerry</name><uri>https://www.youtube.com/channel/UCaaaaaaaaaaaaaaaaaaaaaa</uri></author>
 <published>2010-01-01T00:00:00+00:00</published>
 <entry>
  <id>yt:video:vid00000001</id>
  <yt:videoId>vid00000001</yt:videoId>
  <yt:channelId>UCaaaaaaaaaaaaaaaaaaaaaa</yt:channelId>
  <title>First &quot;one&quot; &#8211; ok</title>
  <link rel="alternate" href="https://www.youtube.com/watch?v=vid00000001"/>
  <author><name>Tom &amp; Jerry</name><uri>x</uri></author>
  <published>2024-05-02T10:00:00+00:00</published>
  <updated>2024-05-03T10:00:00+00:00</updated>
  <media:group>
   <media:title>First</media:title>
   <media:content url="https://www.youtube.com/v/vid00000001?version=3" type="application/x-shockwave-flash" width="640" height="390"/>
   <media:thumbnail url="https://i1.ytimg.com/vi/vid00000001/hqdefault.jpg" width="480" height="360"/>
   <media:description>desc</media:description>
   <media:community>
    <media:starRating count="10" average="5.00" min="1" max="5"/>
    <media:statistics views="12345"/>
   </media:community>
  </media:group>
 </entry>
 <entry>
  <id>yt:video:vid00000002</id>
  <yt:videoId>vid00000002</yt:videoId>
  <yt:channelId>UCaaaaaaaaaaaaaaaaaaaaaa</yt:channelId>
  <title>A short</title>
  <link rel="alternate" href="https://www.youtube.com/shorts/vid00000002"/>
  <author><name>Tom &amp; Jerry</name></author>
  <published>2024-05-01T12:00:00+02:00</published>
  <media:group>
   <media:thumbnail url="https://i2.ytimg.com/vi/vid00000002/hqdefault.jpg" width="480" height="360"/>
   <media:community><media:statistics views="1"/></media:community>
  </media:group>
 </entry>
</feed>"#;

    #[test]
    fn rss_parse() {
        let f = parse_feed(FEED, "UCaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        assert_eq!(f.title, "Tom & Jerry");
        assert_eq!(f.entries.len(), 2);
        let e = &f.entries[0];
        assert_eq!(e.video_id, "vid00000001");
        assert_eq!(e.title, "First \"one\" \u{2013} ok");
        assert_eq!(e.views, Some(12345));
        assert_eq!(e.published, parse_rfc3339("2024-05-02T10:00:00Z").unwrap());
        assert_eq!(e.thumbnail.as_deref(), Some("https://i1.ytimg.com/vi/vid00000001/hqdefault.jpg"));
        assert!(!e.short);
        assert!(f.entries[1].short);
        assert_eq!(f.entries[1].published, parse_rfc3339("2024-05-01T10:00:00Z").unwrap());
        assert!(parse_feed("<html>", "x").is_err());
    }

    #[test]
    fn dates_and_formatting() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00+00:00"), Some(0));
        assert_eq!(parse_rfc3339("2024-05-02T10:00:00.123Z"), Some(1_714_644_000));
        assert_eq!(parse_rfc3339("2024-05-02T12:30:00+02:30"), Some(1_714_644_000));
        assert_eq!(parse_rfc3339("2024-05-02T07:00:00-03:00"), Some(1_714_644_000));
        assert_eq!(compact_count(999), "999");
        assert_eq!(compact_count(1_234), "1.2K");
        assert_eq!(compact_count(12_345), "12K");
        assert_eq!(compact_count(1_000_000), "1M");
        assert_eq!(relative_age(100, 100 + 7200), "2 hours ago");
        assert_eq!(relative_age(100, 100 + 86_400), "1 day ago");
    }

    #[test]
    fn merge_newest_first() {
        let a = parse_feed(FEED, "UCaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let mut b = ChannelFeed { channel_id: "UCbbbbbbbbbbbbbbbbbbbbbb".into(), title: "B".into(), entries: vec![] };
        b.entries.push(FeedEntry {
            video_id: "vidB0000001".into(),
            title: "B new".into(),
            channel_title: "B".into(),
            published: parse_rfc3339("2024-06-01T00:00:00Z").unwrap(),
            ..Default::default()
        });
        b.entries.push(a.entries[0].clone()); // duplicate across channels is dropped
        let now = parse_rfc3339("2024-06-01T03:00:00Z").unwrap();
        let p = merge_feeds("G", [&a, &b], now);
        let ids: Vec<_> = p.shelves[0].items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["vidB0000001", "vid00000001", "vid00000002"]);
        let first = &p.shelves[0].items[0];
        assert_eq!(first.lines, ["B", "3 hours ago"]);
        assert_eq!(first.thumbnail.as_deref(), Some("https://i.ytimg.com/vi/vidB0000001/hqdefault.jpg"));
        assert!(matches!(&first.endpoint, Some(Endpoint::Watch { video_id, .. }) if video_id == "vidB0000001"));
        let second = &p.shelves[0].items[1];
        assert_eq!(second.lines[1], "12K views \u{2022} 4 weeks ago");
        assert_eq!(p.shelves[0].items[2].kind, ItemKind::Short);
    }

    #[test]
    fn cache_ttl() {
        let c = FeedCache::new(Duration::from_millis(0));
        c.put(ChannelFeed { channel_id: "UCx".into(), title: "X".into(), entries: vec![] });
        assert!(c.fresh("UCx").is_none());
        assert!(c.any("UCx").is_some());
        let c = FeedCache::new(Duration::from_secs(60));
        c.put(ChannelFeed { channel_id: "UCx".into(), title: "X".into(), entries: vec![] });
        assert!(c.fresh("UCx").is_some());
        assert_eq!(c.titles()["UCx"], "X");
        c.invalidate("UCx");
        assert!(c.any("UCx").is_none());
    }

    #[test]
    fn channel_ids() {
        let id = "UCbWcXB0PoqOsAvAdfzWMf0w";
        assert_eq!(channel_id_from_url(&format!("https://www.youtube.com/channel/{id}")).as_deref(), Some(id));
        assert_eq!(channel_id_from_url(&format!("http://youtube.com/channel/{id}/videos")).as_deref(), Some(id));
        assert_eq!(channel_id_from_url(id).as_deref(), Some(id));
        assert_eq!(channel_id_from_url("https://rumble.com/c/x"), None);
        assert_eq!(channel_id_from_url("https://www.youtube.com/@handle"), None);
    }

    #[test]
    fn import_newpipe_sample() {
        let s = r#"{"app_version":"0.27.2","app_version_int":999,"subscriptions":[
            {"service_id":0,"url":"https://www.youtube.com/channel/UCiifkYAs_bq1pt_zbNAzYGg","name":"DC"},
            {"service_id":0,"url":"https://www.youtube.com/channel/UC4-79UOlP48-QNGgCko5p2g","name":"Two"},
            {"service_id":1,"url":"https://soundcloud.com/x","name":"SC"},
            {"service_id":0,"url":"https://www.youtube.com/channel/UCiifkYAs_bq1pt_zbNAzYGg","name":"DC dup"}]}"#;
        let (fmt, g) = parse_import(s).unwrap();
        assert_eq!(fmt, ImportFormat::NewPipe);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].channels.len(), 2);
        assert_eq!(g[0].channels[0].title, "DC");
    }

    #[test]
    fn import_grayjay_sample() {
        let inner1 = r#"{"id":"029d7e31","name":"Internet","image":{"url":"https://img/x.jpg"},"urls":["https://www.youtube.com/channel/UCKEsuMNTAd1462h7ZtjM5aw","https://rumble.com/c/mad","lbry://@x#f39","https://www.youtube.com/channel/UCqdTuLTekIsdgaIKjwj4D-A"],"creationTime":1733456455}"#;
        let inner2 = r#"{"id":"792392dc","name":"Docu","image":null,"urls":["https://www.youtube.com/channel/UC73dVtWf9mpjiWYkXyIlm7A"]}"#;
        let s = serde_json::to_string(&vec![inner1, inner2]).unwrap();
        let (fmt, g) = parse_import(&s).unwrap();
        assert_eq!(fmt, ImportFormat::Grayjay);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].id, "029d7e31");
        assert_eq!(g[0].title, "Internet");
        assert_eq!(g[0].icon_url.as_deref(), Some("https://img/x.jpg"));
        assert_eq!(g[0].channel_ids(), ["UCKEsuMNTAd1462h7ZtjM5aw", "UCqdTuLTekIsdgaIKjwj4D-A"]);
        assert_eq!(g[1].icon_url, None);
        // Plain objects (not stringified) work too.
        let (_, g2) = parse_import(&format!("[{inner2}]")).unwrap();
        assert_eq!(g2[0].channels.len(), 1);
    }

    #[test]
    fn import_pockettube_sample() {
        let s = r#"{
            "test":["UCyJTz74gyxWU2I5BzP6D71Q","UCL5J5tawwI7TELw1fstIMpQ"],
            "test3 with spaces":["UCv19bD3h1dfYjVTj3v7R9yA"],
            "ysc_collection":{"test":"test","test3 with spaces":"test3 with spaces"},
            "ysc_meta":{"test":{"img":"/icon/new_pack/_52.png"}},
            "ysc_settings":{"lang":"en"},
            "ysc_subs_count":{"UC0zHw3DRqxCCG0byWSw4Bcg":{"sc":"307000"}}}"#;
        let (fmt, g) = parse_import(s).unwrap();
        assert_eq!(fmt, ImportFormat::PocketTube);
        assert_eq!(g.len(), 2);
        let t = g.iter().find(|g| g.title == "test").unwrap();
        assert_eq!(t.channels.len(), 2);
        assert!(g.iter().any(|g| g.title == "test3 with spaces" && g.channels.len() == 1));
        assert!(parse_import(r#"{"foo":1}"#).is_err());
        assert!(parse_import("not json").is_err());
    }

    #[test]
    fn store_crud_and_persist() {
        let dir = crate::fsutil::temp_dir("groups");
        let path = dir.join(FILE_NAME);
        let mut s = Store::load_from(&path);
        assert!(s.groups.is_empty());
        let a = s.create(" News ");
        let b = s.create("Music");
        assert_eq!(s.get(&a).unwrap().title, "News");
        assert!(s.add_channel(&a, ChannelRef { id: "UC1".into(), title: "One".into(), icon: None }));
        assert!(!s.add_channel(&a, ChannelRef::new("UC1")));
        assert!(s.add_channel(&a, ChannelRef { id: "UC1".into(), title: String::new(), icon: Some("i".into()) }));
        assert!(s.add_channel(&b, ChannelRef::new("UC1")));
        assert_eq!(s.groups_of("UC1"), [a.clone(), b.clone()]);
        assert!(s.rename(&b, "Tunes"));
        assert!(s.move_to(&b, 0));
        assert_eq!(s.groups[0].id, b);
        assert!(s.remove_channel(&b, "UC1"));
        s.save().unwrap();

        let mut s2 = Store::load_from(&path);
        assert_eq!(s2.groups.len(), 2);
        assert_eq!(s2.get(&a).unwrap().channels[0].icon.as_deref(), Some("i"));
        assert!(s2.remove(&b));
        assert!(!s2.remove(&b));

        let mut imp = Group::new("news");
        imp.add_channel(ChannelRef::new("UC1"));
        imp.add_channel(ChannelRef::new("UC2"));
        let mut other = Group::new("Other");
        other.id = a.clone(); // id clash gets a fresh id
        other.add_channel(ChannelRef::new("UC3"));
        assert_eq!(s2.merge(vec![imp, other]), (1, 2));
        assert_eq!(s2.get(&a).unwrap().channels.len(), 2);
        assert_eq!(s2.groups.len(), 2);
        assert_ne!(s2.groups[1].id, a);

        let titles = HashMap::from([("UC2".to_string(), "Two".to_string())]);
        assert_eq!(s2.fill_titles(&titles), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore]
    fn live_rss_group_feed() {
        let http = reqwest::blocking::Client::new();
        let mut g = Group::new("Live");
        // YouTube's own channels.
        g.add_channel(ChannelRef::new("UCBR8-60-B28hp2BmDPdntcQ"));
        g.add_channel(ChannelRef::new("UC_x5XG1OV2P6uZZ5FSM9Ttw"));
        let cache = FeedCache::new(DEFAULT_TTL);
        let p = group_feed(&http, &g, &cache).unwrap();
        let items = &p.shelves[0].items;
        assert!(!items.is_empty());
        println!("{} items; first: {} | {:?}", items.len(), items[0].title, items[0].lines);
        assert!(cache.fresh("UCBR8-60-B28hp2BmDPdntcQ").is_some() || cache.fresh("UC_x5XG1OV2P6uZZ5FSM9Ttw").is_some());
    }
}

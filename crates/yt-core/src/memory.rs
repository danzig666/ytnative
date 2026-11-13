//! Small persisted maps the player consults (`player_memory.json`):
//! - per-channel playback speed and subtitle language ("remember for this channel"),
//! - per-video resume position for videos that are not in the account's watch history
//!   (signed out, history paused, or not yet synced),
//! - daily watch-time totals.
//!
//! Each map is size-limited and evicts the least recently used entries.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "player_memory.json";
pub const MAX_CHANNEL_SPEEDS: usize = 300;
pub const MAX_CHANNEL_SUBTITLES: usize = 300;
pub const MAX_VIDEO_SPEEDS: usize = 500;
pub const MAX_POSITIONS: usize = 500;
pub const MAX_WATCH_DAYS: usize = 90;
/// Positions this close to the start are not worth resuming.
pub const MIN_RESUME_SECS: u64 = 10;
/// Positions this close to the end count as finished (stored as 0: start over).
pub const END_MARGIN_SECS: u64 = 30;

/// A string-keyed map with a capacity, ordered least- to most-recently used (serialized as a
/// plain list, so the file stays readable).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LruMap<V> {
    entries: Vec<(String, V)>,
}

impl<V> Default for LruMap<V> {
    fn default() -> Self {
        LruMap { entries: Vec::new() }
    }
}

impl<V> LruMap<V> {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Looks up without touching the recency order.
    pub fn peek(&self, key: &str) -> Option<&V> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Looks up and marks the entry as most recently used.
    pub fn get(&mut self, key: &str) -> Option<&V> {
        let i = self.entries.iter().position(|(k, _)| k == key)?;
        let e = self.entries.remove(i);
        self.entries.push(e);
        self.entries.last().map(|(_, v)| v)
    }

    /// Inserts/replaces as most recently used, evicting the oldest beyond `cap`.
    pub fn insert(&mut self, key: &str, v: V, cap: usize) {
        self.entries.retain(|(k, _)| k != key);
        self.entries.push((key.to_string(), v));
        if self.entries.len() > cap {
            let extra = self.entries.len() - cap;
            self.entries.drain(..extra);
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<V> {
        let i = self.entries.iter().position(|(k, _)| k == key)?;
        Some(self.entries.remove(i).1)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Keys, most recently used first.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().rev().map(|(k, _)| k.as_str())
    }

    fn trim(&mut self, cap: usize) {
        if self.entries.len() > cap {
            let extra = self.entries.len() - cap;
            self.entries.drain(..extra);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub secs: u64,
    /// 0 when unknown.
    pub duration: u64,
    /// Unix seconds of the last update.
    pub at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlayerMemory {
    #[serde(default)]
    pub channel_speed: LruMap<f32>,
    #[serde(default)]
    pub channel_subtitle: LruMap<String>,
    /// Per-video playback speed ("remember speed per video").
    #[serde(default)]
    pub video_speed: LruMap<f32>,
    #[serde(default)]
    pub positions: LruMap<Position>,
    /// "YYYY-MM-DD" -> seconds watched that day (UTC).
    #[serde(default)]
    pub watch_time: LruMap<u64>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl PlayerMemory {
    pub fn load() -> Self {
        Self::load_from(&crate::paths::data_file(FILE_NAME))
    }

    pub fn load_from(path: &Path) -> Self {
        let mut m: PlayerMemory = crate::fsutil::load_json_or_default(path);
        m.channel_speed.trim(MAX_CHANNEL_SPEEDS);
        m.channel_subtitle.trim(MAX_CHANNEL_SUBTITLES);
        m.video_speed.trim(MAX_VIDEO_SPEEDS);
        m.positions.trim(MAX_POSITIONS);
        m.watch_time.trim(MAX_WATCH_DAYS);
        m.path = Some(path.to_path_buf());
        m
    }

    pub fn save(&self) -> Result<()> {
        let path = self.path.clone().unwrap_or_else(|| crate::paths::data_file(FILE_NAME));
        crate::fsutil::save_json(&path, self)
    }

    /// Remembered speed for a channel.
    pub fn channel_speed(&mut self, channel_id: &str) -> Option<f32> {
        self.channel_speed.get(channel_id).copied()
    }

    /// Remembers `speed` for a channel; `None` (or a non-positive/NaN speed) forgets it.
    pub fn set_channel_speed(&mut self, channel_id: &str, speed: Option<f32>) {
        match speed.filter(|s| s.is_finite() && *s > 0.0) {
            Some(s) => self.channel_speed.insert(channel_id, s, MAX_CHANNEL_SPEEDS),
            None => {
                self.channel_speed.remove(channel_id);
            }
        }
    }

    /// Remembered speed for a video.
    pub fn video_speed(&mut self, video_id: &str) -> Option<f32> {
        self.video_speed.get(video_id).copied()
    }

    /// Remembers `speed` for a video; `None` (or a non-positive/NaN speed) forgets it.
    pub fn set_video_speed(&mut self, video_id: &str, speed: Option<f32>) {
        match speed.filter(|s| s.is_finite() && *s > 0.0) {
            Some(s) => self.video_speed.insert(video_id, s, MAX_VIDEO_SPEEDS),
            None => {
                self.video_speed.remove(video_id);
            }
        }
    }

    /// Remembered subtitle language code ("en", "de", "" = subtitles off).
    pub fn channel_subtitle(&mut self, channel_id: &str) -> Option<String> {
        self.channel_subtitle.get(channel_id).cloned()
    }

    pub fn set_channel_subtitle(&mut self, channel_id: &str, lang: Option<&str>) {
        match lang {
            Some(l) => self.channel_subtitle.insert(channel_id, l.to_string(), MAX_CHANNEL_SUBTITLES),
            None => {
                self.channel_subtitle.remove(channel_id);
            }
        }
    }

    /// Where to resume `video_id`, if a useful position is stored.
    pub fn position(&mut self, video_id: &str) -> Option<u64> {
        self.positions.get(video_id).map(|p| p.secs)
    }

    /// Stores the playback position. A position near the start or the end (finished) is stored
    /// as 0, so the video starts over next time; keeping the entry (rather than removing it) means
    /// this app's own record still wins over YouTube's coarse watched-percentage.
    pub fn set_position(&mut self, video_id: &str, secs: u64, duration: u64) {
        let finished = duration > 0 && secs + END_MARGIN_SECS.min(duration / 10).max(1) >= duration;
        let secs = if secs < MIN_RESUME_SECS || finished { 0 } else { secs };
        let at = crate::fsutil::now_secs();
        self.positions.insert(video_id, Position { secs, duration, at }, MAX_POSITIONS);
    }

    pub fn forget_position(&mut self, video_id: &str) {
        self.positions.remove(video_id);
    }

    /// Adds `secs` of playback to today's (UTC) total.
    pub fn add_watch_time(&mut self, secs: u64) {
        self.add_watch_time_at(secs, crate::fsutil::now_secs());
    }

    pub fn add_watch_time_at(&mut self, secs: u64, now: u64) {
        let day = day_key(now);
        let total = self.watch_time.peek(&day).copied().unwrap_or(0) + secs;
        self.watch_time.insert(&day, total, MAX_WATCH_DAYS);
    }

    /// Seconds watched on the UTC day containing `now`.
    pub fn watch_time_on(&self, now: u64) -> u64 {
        self.watch_time.peek(&day_key(now)).copied().unwrap_or(0)
    }

    /// Seconds watched over the last `days` days (today included).
    pub fn watch_time_last_days(&self, days: u64, now: u64) -> u64 {
        (0..days).map(|d| self.watch_time_on(now.saturating_sub(d * 86_400))).sum()
    }
}

fn day_key(t: u64) -> String {
    let (y, m, d) = crate::fsutil::civil_from_days((t / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lru_eviction() {
        let mut m = LruMap::default();
        m.insert("a", 1, 2);
        m.insert("b", 2, 2);
        assert_eq!(m.get("a"), Some(&1)); // a becomes newest
        m.insert("c", 3, 2); // evicts b
        assert_eq!(m.peek("b"), None);
        assert_eq!(m.keys().collect::<Vec<_>>(), ["c", "a"]);
        m.insert("a", 9, 2);
        assert_eq!(m.len(), 2);
        assert_eq!(m.remove("a"), Some(9));
    }

    #[test]
    fn speeds_subs_positions() {
        let mut m = PlayerMemory::default();
        m.set_channel_speed("UC1", Some(1.5));
        assert_eq!(m.channel_speed("UC1"), Some(1.5));
        m.set_channel_speed("UC1", Some(f32::NAN));
        assert_eq!(m.channel_speed("UC1"), None);
        m.set_channel_subtitle("UC1", Some("de"));
        assert_eq!(m.channel_subtitle("UC1").as_deref(), Some("de"));
        m.set_channel_subtitle("UC1", None);
        assert_eq!(m.channel_subtitle("UC1"), None);

        m.set_position("v1", 120, 600);
        assert_eq!(m.position("v1"), Some(120));
        m.set_position("v1", 5, 600); // too early -> start over
        assert_eq!(m.position("v1"), Some(0));
        m.set_position("v1", 300, 600);
        m.set_position("v1", 590, 600); // finished -> start over
        assert_eq!(m.position("v1"), Some(0));
        m.set_position("live", 4000, 0); // unknown duration is kept
        assert_eq!(m.position("live"), Some(4000));
        for i in 0..(MAX_POSITIONS + 10) {
            m.set_position(&format!("x{i}"), 100, 1000);
        }
        assert_eq!(m.positions.len(), MAX_POSITIONS);
        assert_eq!(m.position("live"), None);
    }

    #[test]
    fn watch_time() {
        let mut m = PlayerMemory::default();
        let day = 1_700_000_000;
        m.add_watch_time_at(60, day);
        m.add_watch_time_at(30, day + 10);
        m.add_watch_time_at(100, day - 86_400);
        assert_eq!(m.watch_time_on(day), 90);
        assert_eq!(m.watch_time_last_days(2, day), 190);
        assert_eq!(m.watch_time_last_days(1, day), 90);
    }

    #[test]
    fn persist() {
        let d = crate::fsutil::temp_dir("memory");
        let p = d.join(FILE_NAME);
        let mut m = PlayerMemory::load_from(&p);
        m.set_channel_speed("UC1", Some(2.0));
        m.set_position("v", 50, 100);
        m.save().unwrap();
        let mut m2 = PlayerMemory::load_from(&p);
        assert_eq!(m2.channel_speed("UC1"), Some(2.0));
        assert_eq!(m2.position("v"), Some(50));
        std::fs::remove_dir_all(d).unwrap();
    }
}

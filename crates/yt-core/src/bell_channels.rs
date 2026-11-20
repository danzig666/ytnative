//! Channels whose new videos the Notifications section shows ("bell" channels), kept on this
//! device in `bell_channels.json`.
//!
//! YouTube's notifications inbox (`notification/get_notification_menu`) now answers HTTP 400
//! "Precondition check failed" to every client, so the section falls back to what SmartTube does
//! (MediaServiceCore's `NotificationStorage`): a local list of channels, filled when the viewer
//! sets a channel's bell to "All" (on YouTube, seen in the watch page, or with the player's
//! "Notify me of new videos from this channel" switch), and shown as the newest videos of their
//! RSS feeds (`Session::notifications`).
//!
//! The list is cached in memory after the first read (the player menu asks for every frame it
//! draws); every change goes through this module, which saves the file at once.

use crate::actions::NotificationOption;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const FILE_NAME: &str = "bell_channels.json";
/// The bell state YouTube calls "All": every new video notifies.
pub const STATE_ALL: &str = "SUBSCRIPTION_NOTIFICATION_STATE_ALL";
pub const STATE_OFF: &str = "SUBSCRIPTION_NOTIFICATION_STATE_OFF";

/// One channel of the list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BellChannel {
    pub id: String,
    /// The channel's name when it was added (for display); may be empty.
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BellChannels {
    /// In the order they were added.
    #[serde(default)]
    pub channels: Vec<BellChannel>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl BellChannels {
    pub fn load() -> Self {
        Self::load_from(&crate::paths::data_file(FILE_NAME))
    }

    pub fn load_from(path: &Path) -> Self {
        let mut s: BellChannels = crate::fsutil::load_json_or_default(path);
        // A hand-edited or damaged file: no blank ids, no duplicates.
        let mut seen = std::collections::HashSet::new();
        s.channels.retain(|c| !c.id.trim().is_empty() && seen.insert(c.id.clone()));
        s.path = Some(path.to_path_buf());
        s
    }

    pub fn save(&self) -> Result<()> {
        let path = self.path.clone().unwrap_or_else(|| crate::paths::data_file(FILE_NAME));
        crate::fsutil::save_json(&path, self)
    }

    pub fn contains(&self, channel_id: &str) -> bool {
        self.channels.iter().any(|c| c.id == channel_id)
    }

    /// Adds the channel (or updates its name). Returns true if the list changed.
    pub fn add(&mut self, channel_id: &str, name: &str) -> bool {
        if channel_id.trim().is_empty() {
            return false;
        }
        match self.channels.iter_mut().find(|c| c.id == channel_id) {
            Some(c) if name.is_empty() || c.name == name => false,
            Some(c) => {
                c.name = name.to_string();
                true
            }
            None => {
                self.channels.push(BellChannel { id: channel_id.to_string(), name: name.to_string() });
                true
            }
        }
    }

    /// Returns true if the channel was in the list.
    pub fn remove(&mut self, channel_id: &str) -> bool {
        let n = self.channels.len();
        self.channels.retain(|c| c.id != channel_id);
        n != self.channels.len()
    }

    pub fn ids(&self) -> Vec<String> {
        self.channels.iter().map(|c| c.id.clone()).collect()
    }
}

/// Whether a channel's bell, as YouTube reports it, means "in the list" (Some(true)), "not in
/// the list" (Some(false)), or says nothing (None: no bell choices and no subscription state).
/// Not subscribed: no notifications. Subscribed: "All" selected adds, any other selected choice
/// ("Personalized", "None") removes.
pub fn wanted(subscribed: Option<bool>, options: &[NotificationOption]) -> Option<bool> {
    if subscribed == Some(false) {
        return Some(false);
    }
    options.iter().find(|o| o.selected).map(|o| o.state == STATE_ALL)
}

/// The bell choice that sets "All" (on) or turns the bell off (off: "None", else the first
/// choice that is not "All", which is "Personalized").
pub fn option_for(options: &[NotificationOption], on: bool) -> Option<&NotificationOption> {
    if on {
        options.iter().find(|o| o.state == STATE_ALL)
    } else {
        options.iter().find(|o| o.state == STATE_OFF).or_else(|| options.iter().find(|o| o.state != STATE_ALL))
    }
}

static CACHE: Mutex<Option<BellChannels>> = Mutex::new(None);

fn with_cache<T>(f: impl FnOnce(&mut BellChannels) -> T) -> T {
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(BellChannels::load))
}

/// Whether the channel is in the list.
pub fn contains(channel_id: &str) -> bool {
    !channel_id.is_empty() && with_cache(|s| s.contains(channel_id))
}

/// Whether the list has no channel.
pub fn is_empty() -> bool {
    with_cache(|s| s.channels.is_empty())
}

/// The channel ids of the list.
pub fn ids() -> Vec<String> {
    with_cache(|s| s.ids())
}

/// Adds (on) or removes the channel and saves when that changed the list. Returns whether it did.
pub fn set(channel_id: &str, name: &str, on: bool) -> Result<bool> {
    with_cache(|s| {
        let changed = if on { s.add(channel_id, name) } else { s.remove(channel_id) };
        if changed {
            s.save()?;
        }
        Ok(changed)
    })
}

/// Brings the list in line with a watch page's subscribe button (see [`wanted`]).
pub fn sync(channel_id: &str, name: &str, subscribed: Option<bool>, options: &[NotificationOption]) -> Result<bool> {
    match wanted(subscribed, options) {
        Some(on) if !channel_id.is_empty() => set(channel_id, name, on),
        _ => Ok(false),
    }
}

/// Drops the in-memory copy, so the next use reads the file again (after a restore).
pub fn reload() {
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opt(state: &str, selected: bool) -> NotificationOption {
        NotificationOption { label: state.into(), state: state.into(), params: format!("P-{state}"), selected }
    }

    #[test]
    fn add_remove_contains() {
        let mut s = BellChannels::default();
        assert!(s.add("UC1", "One"));
        assert!(!s.add("UC1", "One"), "already there");
        assert!(!s.add("UC1", ""), "no name keeps the old one");
        assert!(s.add("UC1", "Renamed"));
        assert!(s.add("UC2", ""));
        assert!(!s.add(" ", "Blank"));
        assert!(s.contains("UC1") && s.contains("UC2"));
        assert_eq!(s.ids(), ["UC1", "UC2"]);
        assert_eq!(s.channels[0].name, "Renamed");
        assert!(s.remove("UC1"));
        assert!(!s.remove("UC1"));
        assert_eq!(s.ids(), ["UC2"]);
    }

    #[test]
    fn roundtrip_and_damaged_file() {
        let dir = std::env::temp_dir().join(format!("ytn-bell-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE_NAME);
        let mut s = BellChannels::load_from(&path);
        assert!(s.channels.is_empty());
        s.add("UC1", "One");
        s.save().unwrap();
        assert_eq!(BellChannels::load_from(&path).ids(), ["UC1"]);
        std::fs::write(&path, r#"{"channels":[{"id":"UC1"},{"id":""},{"id":"UC1","name":"x"},{"id":"UC2"}]}"#).unwrap();
        assert_eq!(BellChannels::load_from(&path).ids(), ["UC1", "UC2"]);
        std::fs::write(&path, "not json").unwrap();
        assert!(BellChannels::load_from(&path).channels.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bell_state_decides() {
        let all = [opt(STATE_ALL, true), opt("SUBSCRIPTION_NOTIFICATION_STATE_OCCASIONAL", false), opt(STATE_OFF, false)];
        let some = [opt(STATE_ALL, false), opt("SUBSCRIPTION_NOTIFICATION_STATE_OCCASIONAL", true)];
        let off = [opt(STATE_ALL, false), opt(STATE_OFF, true)];
        assert_eq!(wanted(Some(true), &all), Some(true));
        assert_eq!(wanted(None, &all), Some(true));
        assert_eq!(wanted(Some(true), &some), Some(false));
        assert_eq!(wanted(Some(true), &off), Some(false));
        assert_eq!(wanted(Some(false), &all), Some(false), "not subscribed");
        assert_eq!(wanted(Some(true), &[]), None, "nothing known");
        assert_eq!(wanted(None, &[]), None);
    }

    #[test]
    fn options_to_set() {
        let full = [opt(STATE_ALL, false), opt("SUBSCRIPTION_NOTIFICATION_STATE_OCCASIONAL", true), opt(STATE_OFF, false)];
        assert_eq!(option_for(&full, true).unwrap().state, STATE_ALL);
        assert_eq!(option_for(&full, false).unwrap().state, STATE_OFF);
        let two = [opt(STATE_ALL, true), opt("SUBSCRIPTION_NOTIFICATION_STATE_OCCASIONAL", false)];
        assert_eq!(option_for(&two, false).unwrap().state, "SUBSCRIPTION_NOTIFICATION_STATE_OCCASIONAL");
        assert!(option_for(&[], true).is_none());
    }
}

//! User settings (SmartTube's options that make sense on webOS), persisted as JSON in the app's
//! data directory. Unknown/missing fields fall back to defaults so old files keep loading.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Codec {
    Avc,
    Vp9,
    Av1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Repeat {
    /// Play the next suggestion when a video ends.
    Autoplay,
    /// Stop at the end.
    Off,
    /// Repeat the current video.
    One,
    /// Repeat the current playlist / queue.
    List,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SbAction {
    Off,
    Skip,
    SkipWithToast,
    Ask,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // Playback
    pub max_height: u32,
    pub min_height: u32,
    pub codec: Codec,
    pub speed: f32,
    pub remember_speed_per_channel: bool,
    pub speed_by_channel: std::collections::HashMap<String, f32>,
    pub seek_step_secs: u32,
    pub repeat: Repeat,
    pub resume_playback: bool,
    pub hud_timeout_secs: u32,
    pub show_clock: bool,
    pub show_ending_time: bool,
    // Subtitles
    pub subtitles: bool,
    pub subtitle_language: String,
    pub subtitle_auto_translate: bool,
    pub subtitle_size: u32,
    pub subtitle_bottom_offset: u32,
    /// "white" | "yellow" | "cyan".
    pub subtitle_color: String,
    /// "outline", "shadow" or "box".
    pub subtitle_style: String,
    /// Subtitle font: "default", "sans", "serif", "mono", "roboto".
    pub subtitle_font: String,
    /// Bold subtitles (off: the font's regular weight).
    pub subtitle_bold: bool,
    /// Background opacity 0..=100.
    pub subtitle_background: u32,
    // SponsorBlock / DeArrow / RYD
    pub sponsorblock: bool,
    pub sb_sponsor: SbAction,
    pub sb_intro: SbAction,
    pub sb_outro: SbAction,
    pub sb_interaction: SbAction,
    pub sb_selfpromo: SbAction,
    pub sb_music_offtopic: SbAction,
    pub sb_preview: SbAction,
    pub sb_filler: SbAction,
    pub sb_skip_once: bool,
    pub sb_markers: bool,
    pub dearrow_titles: bool,
    pub dearrow_thumbnails: bool,
    pub return_dislikes: bool,
    // Content filters
    pub hide_shorts_home: bool,
    pub hide_shorts_subscriptions: bool,
    pub hide_shorts_search: bool,
    pub hide_watched: bool,
    pub hide_upcoming: bool,
    pub hide_mixes: bool,
    pub blocked_channels: Vec<String>,
    // Interface
    pub sections: Vec<String>,
    /// Channels / playlists pinned to the sidebar: (title, browse id, params).
    pub pinned: Vec<(String, String, Option<String>)>,
    pub boot_section: String,
    pub idle_blank_secs: u32,
    pub time_24h: bool,
    /// "red" | "blue" | "green" | "purple" | "orange".
    pub accent: String,
    pub ui_language: String,
    pub content_country: String,
    // Remote
    pub lounge_enabled: bool,
    /// Title and channel under every thumbnail (SmartTube style).
    pub titles_under: bool,
    /// Screen size in percent (90..=100): shrinks the UI for TVs that overscan.
    pub ui_scale: u8,
    /// Card menu: item kinds in display order (see `screens::menu::CARD_ITEMS`) and the hidden ones.
    pub card_menu: Vec<String>,
    pub card_menu_hidden: Vec<String>,
    /// Cards per grid row / shelf: 3 (large), 4 (default) or 5 (small).
    pub card_columns: u8,
    /// Playback engine: "system" (engine A, the TV's DASH player) or "auto" (engine B for 4K / VP9 / AV1).
    pub engine: String,
    /// Actions of the red, green, yellow and blue remote buttons ("none", "search", "settings",
    /// "accounts", "subtitles", "speed").
    pub color_keys: [String; 4],
    /// Proxy for app traffic (http://, socks5://, socks5h://); empty = direct. Applies on restart.
    pub proxy_url: String,
    /// DNS-over-HTTPS: "off", "cloudflare", "google", "quad9". Applies on restart.
    pub doh: String,
    /// Answer DIAL discovery so the TV is in the phone's cast menu.
    pub dial_enabled: bool,
    pub device_name: String,
    // Parental
    pub child_mode: bool,
    pub settings_pin: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            max_height: 1080,
            min_height: 720,
            codec: Codec::Avc,
            speed: 1.0,
            remember_speed_per_channel: false,
            speed_by_channel: Default::default(),
            seek_step_secs: 10,
            repeat: Repeat::Autoplay,
            resume_playback: true,
            hud_timeout_secs: 4,
            show_clock: true,
            show_ending_time: true,
            subtitles: false,
            subtitle_language: "en".into(),
            subtitle_auto_translate: false,
            subtitle_size: 44,
            subtitle_style: "outline".into(),
            subtitle_font: "default".into(),
            subtitle_bold: false,
            subtitle_bottom_offset: 90,
            subtitle_color: "white".into(),
            subtitle_background: 72,
            sponsorblock: true,
            sb_sponsor: SbAction::SkipWithToast,
            sb_intro: SbAction::Off,
            sb_outro: SbAction::Off,
            sb_interaction: SbAction::Off,
            sb_selfpromo: SbAction::SkipWithToast,
            sb_music_offtopic: SbAction::Off,
            sb_preview: SbAction::Off,
            sb_filler: SbAction::Off,
            sb_skip_once: true,
            sb_markers: true,
            dearrow_titles: false,
            dearrow_thumbnails: false,
            return_dislikes: true,
            hide_shorts_home: false,
            hide_shorts_subscriptions: false,
            hide_shorts_search: false,
            hide_watched: false,
            hide_upcoming: false,
            hide_mixes: false,
            blocked_channels: Vec::new(),
            pinned: Vec::new(),
            // Trending is off by default: YouTube retired the page (browse answers HTTP 400).
            sections: crate::store::Section::ALL.iter().filter(|s| **s != crate::store::Section::Trending).map(|s| s.key().to_string()).collect(),
            boot_section: "home".into(),
            idle_blank_secs: 120,
            time_24h: true,
            accent: "red".into(),
            ui_language: "system".into(),
            content_country: "".into(),
            lounge_enabled: true,
            titles_under: true,
            ui_scale: 100,
            card_menu: crate::screens::menu::CARD_ITEMS.iter().map(|(k, _)| k.to_string()).collect(),
            card_menu_hidden: Vec::new(),
            card_columns: 4,
            engine: "system".into(),
            color_keys: ["none".into(), "subtitles".into(), "speed".into(), "search".into()],
            proxy_url: String::new(),
            doh: "off".into(),
            dial_enabled: true,
            device_name: "ytnative".into(),
            child_mode: false,
            settings_pin: String::new(),
        }
    }
}

impl Settings {
    fn path() -> std::path::PathBuf {
        yt_core::paths::data_file("settings.json")
    }

    pub fn load() -> Settings {
        std::fs::read_to_string(Self::path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let tmp = Self::path().with_extension("json.tmp");
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(&tmp, Self::path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_take_defaults() {
        let s: Settings = serde_json::from_str(r#"{"max_height": 2160}"#).unwrap();
        assert_eq!(s.max_height, 2160);
        assert_eq!(s.seek_step_secs, 10);
        assert!(s.sponsorblock);
    }
}

//! User settings (SmartTube's options that make sense on webOS), persisted as JSON in the app's
//! data directory. Unknown/missing fields fall back to defaults so old files keep loading.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Codec {
    Avc,
    Vp9,
    Av1,
}

/// What happens when a video ends (SmartTube's playback modes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaybackMode {
    /// YouTube's up next (the playlist's next item first).
    PlayNext,
    /// Repeat the current video.
    RepeatOne,
    /// The list (playlist / opened list) to its end, then stop.
    ListOnce,
    /// The list, starting over after its last item.
    ListRepeat,
    /// A random item of the list.
    Shuffle,
    /// The list backwards; stop at its first item.
    Reverse,
    /// Pause at the end and show the controls.
    PauseEach,
    /// Leave the player.
    CloseEach,
}

impl PlaybackMode {
    pub const ALL: &'static [PlaybackMode] = &[
        PlaybackMode::PlayNext,
        PlaybackMode::RepeatOne,
        PlaybackMode::ListOnce,
        PlaybackMode::ListRepeat,
        PlaybackMode::Shuffle,
        PlaybackMode::Reverse,
        PlaybackMode::PauseEach,
        PlaybackMode::CloseEach,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PlaybackMode::PlayNext => "Play next",
            PlaybackMode::RepeatOne => "Repeat this video",
            PlaybackMode::ListOnce => "Play the list then stop",
            PlaybackMode::ListRepeat => "Repeat the list",
            PlaybackMode::Shuffle => "Shuffle the list",
            PlaybackMode::Reverse => "Play the list in reverse",
            PlaybackMode::PauseEach => "Pause after each video",
            PlaybackMode::CloseEach => "Close the player after each video",
        }
    }
}

/// Which speed a video starts at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RememberSpeed {
    /// Always the default speed.
    Off,
    /// The last chosen speed becomes the default.
    All,
    PerVideo,
    PerChannel,
}

impl RememberSpeed {
    pub const ALL: &'static [RememberSpeed] = &[RememberSpeed::Off, RememberSpeed::All, RememberSpeed::PerVideo, RememberSpeed::PerChannel];

    pub fn label(self) -> &'static str {
        match self {
            RememberSpeed::Off => "Off",
            RememberSpeed::All => "Same for all videos",
            RememberSpeed::PerVideo => "Per video",
            RememberSpeed::PerChannel => "Per channel",
        }
    }
}

/// What OK does while a video plays with the controls hidden.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OkAction {
    Controls,
    ControlsAndPause,
    Pause,
    ToggleSpeed,
}

impl OkAction {
    pub const ALL: &'static [OkAction] = &[OkAction::Controls, OkAction::ControlsAndPause, OkAction::Pause, OkAction::ToggleSpeed];

    pub fn label(self) -> &'static str {
        match self {
            OkAction::Controls => "Show controls only",
            OkAction::ControlsAndPause => "Show controls and pause",
            OkAction::Pause => "Pause only",
            OkAction::ToggleSpeed => "Toggle speed",
        }
    }
}

/// The time shown at the right of the seek bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlsTime {
    Remaining,
    /// The wall-clock time the video ends.
    Ending,
    Off,
}

impl ControlsTime {
    pub const ALL: &'static [ControlsTime] = &[ControlsTime::Remaining, ControlsTime::Ending, ControlsTime::Off];

    pub fn label(self) -> &'static str {
        match self {
            ControlsTime::Remaining => "Remaining time",
            ControlsTime::Ending => "Ending time",
            ControlsTime::Off => "Off",
        }
    }
}

/// Playback speeds offered (SmartTube's list).
pub const SPEEDS: &[f32] = &[
    0.25, 0.5, 0.75, 0.8, 0.85, 0.9, 0.95, 1.0, 1.05, 1.1, 1.15, 1.2, 1.25, 1.3, 1.4, 1.5, 1.6, 1.7, 1.75, 1.8, 1.9, 2.0, 2.25, 2.5, 2.75, 3.0,
    3.25, 3.5, 3.75, 4.0,
];

/// Index of the listed speed closest to `r`.
pub fn speed_index(r: f32) -> usize {
    let r = if r > 0.0 { r } else { 1.0 };
    (0..SPEEDS.len()).min_by(|a, b| (SPEEDS[*a] - r).abs().total_cmp(&(SPEEDS[*b] - r).abs())).unwrap_or(7)
}

/// Sleep timer choices in minutes: off, then half an hour to ten hours in half-hour steps.
pub const SLEEP_MINS: &[u32] = &[0, 30, 60, 90, 120, 150, 180, 210, 240, 270, 300, 330, 360, 390, 420, 450, 480, 510, 540, 570, 600];

/// "Off" / "1.5 hours" (translated).
pub fn sleep_label(mins: u32) -> String {
    use crate::i18n::tr;
    if mins == 0 {
        return tr("Off").to_string();
    }
    let h = mins as f32 / 60.0;
    format!("{h} {}", tr(if mins == 60 { "hour" } else { "hours" }))
}

/// "5 seconds" (translated).
pub fn secs_label(n: u32) -> String {
    format!("{n} {}", crate::i18n::tr(if n == 1 { "second" } else { "seconds" }))
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
    /// Play HDR (PQ/HLG) formats when a video has them (engine B).
    pub hdr: bool,
    pub speed: f32,
    pub remember_speed: RememberSpeed,
    /// Speeds remembered per channel (by channel name).
    pub speed_by_channel: std::collections::HashMap<String, f32>,
    /// The speed OK switches to ("Toggle speed"): the last speed other than 1.0 chosen.
    pub toggle_speed: f32,
    pub seek_step_secs: u32,
    pub playback_mode: PlaybackMode,
    pub resume_playback: bool,
    /// Resume videos shorter than 5 minutes too (off: they start over).
    pub resume_short_videos: bool,
    /// Resume live streams where they were left.
    pub resume_live: bool,
    /// Hide the controls after this many seconds (0 = never).
    pub hud_timeout_secs: u32,
    pub show_clock: bool,
    /// A clock at the top right of the video lists.
    pub clock_on_pages: bool,
    pub controls_time: ControlsTime,
    pub ok_action: OkAction,
    /// Pause after this many minutes without a key press (0 = off).
    pub sleep_timer_mins: u32,
    /// In a Shorts list, a Short repeats at its end instead of moving on.
    pub loop_shorts: bool,
    /// Number keys 0..9 seek to 0..90 % of the video.
    pub seek_number_keys: bool,
    /// With the controls hidden, BACK has to be pressed twice to leave the player.
    pub double_back_exit: bool,
    /// Show the controls when the next video starts by itself (or with next / previous).
    pub hud_on_next_video: bool,
    /// Preferred audio language code ("" = the video's original track).
    pub audio_language: String,
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
    /// Subtitles switched on / off or a language chosen while watching a channel are remembered
    /// for that channel.
    pub subtitles_per_channel: bool,
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
    /// True black background (OLED).
    pub black_background: bool,
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
            hdr: false,
            speed: 1.0,
            remember_speed: RememberSpeed::Off,
            speed_by_channel: Default::default(),
            toggle_speed: 1.5,
            seek_step_secs: 10,
            playback_mode: PlaybackMode::PlayNext,
            resume_playback: true,
            resume_short_videos: false,
            resume_live: true,
            hud_timeout_secs: 3,
            show_clock: true,
            clock_on_pages: false,
            controls_time: ControlsTime::Remaining,
            ok_action: OkAction::Controls,
            sleep_timer_mins: 0,
            loop_shorts: true,
            seek_number_keys: true,
            double_back_exit: false,
            hud_on_next_video: false,
            audio_language: String::new(),
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
            subtitles_per_channel: true,
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
            black_background: false,
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
        let raw: Option<serde_json::Value> = std::fs::read_to_string(Self::path()).ok().and_then(|s| serde_json::from_str(&s).ok());
        Self::from_json(raw)
    }

    fn from_json(raw: Option<serde_json::Value>) -> Settings {
        let Some(v) = raw else { return Settings::default() };
        let mut s: Settings = serde_json::from_value(v.clone()).unwrap_or_default();
        s.migrate(&v);
        s
    }

    /// Carries older single options over to the ones that replaced them.
    fn migrate(&mut self, v: &serde_json::Value) {
        let has = |k: &str| v.get(k).is_some();
        if !has("playback_mode") {
            self.playback_mode = match v["repeat"].as_str() {
                Some("Off") => PlaybackMode::PauseEach,
                Some("One") => PlaybackMode::RepeatOne,
                Some("List") => PlaybackMode::ListRepeat,
                _ => PlaybackMode::PlayNext,
            };
        }
        if !has("remember_speed") && v["remember_speed_per_channel"].as_bool() == Some(true) {
            self.remember_speed = RememberSpeed::PerChannel;
        }
        if !has("controls_time") && v["show_ending_time"].as_bool() == Some(false) {
            self.controls_time = ControlsTime::Off;
        }
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

    #[test]
    fn old_options_migrate() {
        let v = serde_json::json!({"repeat": "One", "remember_speed_per_channel": true, "show_ending_time": false});
        let s = Settings::from_json(Some(v));
        assert_eq!(s.playback_mode, PlaybackMode::RepeatOne);
        assert_eq!(s.remember_speed, RememberSpeed::PerChannel);
        assert_eq!(s.controls_time, ControlsTime::Off);
        let s = Settings::from_json(Some(serde_json::json!({"playback_mode": "Shuffle", "repeat": "One"})));
        assert_eq!(s.playback_mode, PlaybackMode::Shuffle);
    }
}

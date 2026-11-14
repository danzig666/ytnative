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

/// How the player's controls look.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayerStyle {
    /// Two rows of round buttons around the seek bar, with every option at hand.
    SmartTube,
    /// The official YouTube TV app's player: the title at the top, one row of controls.
    YouTube,
}

impl PlayerStyle {
    pub const ALL: &'static [PlayerStyle] = &[PlayerStyle::SmartTube, PlayerStyle::YouTube];

    pub fn label(self) -> &'static str {
        match self {
            PlayerStyle::SmartTube => "SmartTube",
            PlayerStyle::YouTube => "YouTube",
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

/// Thumbnail frame choices ("default" = YouTube's / the uploader's picture).
pub const THUMBNAIL_FRAMES: &[&str] = &["default", "start", "middle", "end"];

pub fn thumbnail_frame_label(k: &str) -> &'static str {
    match k {
        "start" => "From the start",
        "middle" => "From the middle",
        "end" => "From the end",
        _ => "YouTube's default",
    }
}

/// A thumbnail-frame setting as DeArrow's frame choice.
pub fn thumbnail_frame(k: &str) -> yt_core::ext::dearrow::FallbackFrame {
    use yt_core::ext::dearrow::FallbackFrame as F;
    match k {
        "start" => F::Start,
        "middle" => F::Middle,
        "end" => F::End,
        _ => F::Default,
    }
}

/// Content countries offered (region code, the country's own name); "" = automatic.
pub const COUNTRIES: &[(&str, &str)] = &[
    ("", "Automatic"),
    ("US", "United States"),
    ("GB", "United Kingdom"),
    ("CA", "Canada"),
    ("AU", "Australia"),
    ("NZ", "New Zealand"),
    ("IE", "Ireland"),
    ("HU", "Magyarország"),
    ("DE", "Deutschland"),
    ("AT", "Österreich"),
    ("CH", "Schweiz"),
    ("FR", "France"),
    ("BE", "Belgique"),
    ("NL", "Nederland"),
    ("LU", "Luxembourg"),
    ("ES", "España"),
    ("PT", "Portugal"),
    ("IT", "Italia"),
    ("PL", "Polska"),
    ("CZ", "Česko"),
    ("SK", "Slovensko"),
    ("SI", "Slovenija"),
    ("HR", "Hrvatska"),
    ("RS", "Србија"),
    ("BA", "Bosna i Hercegovina"),
    ("RO", "România"),
    ("BG", "България"),
    ("GR", "Ελλάδα"),
    ("TR", "Türkiye"),
    ("UA", "Україна"),
    ("RU", "Россия"),
    ("SE", "Sverige"),
    ("NO", "Norge"),
    ("DK", "Danmark"),
    ("FI", "Suomi"),
    ("IS", "Ísland"),
    ("EE", "Eesti"),
    ("LV", "Latvija"),
    ("LT", "Lietuva"),
    ("IL", "ישראל"),
    ("AE", "الإمارات"),
    ("SA", "السعودية"),
    ("EG", "مصر"),
    ("IN", "भारत"),
    ("PK", "پاکستان"),
    ("ID", "Indonesia"),
    ("TH", "ประเทศไทย"),
    ("VN", "Việt Nam"),
    ("PH", "Pilipinas"),
    ("MY", "Malaysia"),
    ("SG", "Singapore"),
    ("JP", "日本"),
    ("KR", "대한민국"),
    ("TW", "臺灣"),
    ("HK", "香港"),
    ("MX", "México"),
    ("BR", "Brasil"),
    ("AR", "Argentina"),
    ("CL", "Chile"),
    ("CO", "Colombia"),
    ("PE", "Perú"),
    ("ZA", "South Africa"),
    ("NG", "Nigeria"),
    ("KE", "Kenya"),
];

/// A content country's name ("" = "Automatic", translated).
pub fn country_name(code: &str) -> String {
    match COUNTRIES.iter().find(|(c, _)| c.eq_ignore_ascii_case(code)) {
        Some(("", n)) => crate::i18n::tr(n).to_string(),
        Some((_, n)) => n.to_string(),
        None => code.to_string(),
    }
}

/// Index of the listed speed closest to `r`.
pub fn speed_index(r: f32) -> usize {
    let r = if r > 0.0 { r } else { 1.0 };
    (0..SPEEDS.len()).min_by(|a, b| (SPEEDS[*a] - r).abs().total_cmp(&(SPEEDS[*b] - r).abs())).unwrap_or(7)
}

/// Screen dimming amounts (percent) and timeouts (seconds; 0 = at once), as SmartTube offers them.
pub const DIM_PERCENTS: &[u32] = &[10, 20, 30, 40, 50, 60, 70, 80, 90, 100];
pub const DIM_TIMEOUTS: &[u32] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 1800, 3600, 5400, 7200, 9000, 10800];

/// A screen dimming timeout in words.
pub fn dim_timeout_label(secs: u32) -> String {
    match secs {
        0 => crate::i18n::tr("When pressed").to_string(),
        s if s < 60 => format!("{s} {}", crate::i18n::tr("seconds")),
        s => format!("{} {}", s / 60, crate::i18n::tr("minutes")),
    }
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

/// "Off" / "0.5 seconds" for a duration in milliseconds (translated).
pub fn ms_label(ms: u32) -> String {
    if ms == 0 {
        return crate::i18n::tr("Off").to_string();
    }
    let v = ms as f32 / 1000.0;
    format!("{v} {}", crate::i18n::tr(if ms == 1000 { "second" } else { "seconds" }))
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
    /// Left on the leftmost tile opens the sidebar (off: only BACK does).
    pub left_opens_sidebar: bool,
    pub controls_time: ControlsTime,
    pub ok_action: OkAction,
    /// The look of the player's controls.
    pub player_style: PlayerStyle,
    /// Pause after this many minutes without a key press (0 = off).
    pub sleep_timer_mins: u32,
    /// Screen dimming in the player (SmartTube's "Screen off"): how dark, in percent (100 = black).
    pub screen_off_dimming: u32,
    /// Seconds without a key press before the armed dimming starts; 0 = the button dims at once.
    pub screen_off_timeout_secs: u32,
    /// The dimming button is on (armed timeout, or a partial dim that carries over to the next video).
    pub screen_off_armed: bool,
    /// In a Shorts list, a Short repeats at its end instead of moving on.
    pub loop_shorts: bool,
    /// Number keys 0..9 seek to 0..90 % of the video.
    pub seek_number_keys: bool,
    /// With the controls hidden, BACK has to be pressed twice to leave the player.
    pub double_back_exit: bool,
    /// Show the controls when the next video starts by itself (or with next / previous).
    pub hud_on_next_video: bool,
    /// "Play next" is kept when the app closes and comes back at the next start.
    pub keep_play_next: bool,
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
    /// The highlight point: skip = jump to it when the video starts, ask = offer the jump.
    pub sb_highlight: SbAction,
    pub sb_skip_once: bool,
    /// Segment markers on the seek bar (all categories)...
    pub sb_markers: bool,
    /// ...except these categories (API names: "sponsor", "intro", ...).
    pub sb_marker_hidden: Vec<String>,
    /// Segments shorter than this are not skipped (0 = all are).
    pub sb_min_segment_ms: u32,
    /// Channels (by name) SponsorBlock leaves alone.
    pub sb_excluded_channels: Vec<String>,
    /// Ask SponsorBlock's alternative server first.
    pub sb_alt_server: bool,
    /// A notice when YouTube marks the video as containing paid promotion.
    pub sb_paid_notice: bool,
    pub dearrow_titles: bool,
    pub dearrow_thumbnails: bool,
    /// Videos DeArrow has no thumbnail for: "default" (keep it), "start", "middle", "end"
    /// (YouTube's frames).
    pub dearrow_fallback: String,
    pub return_dislikes: bool,
    // Content filters
    pub hide_shorts_home: bool,
    pub hide_shorts_subscriptions: bool,
    pub hide_shorts_search: bool,
    /// Shorts hidden in every list (channel pages, history, trending too).
    pub hide_shorts_everywhere: bool,
    pub hide_watched_home: bool,
    pub hide_watched_subscriptions: bool,
    pub hide_upcoming_home: bool,
    pub hide_upcoming_subscriptions: bool,
    pub hide_upcoming_channels: bool,
    /// Live and past live streams hidden in Subscriptions.
    pub hide_streams_subscriptions: bool,
    pub hide_mixes: bool,
    pub blocked_channels: Vec<String>,
    /// "auto" (YouTube's account setting decides), "on" or "off" (no history pings, no resume
    /// positions saved).
    pub watch_history: String,
    /// Searches are recorded (off: the history is cleared and nothing is recorded).
    pub search_history: bool,
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
    /// Region of YouTube's content (InnerTube `gl`), "" = automatic.
    pub content_country: String,
    /// Card thumbnails: "default" (the uploader's), "start", "middle" or "end" (YouTube's frames).
    pub thumbnail_frame: String,
    /// Channels page sorted alphabetically (off: YouTube's order, last viewed first).
    pub channels_alphabetical: bool,
    // Remote
    pub lounge_enabled: bool,
    /// A message when a phone connects / disconnects.
    pub lounge_connect_notice: bool,
    /// Leave the player when the phone disconnects.
    pub lounge_close_on_disconnect: bool,
    /// Videos started from the phone stay out of the watch history.
    pub lounge_no_history: bool,
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
    /// The TV's capabilities were read once (first start) and the playback settings set from them.
    pub device_checked: bool,
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
            left_opens_sidebar: true,
            controls_time: ControlsTime::Remaining,
            ok_action: OkAction::Controls,
            player_style: PlayerStyle::SmartTube,
            sleep_timer_mins: 0,
            screen_off_dimming: 100,
            screen_off_timeout_secs: 0,
            screen_off_armed: false,
            loop_shorts: true,
            seek_number_keys: true,
            double_back_exit: false,
            hud_on_next_video: false,
            keep_play_next: false,
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
            sb_highlight: SbAction::Ask,
            sb_skip_once: true,
            sb_markers: true,
            sb_marker_hidden: Vec::new(),
            sb_min_segment_ms: 5000,
            sb_excluded_channels: Vec::new(),
            sb_alt_server: false,
            sb_paid_notice: false,
            dearrow_titles: false,
            dearrow_thumbnails: false,
            dearrow_fallback: "default".into(),
            return_dislikes: true,
            hide_shorts_home: false,
            hide_shorts_subscriptions: false,
            hide_shorts_search: false,
            hide_shorts_everywhere: false,
            hide_watched_home: false,
            hide_watched_subscriptions: false,
            hide_upcoming_home: false,
            hide_upcoming_subscriptions: false,
            hide_upcoming_channels: false,
            hide_streams_subscriptions: false,
            hide_mixes: false,
            blocked_channels: Vec::new(),
            watch_history: "auto".into(),
            search_history: true,
            pinned: Vec::new(),
            // Trending is off by default: YouTube retired the page (browse answers HTTP 400).
            sections: crate::store::Section::ALL.iter().filter(|s| **s != crate::store::Section::Trending).map(|s| s.key().to_string()).collect(),
            boot_section: "home".into(),
            idle_blank_secs: 120,
            time_24h: true,
            accent: "red".into(),
            ui_language: "system".into(),
            content_country: "".into(),
            thumbnail_frame: "default".into(),
            channels_alphabetical: false,
            lounge_enabled: true,
            lounge_connect_notice: false,
            lounge_close_on_disconnect: false,
            lounge_no_history: false,
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
            device_checked: false,
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
        let mut s: Settings = serde_json::from_value(v.clone()).unwrap_or_else(|_| Self::lenient(&v));
        s.migrate(&v);
        s
    }

    /// A file with a value this build cannot read (an option of a newer version, a hand edit):
    /// keep every other saved choice instead of starting over (which the next save would make
    /// permanent). Each field is taken over only if it still reads.
    fn lenient(v: &serde_json::Value) -> Settings {
        let Some(obj) = v.as_object() else { return Settings::default() };
        let Ok(serde_json::Value::Object(mut merged)) = serde_json::to_value(Settings::default()) else { return Settings::default() };
        for (k, val) in obj {
            let before = merged.insert(k.clone(), val.clone());
            if serde_json::from_value::<Settings>(serde_json::Value::Object(merged.clone())).is_err() {
                crate::log(&format!("settings: unreadable value for {k:?} ignored"));
                match before {
                    Some(b) => merged.insert(k.clone(), b),
                    None => merged.remove(k),
                };
            }
        }
        serde_json::from_value(serde_json::Value::Object(merged)).unwrap_or_default()
    }

    /// Carries older single options over to the ones that replaced them.
    fn migrate(&mut self, v: &serde_json::Value) {
        let has = |k: &str| v.get(k).is_some();
        // Settings saved before the TV's capabilities were read: the playback choices in them may
        // be the viewer's own (1080p, H.264, the system player look the same as the old defaults),
        // so only a first start (no settings yet) is set from the TV.
        if !has("device_checked") {
            self.device_checked = true;
        }
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
        if !has("hide_watched_home") && v["hide_watched"].as_bool() == Some(true) {
            self.hide_watched_home = true;
            self.hide_watched_subscriptions = true;
        }
        // Card-menu kinds added later go where the default order has them, not at the end.
        let defaults: Vec<&str> = crate::screens::menu::CARD_ITEMS.iter().map(|(k, _)| *k).collect();
        for (i, k) in defaults.iter().enumerate() {
            if !self.card_menu.iter().any(|x| x == k) {
                let after = defaults[..i].iter().rev().find_map(|p| self.card_menu.iter().position(|x| x == p));
                self.card_menu.insert(after.map_or(0, |a| a + 1), k.to_string());
            }
        }
        if !has("hide_upcoming_home") && v["hide_upcoming"].as_bool() == Some(true) {
            self.hide_upcoming_home = true;
            self.hide_upcoming_subscriptions = true;
            self.hide_upcoming_channels = true;
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
        let v = serde_json::json!({"repeat": "One", "remember_speed_per_channel": true, "show_ending_time": false, "hide_watched": true});
        let s = Settings::from_json(Some(v));
        assert!(s.hide_watched_home && s.hide_watched_subscriptions && !s.hide_upcoming_home);
        assert_eq!(s.playback_mode, PlaybackMode::RepeatOne);
        assert_eq!(s.remember_speed, RememberSpeed::PerChannel);
        assert_eq!(s.controls_time, ControlsTime::Off);
        let s = Settings::from_json(Some(serde_json::json!({"playback_mode": "Shuffle", "repeat": "One"})));
        assert_eq!(s.playback_mode, PlaybackMode::Shuffle);
    }

    #[test]
    fn saved_settings_keep_their_playback_choices() {
        // Saved before the TV's capabilities were read: not a first start.
        assert!(Settings::from_json(Some(serde_json::json!({"codec": "Avc"}))).device_checked);
        assert!(!Settings::from_json(Some(serde_json::json!({"device_checked": false}))).device_checked);
        assert!(!Settings::from_json(None).device_checked);
    }

    #[test]
    fn new_card_menu_kinds_go_where_the_default_order_has_them() {
        let s = Settings::from_json(Some(serde_json::json!({"card_menu": ["comments", "play", "play_next", "open", "playlist"]})));
        let pos = |k: &str| s.card_menu.iter().position(|x| x == k).unwrap();
        assert_eq!(pos("comments"), 0);
        assert_eq!(pos("play_all"), pos("play_next") + 1);
        assert_eq!(pos("tv_order"), pos("play_all") + 1);
        assert_eq!(pos("tv_manage"), pos("open") + 1);
        assert_eq!(pos("copy_list"), pos("playlist") + 1);
        assert_eq!(s.card_menu.len(), crate::screens::menu::CARD_ITEMS.len());
        assert!(!s.keep_play_next);
    }

    #[test]
    fn one_unreadable_value_keeps_the_others() {
        let s = Settings::from_json(Some(serde_json::json!({
            "max_height": 2160, "player_style": "SomeFutureStyle", "color_keys": ["none"], "device_checked": true
        })));
        assert_eq!(s.max_height, 2160);
        assert!(s.device_checked);
        assert_eq!(s.player_style, PlayerStyle::SmartTube);
        assert_eq!(s.color_keys[1], "subtitles");
    }
}

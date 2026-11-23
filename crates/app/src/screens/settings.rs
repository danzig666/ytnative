//! Settings in two panes: the categories down the left, the focused category's options on the
//! right. Up / Down on a category shows its options at once; Right or OK goes into them, and
//! Left or BACK comes back out. Each option row shows its value: an on / off switch, which OK (or
//! a click) flips, or the value, where OK opens the list of values to pick from.
//! A row that opens a page (blocked channels, segment markers) opens it in the right pane, the
//! category staying selected on the left. Changes are saved immediately.
//!
//! "Search settings" heads the left pane: OK opens the on-screen keyboard, and the options found
//! ([`super::settings_search`]) are listed on the right like a category's, each with its category
//! dimmed after it. OK on one goes to the option in its category, focused (a sub-page's option
//! opens the sub-page). Left and BACK in the results come back to "Search settings".

use crate::host::{SettingsPage, YtArg, YtFx, YtHost};
use crate::settings::{secs_label, Codec, ControlsTime, KeyPair, OkAction, PlaybackMode, PlayerStyle, RememberSpeed, SbAction, Settings, SPEEDS};
use crate::store::Section;
use crate::ui::{self, VList};
use plx_machine::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, Key, LogicalState, Machine, NavOp, ScreenEvent,
};
use plx_ui::frame::Budget;
use plx_ui::screen::{Activate, At, Hover, Stop, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_machine::machine::Measure;
use plx_ui::{Painter, Rect};
use std::borrow::Cow;

const G_LEFT: GroupId = GroupId(1);
const G_RIGHT: GroupId = GroupId(2);
const LEFT_BASE: u32 = 0x3000_0000;
const RIGHT_BASE: u32 = 0x3100_0000;

/// The categories down the left pane, with their icons.
const CATS: [(SettingsPage, ui::icon::Kind); 7] = [
    (SettingsPage::Playback, ui::icon::Kind::CirclePlay),
    (SettingsPage::Subtitles, ui::icon::Kind::Captions),
    (SettingsPage::SponsorBlock, ui::icon::Kind::Shield),
    (SettingsPage::Content, ui::icon::Kind::Filter),
    (SettingsPage::Interface, ui::icon::Kind::Monitor),
    (SettingsPage::Remote, ui::icon::Kind::Cast),
    (SettingsPage::About, ui::icon::Kind::User),
];

/// The category a page is shown under (a sub-page under the category that opens it).
fn category_of(page: SettingsPage) -> usize {
    let p = match page {
        SettingsPage::Blocked => SettingsPage::Content,
        SettingsPage::Markers => SettingsPage::SponsorBlock,
        SettingsPage::CardMenu => SettingsPage::Interface,
        p => p,
    };
    CATS.iter().position(|c| c.0 == p).unwrap_or(0)
}

/// The search results' slot beside the categories' (in `cat`, `right_last` and `tops`). Its row,
/// "Search settings", heads the left pane, above the categories.
const SEARCH: usize = CATS.len();
/// The slots the right pane can show: the categories and the search results.
const SLOTS: usize = CATS.len() + 1;
/// The left pane's rows: "Search settings", then the categories.
const LEFT_N: usize = CATS.len() + 1;

/// The slot a left-pane row shows.
fn slot_of_row(row: usize) -> usize {
    if row == 0 { SEARCH } else { row - 1 }
}

/// The left-pane row of a slot.
fn row_of_slot(slot: usize) -> usize {
    if slot == SEARCH { 0 } else { slot + 1 }
}

/// The pages a settings search looks through, in the order their results are listed (a sub-page
/// right after the category that opens it). The blocked channels are content, not options.
const SEARCHED: [SettingsPage; 9] = [
    SettingsPage::Playback,
    SettingsPage::Subtitles,
    SettingsPage::SponsorBlock,
    SettingsPage::Markers,
    SettingsPage::Content,
    SettingsPage::Interface,
    SettingsPage::CardMenu,
    SettingsPage::Remote,
    SettingsPage::About,
];

/// A search result: the option's page, its row there and its label.
type Hit = (SettingsPage, usize, &'static str);

/// The options whose label, as shown or in English, holds every word of `query`.
fn find(query: &str, s: &Settings) -> Vec<Hit> {
    let words = super::settings_search::words(query);
    let mut out = Vec::new();
    for page in SEARCHED {
        for (i, it) in items(page, s).iter().enumerate() {
            let Some(l) = label_of(it, s) else { continue };
            if super::settings_search::matches(&words, crate::i18n::tr(l), l) {
                out.push((page, i, l));
            }
        }
    }
    out
}

/// The category a result is under, as its row names it (a sub-page after its category).
fn hit_place(page: SettingsPage) -> String {
    let top = CATS[category_of(page)].0;
    if top == page {
        crate::i18n::tr(title(page)).to_string()
    } else {
        format!("{} \u{203A} {}", crate::i18n::tr(title(top)), crate::i18n::tr(title(page)))
    }
}

// Layout (the 1920 x 1080 frame).
const LEFT_W: f32 = 600.0;
const RIGHT_X: f32 = 676.0;
const RIGHT_W: f32 = 1168.0;
const LIST_Y: f32 = 176.0;
const RIGHT_ROW_H: f32 = 64.0;
const RIGHT_ROWS: f32 = 13.0;
const TEXT: i32 = 28;
const SWITCH_W: f32 = 66.0;
const SWITCH_H: f32 = 36.0;
/// What the "Card menu" page says above its rows.
const CARD_MENU_NOTE: &str = "The card menu opens when you hold OK on a video, channel or playlist. Here you choose what it offers and in what order: OK shows or hides an item; to move one, press Right, then Up or Down, then OK.";
/// The "Card menu" page: room above its rows for what a card menu is and how to edit it.
const NOTE_H: f32 = 104.0;

type Get = fn(&Settings) -> String;
type Change = fn(&mut Settings, i32);

enum Item {
    Page(&'static str, SettingsPage),
    Opt(&'static str, Get, Change),
    Action(&'static str, fn(&mut Effects<'_, YtHost>)),
    Info(&'static str, fn(&Cx<'_, YtHost>) -> String),
    /// A blocked channel (by index in the settings): OK unblocks it.
    Blocked(usize),
    /// A settings search result ([`Hit`]): OK goes to the option.
    Hit(SettingsPage, usize, &'static str),
    /// A card-menu item (by its place in `Settings::card_menu`): OK shows or hides it, Right
    /// picks it up to move with Up / Down.
    CardItem(usize),
}

/// The label a row is found by (in a settings search, and again when going to it).
fn label_of(it: &Item, s: &Settings) -> Option<&'static str> {
    match it {
        Item::Opt(l, ..) | Item::Page(l, _) | Item::Action(l, _) | Item::Info(l, _) => Some(*l),
        Item::CardItem(i) => s.card_menu.get(*i).and_then(|k| card_label(k)),
        Item::Blocked(_) | Item::Hit(..) => None,
    }
}

fn cycle_key(k: &mut String, d: i32) {
    const A: &[&str] = &["none", "search", "settings", "accounts", "subtitles", "speed", "comments", "video info", "chapters"];
    let i = A.iter().position(|x| *x == k).unwrap_or(0) as i32;
    *k = A[((i + d).rem_euclid(A.len() as i32)) as usize].into();
}

/// A card-menu item's settings label ("Card menu: Play"): what a settings search finds.
fn card_label(key: &str) -> Option<&'static str> {
    super::menu::CARD_ITEMS.iter().find(|(k, _)| *k == key).map(|(_, l)| *l)
}

/// A card-menu item's name on the "Card menu" page: its label without "Card menu: ", translated.
fn card_name(key: &str) -> String {
    let Some(label) = card_label(key) else { return key.to_string() };
    let bare = label.trim_start_matches("Card menu: ");
    let t = crate::i18n::tr(bare);
    if t != bare || crate::i18n::tr(label) == label {
        return t.to_string();
    }
    // Translated only with the prefix: the translation's own prefix comes off.
    without_prefix(crate::i18n::tr(label))
}

/// "Kártyamenü: Lejátszás" -> "Lejátszás" ("Menu de la carte : Lire" -> "Lire").
fn without_prefix(label: &str) -> String {
    label.split_once(':').map_or(label, |(_, rest)| rest).trim().to_string()
}

/// Shows or hides the card-menu item `key`.
fn card_toggle(s: &mut Settings, key: &str) {
    if let Some(i) = s.card_menu_hidden.iter().position(|h| h == key) {
        s.card_menu_hidden.remove(i);
    } else {
        s.card_menu_hidden.push(key.to_string());
    }
}

/// Swaps the card-menu items `a` and `b` (by name: the order may have changed since).
fn card_swap(s: &mut Settings, a: &str, b: &str) {
    let (Some(i), Some(j)) = (s.card_menu.iter().position(|k| k == a), s.card_menu.iter().position(|k| k == b)) else { return };
    s.card_menu.swap(i, j);
}

fn language_name(code: &str) -> String {
    crate::i18n::language_name(code)
}

/// Language codes offered in language lists (each language once: aliases such as iw / zh-CN are
/// not offered).
fn language_codes() -> Vec<&'static str> {
    crate::i18n::LANGUAGES.iter().map(|(c, _)| *c).filter(|c| !matches!(*c, "iw" | "tl" | "nb" | "zh-CN" | "zh-TW" | "zh")).collect()
}

/// Steps through `list` (strings) from `cur` by `d`, wrapping.
fn cycle_str(list: &[&str], cur: &mut String, d: i32) {
    let i = list.iter().position(|x| *x == cur.as_str()).unwrap_or(0) as i32;
    *cur = list[((i + d).rem_euclid(list.len() as i32)) as usize].to_string();
}

fn on_off(b: bool) -> String {
    if b { "On".into() } else { "Off".into() }
}

fn cycle<T: Copy + PartialEq>(list: &[T], cur: T, dir: i32) -> T {
    let i = list.iter().position(|x| *x == cur).unwrap_or(0) as i32;
    let n = list.len() as i32;
    list[((i + dir).rem_euclid(n)) as usize]
}

fn sb(a: SbAction) -> String {
    match a {
        SbAction::Off => "Off",
        SbAction::Skip => "Skip",
        SbAction::SkipWithToast => "Skip + notify",
        SbAction::Ask => "Ask",
    }
    .into()
}
const SB: &[SbAction] = &[SbAction::Off, SbAction::Skip, SbAction::SkipWithToast, SbAction::Ask];

fn items(page: SettingsPage, s: &Settings) -> Vec<Item> {
    use Item::*;
    match page {
        SettingsPage::Blocked if s.blocked_channels.is_empty() => vec![Info("No blocked channels", |_| String::new())],
        SettingsPage::Blocked => (0..s.blocked_channels.len()).map(Blocked).collect(),
        SettingsPage::CardMenu => {
            let mut v: Vec<Item> = (0..s.card_menu.len()).map(CardItem).collect();
            v.push(Action("Reset the card menu", |fx| {
                fx.push(Fx::App(YtFx::EditSettings(Box::new(|s| {
                    let d = Settings::default();
                    s.card_menu = d.card_menu;
                    s.card_menu_hidden = d.card_menu_hidden;
                }))));
                fx.push(Fx::App(YtFx::Toast(crate::i18n::tr("Card menu reset").into())));
            }));
            v
        }
        SettingsPage::Markers => {
            macro_rules! marker {
                ($label:expr, $cat:expr) => {
                    Opt($label, |s| on_off(!s.sb_marker_hidden.iter().any(|c| c == $cat)), |s, _| {
                        if let Some(i) = s.sb_marker_hidden.iter().position(|c| c == $cat) {
                            s.sb_marker_hidden.remove(i);
                        } else {
                            s.sb_marker_hidden.push($cat.to_string());
                        }
                    })
                };
            }
            vec![
                Opt("Segment markers on the seek bar", |s| on_off(s.sb_markers), |s, _| s.sb_markers = !s.sb_markers),
                marker!("Sponsor", "sponsor"),
                marker!("Self-promotion", "selfpromo"),
                marker!("Interaction reminder", "interaction"),
                marker!("Intro", "intro"),
                marker!("Outro / endcards", "outro"),
                marker!("Preview / recap", "preview"),
                marker!("Non-music section", "music_offtopic"),
                marker!("Filler", "filler"),
                marker!("Highlight", "poi_highlight"),
            ]
        }
        SettingsPage::Root => vec![
            Page("Playback", SettingsPage::Playback),
            Page("Subtitles", SettingsPage::Subtitles),
            Page("SponsorBlock & DeArrow", SettingsPage::SponsorBlock),
            Page("Content filters", SettingsPage::Content),
            Page("Interface", SettingsPage::Interface),
            Page("Remote control", SettingsPage::Remote),
            Page("Account & about", SettingsPage::About),
        ],
        SettingsPage::Playback => vec![
            Opt("Maximum quality", |s| format!("{}p", s.max_height), |s, d| s.max_height = cycle(&[480, 720, 1080, 1440, 2160], s.max_height, d)),
            Opt("Start quality (minimum)", |s| format!("{}p", s.min_height), |s, d| s.min_height = cycle(&[144, 360, 480, 720, 1080], s.min_height, d)),
            Opt(
                "Preferred codec",
                |s| match s.codec {
                    Codec::Avc => "H.264 (AVC)".into(),
                    Codec::Vp9 => "VP9".into(),
                    Codec::Av1 => "AV1".into(),
                },
                |s, d| s.codec = cycle(&[Codec::Avc, Codec::Vp9, Codec::Av1], s.codec, d),
            ),
            Opt("HDR", |s| on_off(s.hdr), |s, _| s.hdr = !s.hdr),
            Opt("Audio delay", |s| format!("{:+} ms", s.audio_delay_ms).replace("+0 ms", "0 ms"), |s, d| {
                let steps: Vec<i32> = (-10..=10).map(|i| i * 50).collect();
                s.audio_delay_ms = cycle(&steps, s.audio_delay_ms, d)
            }),
            Opt("Playback engine", |s| if s.engine == "auto" { "4K / VP9 / AV1 (engine B)".into() } else { "System player".into() }, |s, _| {
                s.engine = if s.engine == "auto" { "system".into() } else { "auto".into() }
            }),
            Opt("Player style", |s| s.player_style.label().into(), |s, d| s.player_style = cycle(PlayerStyle::ALL, s.player_style, d)),
            Opt("When a video ends", |s| s.playback_mode.label().into(), |s, d| s.playback_mode = cycle(PlaybackMode::ALL, s.playback_mode, d)),
            Opt("Default speed", |s| format!("{}x", s.speed), |s, d| {
                let i = crate::settings::speed_index(s.speed) as i32;
                s.speed = SPEEDS[((i + d).rem_euclid(SPEEDS.len() as i32)) as usize];
            }),
            Opt("Remember speed", |s| s.remember_speed.label().into(), |s, d| s.remember_speed = cycle(RememberSpeed::ALL, s.remember_speed, d)),
            Opt("OK button during playback", |s| s.ok_action.label().into(), |s, d| s.ok_action = cycle(OkAction::ALL, s.ok_action, d)),
            Opt("CH+ / CH- buttons", |s| s.channel_keys.label().into(), |s, d| s.channel_keys = cycle(KeyPair::ALL, s.channel_keys, d)),
            Opt("Fast-forward / rewind buttons", |s| s.ff_keys.label().into(), |s, d| s.ff_keys = cycle(KeyPair::ALL, s.ff_keys, d)),
            Opt("Confirm seeking with OK", |s| on_off(s.seek_confirm), |s, _| s.seek_confirm = !s.seek_confirm),
            Opt("Show the chapter's name", |s| on_off(s.chapter_notification), |s, _| s.chapter_notification = !s.chapter_notification),
            Opt("Hide controls after", |s| if s.hud_timeout_secs == 0 { "Never".into() } else { secs_label(s.hud_timeout_secs) }, |s, d| {
                s.hud_timeout_secs = cycle(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15], s.hud_timeout_secs, d)
            }),
            Opt("Seek step", |s| secs_label(s.seek_step_secs), |s, d| s.seek_step_secs = cycle(&[1, 2, 3, 5, 7, 10, 15, 20, 30, 60], s.seek_step_secs, d)),
            Opt("Seek with number keys", |s| on_off(s.seek_number_keys), |s, _| s.seek_number_keys = !s.seek_number_keys),
            Opt("Time in the controls", |s| s.controls_time.label().into(), |s, d| s.controls_time = cycle(ControlsTime::ALL, s.controls_time, d)),
            Opt("Clock in player", |s| on_off(s.show_clock), |s, _| s.show_clock = !s.show_clock),
            Opt("Screen dimming amount", |s| format!("{} %", s.screen_off_dimming), |s, d| {
                s.screen_off_dimming = cycle(crate::settings::DIM_PERCENTS, s.screen_off_dimming, d)
            }),
            Opt("Screen dimming timeout", |s| crate::settings::dim_timeout_label(s.screen_off_timeout_secs), |s, d| {
                s.screen_off_timeout_secs = cycle(crate::settings::DIM_TIMEOUTS, s.screen_off_timeout_secs, d)
            }),
            Opt("Resume where you left off", |s| on_off(s.resume_playback), |s, _| s.resume_playback = !s.resume_playback),
            Opt("Resume short videos (under 5 minutes)", |s| on_off(s.resume_short_videos), |s, _| s.resume_short_videos = !s.resume_short_videos),
            Opt("Remember live stream position", |s| on_off(s.resume_live), |s, _| s.resume_live = !s.resume_live),
            Opt("Keep up with live", |s| on_off(s.keep_live), |s, _| s.keep_live = !s.keep_live),
            Opt("Live chat in live streams", |s| on_off(s.live_chat), |s, _| s.live_chat = !s.live_chat),
            Opt("Loop Shorts", |s| on_off(s.loop_shorts), |s, _| s.loop_shorts = !s.loop_shorts),
            Opt("Leave the player with a double BACK", |s| on_off(s.double_back_exit), |s, _| s.double_back_exit = !s.double_back_exit),
            Opt("Show the controls when the next video starts", |s| on_off(s.hud_on_next_video), |s, _| s.hud_on_next_video = !s.hud_on_next_video),
            Opt("Keep Play next after closing", |s| on_off(s.keep_play_next), |s, _| s.keep_play_next = !s.keep_play_next),
            Opt("Ambient mode", |s| on_off(s.ambient_mode), |s, _| s.ambient_mode = !s.ambient_mode),
            Opt("Preferred audio language", |s| if s.audio_language.is_empty() { "Original".into() } else { language_name(&s.audio_language) }, |s, d| {
                let mut l = vec![""];
                l.extend(language_codes());
                cycle_str(&l, &mut s.audio_language, d);
            }),
            Opt("Audio only", |s| on_off(s.audio_only), |s, _| s.audio_only = !s.audio_only),
            Opt("Video details", |s| on_off(s.video_details), |s, _| s.video_details = !s.video_details),
            Opt("Same volume for every video", |s| on_off(s.same_volume), |s, _| s.same_volume = !s.same_volume),
        ],
        SettingsPage::Subtitles => vec![
            Opt("Subtitles", |s| on_off(s.subtitles), |s, _| s.subtitles = !s.subtitles),
            Opt(
                "Language",
                |s| language_name(&s.subtitle_language),
                |s, d| cycle_str(&language_codes(), &mut s.subtitle_language, d),
            ),
            Opt("Subtitles per channel", |s| on_off(s.subtitles_per_channel), |s, _| s.subtitles_per_channel = !s.subtitles_per_channel),
            Opt("Auto-translate when missing", |s| on_off(s.subtitle_auto_translate), |s, _| s.subtitle_auto_translate = !s.subtitle_auto_translate),
            Opt("Size", |s| s.subtitle_size.to_string(), |s, d| s.subtitle_size = cycle(&[32, 38, 44, 52, 60, 72], s.subtitle_size, d)),
            Opt("Colour", |s| s.subtitle_color.clone(), |s, d| {
                const C: &[&str] = &["white", "light gray", "gray", "dark gray", "yellow", "light yellow", "cyan", "green", "orange", "pink"];
                let i = C.iter().position(|c| *c == s.subtitle_color).unwrap_or(0) as i32;
                s.subtitle_color = C[((i + d).rem_euclid(C.len() as i32)) as usize].into();
            }),
            Opt("Font", |s| s.subtitle_font.clone(), |s, d| {
                const F: &[&str] = &["default", "sans", "serif", "mono", "roboto"];
                let i = F.iter().position(|x| *x == s.subtitle_font).unwrap_or(0) as i32;
                s.subtitle_font = F[((i + d).rem_euclid(F.len() as i32)) as usize].into();
            }),
            Opt("Bold", |s| on_off(s.subtitle_bold), |s, _| s.subtitle_bold = !s.subtitle_bold),
            Opt("Style", |s| s.subtitle_style.clone(), |s, d| {
                const T: &[&str] = &["outline", "shadow", "box"];
                let i = T.iter().position(|x| *x == s.subtitle_style).unwrap_or(0) as i32;
                s.subtitle_style = T[((i + d).rem_euclid(T.len() as i32)) as usize].into();
            }),
            Opt("Position from bottom", |s| format!("{} {}", s.subtitle_bottom_offset, crate::i18n::tr("pixels")), |s, d| s.subtitle_bottom_offset = cycle(&[40, 90, 140, 200], s.subtitle_bottom_offset, d)),
        ],
        SettingsPage::SponsorBlock => vec![
            Opt("SponsorBlock", |s| on_off(s.sponsorblock), |s, _| s.sponsorblock = !s.sponsorblock),
            Opt("Sponsor", |s| sb(s.sb_sponsor), |s, d| s.sb_sponsor = cycle(SB, s.sb_sponsor, d)),
            Opt("Self-promotion", |s| sb(s.sb_selfpromo), |s, d| s.sb_selfpromo = cycle(SB, s.sb_selfpromo, d)),
            Opt("Interaction reminder", |s| sb(s.sb_interaction), |s, d| s.sb_interaction = cycle(SB, s.sb_interaction, d)),
            Opt("Intro", |s| sb(s.sb_intro), |s, d| s.sb_intro = cycle(SB, s.sb_intro, d)),
            Opt("Outro / endcards", |s| sb(s.sb_outro), |s, d| s.sb_outro = cycle(SB, s.sb_outro, d)),
            Opt("Preview / recap", |s| sb(s.sb_preview), |s, d| s.sb_preview = cycle(SB, s.sb_preview, d)),
            Opt("Non-music section", |s| sb(s.sb_music_offtopic), |s, d| s.sb_music_offtopic = cycle(SB, s.sb_music_offtopic, d)),
            Opt("Filler", |s| sb(s.sb_filler), |s, d| s.sb_filler = cycle(SB, s.sb_filler, d)),
            Opt("Highlight", |s| sb(s.sb_highlight), |s, d| s.sb_highlight = cycle(SB, s.sb_highlight, d)),
            Opt("Skip each segment only once", |s| on_off(s.sb_skip_once), |s, _| s.sb_skip_once = !s.sb_skip_once),
            Opt("Ignore short segments", |s| crate::settings::ms_label(s.sb_min_segment_ms), |s, d| {
                let l: Vec<u32> = (0..=40).map(|i| i * 500).collect();
                s.sb_min_segment_ms = cycle(&l, s.sb_min_segment_ms, d);
            }),
            Page("Segment markers", SettingsPage::Markers),
            Opt("Use the alternative server", |s| on_off(s.sb_alt_server), |s, _| s.sb_alt_server = !s.sb_alt_server),
            Opt("Paid promotion notice", |s| on_off(s.sb_paid_notice), |s, _| s.sb_paid_notice = !s.sb_paid_notice),
            Opt("DeArrow titles", |s| on_off(s.dearrow_titles), |s, _| s.dearrow_titles = !s.dearrow_titles),
            Opt("DeArrow thumbnails", |s| on_off(s.dearrow_thumbnails), |s, _| s.dearrow_thumbnails = !s.dearrow_thumbnails),
            Opt("Fallback thumbnail when DeArrow has none", |s| crate::settings::thumbnail_frame_label(&s.dearrow_fallback).into(), |s, d| {
                cycle_str(crate::settings::THUMBNAIL_FRAMES, &mut s.dearrow_fallback, d)
            }),
            Opt("Return YouTube Dislike", |s| on_off(s.return_dislikes), |s, _| s.return_dislikes = !s.return_dislikes),
        ],
        SettingsPage::Content => vec![
            Opt("Hide Shorts everywhere", |s| on_off(s.hide_shorts_everywhere), |s, _| s.hide_shorts_everywhere = !s.hide_shorts_everywhere),
            Opt("Hide Shorts on Home", |s| on_off(s.hide_shorts_home), |s, _| s.hide_shorts_home = !s.hide_shorts_home),
            Opt("Hide Shorts in Subscriptions", |s| on_off(s.hide_shorts_subscriptions), |s, _| s.hide_shorts_subscriptions = !s.hide_shorts_subscriptions),
            Opt("Hide Shorts in Search", |s| on_off(s.hide_shorts_search), |s, _| s.hide_shorts_search = !s.hide_shorts_search),
            Opt("Hide Shorts in History", |s| on_off(s.hide_shorts_history), |s, _| s.hide_shorts_history = !s.hide_shorts_history),
            Opt("Hide Shorts in Trending", |s| on_off(s.hide_shorts_trending), |s, _| s.hide_shorts_trending = !s.hide_shorts_trending),
            Opt("Hide Shorts on channel pages", |s| on_off(s.hide_shorts_channels), |s, _| s.hide_shorts_channels = !s.hide_shorts_channels),
            Opt("Hide watched videos on Home", |s| on_off(s.hide_watched_home), |s, _| s.hide_watched_home = !s.hide_watched_home),
            Opt("Hide watched videos in Subscriptions", |s| on_off(s.hide_watched_subscriptions), |s, _| {
                s.hide_watched_subscriptions = !s.hide_watched_subscriptions
            }),
            Opt("Hide watched videos in Notifications", |s| on_off(s.hide_watched_notifications), |s, _| {
                s.hide_watched_notifications = !s.hide_watched_notifications
            }),
            Opt("Hide watched videos in Watch later", |s| on_off(s.hide_watched_watch_later), |s, _| {
                s.hide_watched_watch_later = !s.hide_watched_watch_later
            }),
            Opt("Hide upcoming streams on Home", |s| on_off(s.hide_upcoming_home), |s, _| s.hide_upcoming_home = !s.hide_upcoming_home),
            Opt("Hide upcoming streams in Subscriptions", |s| on_off(s.hide_upcoming_subscriptions), |s, _| {
                s.hide_upcoming_subscriptions = !s.hide_upcoming_subscriptions
            }),
            Opt("Hide upcoming streams on channel pages", |s| on_off(s.hide_upcoming_channels), |s, _| s.hide_upcoming_channels = !s.hide_upcoming_channels),
            Opt("Hide streams in Subscriptions", |s| on_off(s.hide_streams_subscriptions), |s, _| s.hide_streams_subscriptions = !s.hide_streams_subscriptions),
            Opt("Hide mixes", |s| on_off(s.hide_mixes), |s, _| s.hide_mixes = !s.hide_mixes),
            Page("Blocked channels", SettingsPage::Blocked),
            Opt("Watch history", |s| match s.watch_history.as_str() { "on" => "On".into(), "off" => "Off".into(), _ => "Automatic".into() }, |s, d| {
                cycle_str(&["auto", "on", "off"], &mut s.watch_history, d)
            }),
            // Switched off, the searches made so far are forgotten (in the host, where the change
            // is applied: an option's `change` must not do anything but change the settings, as
            // this screen also runs it on copies to list the option's values).
            Opt("Search history", |s| on_off(s.search_history), |s, _| s.search_history = !s.search_history),
            Action("Clear search history", |fx| fx.push(Fx::App(YtFx::Act(crate::host::UserAction::ClearSearchHistory)))),
            Action("Import channel groups from USB", |fx| fx.push(Fx::App(YtFx::Act(crate::host::UserAction::ImportGroups)))),
            Opt("Child mode (no search, no comments)", |s| on_off(s.child_mode), |s, _| s.child_mode = !s.child_mode),
            Action("Set settings PIN", |fx| fx.push(Fx::Nav(NavOp::Push(YtArg::Pin(crate::host::PinPurpose::Set))))),
            Opt("Settings PIN", |s| (if s.settings_pin.is_empty() { "Off" } else { "On" }).into(), |s, _| s.settings_pin.clear()),
        ],
        SettingsPage::Interface => {
            let mut v = vec![
                Opt("Start in", |s| Section::from_key(&s.boot_section).unwrap_or(Section::Home).title().into(), |s, d| {
                    let cur = Section::from_key(&s.boot_section).unwrap_or(Section::Home);
                    s.boot_section = cycle(Section::ALL, cur, d).key().into();
                }),
                Opt("Black background (OLED)", |s| on_off(s.black_background), |s, _| s.black_background = !s.black_background),
                Opt("Blank screen when idle (OLED)", |s| if s.idle_blank_mins == 0 { "Off".into() } else { format!("{} {}", s.idle_blank_mins, crate::i18n::tr("minutes")) }, |s, d| {
                    s.idle_blank_mins = cycle(&[0u32, 2, 5, 10, 30], s.idle_blank_mins, d)
                }),
                Opt("Screen size", |s| format!("{}%", s.ui_scale), |s, d| s.ui_scale = cycle(&[100u8, 97, 95, 92, 90], s.ui_scale, d)),
                Opt("Titles under thumbnails", |s| on_off(s.titles_under), |s, _| s.titles_under = !s.titles_under),
                Opt("Original video titles", |s| on_off(s.original_titles), |s, _| s.original_titles = !s.original_titles),
                Opt("Card size", |s| match s.card_columns { 3 => "large".into(), 5 => "small".into(), _ => "medium".into() }, |s, d| {
                    s.card_columns = cycle(&[3u8, 4, 5], s.card_columns, d)
                }),
                Opt("Interface language", |s| language_name(&s.ui_language), |s, d| {
                    let l = crate::i18n::picker_languages();
                    let i = l.iter().position(|x| *x == s.ui_language).unwrap_or(0) as i32;
                    s.ui_language = l[((i + d).rem_euclid(l.len() as i32)) as usize].into();
                }),
                Opt("Content country", |s| crate::settings::country_name(&s.content_country), |s, d| {
                    let l: Vec<&str> = crate::settings::COUNTRIES.iter().map(|(c, _)| *c).collect();
                    cycle_str(&l, &mut s.content_country, d);
                }),
                Opt("Thumbnail picture", |s| crate::settings::thumbnail_frame_label(&s.thumbnail_frame).into(), |s, d| {
                    cycle_str(crate::settings::THUMBNAIL_FRAMES, &mut s.thumbnail_frame, d)
                }),
                Opt("Channels sorting", |s| (if s.channels_alphabetical { "Alphabetically" } else { "Last viewed" }).into(), |s, _| {
                    s.channels_alphabetical = !s.channels_alphabetical
                }),
                Opt("Clock on video lists", |s| on_off(s.clock_on_pages), |s, _| s.clock_on_pages = !s.clock_on_pages),
                Opt("Left on the first tile opens the sidebar", |s| on_off(s.left_opens_sidebar), |s, _| {
                    s.left_opens_sidebar = !s.left_opens_sidebar
                }),
                Opt("24-hour clock", |s| on_off(s.time_24h), |s, _| s.time_24h = !s.time_24h),
            Opt("Accent colour", |s| s.accent.clone(), |s, d| {
                const C: &[&str] = &["red", "blue", "green", "purple", "orange"];
                let i = C.iter().position(|c| *c == s.accent).unwrap_or(0) as i32;
                s.accent = C[((i + d).rem_euclid(C.len() as i32)) as usize].into();
            }),
            Opt("Play a preview of the focused video", |s| on_off(s.video_previews), |s, _| s.video_previews = !s.video_previews),
            Opt("Recommendations on the TV's Home", |s| on_off(s.home_recommendations), |s, _| {
                s.home_recommendations = !s.home_recommendations
            }),
            ];
            // One toggle per sidebar section.
            macro_rules! section_toggle {
                ($label:expr, $sec:expr) => {
                    Opt($label, |s| on_off(s.sections.iter().any(|k| k == $sec.key())), |s, _| {
                        let k = $sec.key().to_string();
                        if let Some(i) = s.sections.iter().position(|x| *x == k) {
                            s.sections.remove(i);
                        } else {
                            crate::settings::show_section(&mut s.sections, &k);
                        }
                    })
                };
            }
            v.extend([
                section_toggle!("Sidebar: Home", Section::Home),
                section_toggle!("Sidebar: Subscriptions", Section::Subscriptions),
                section_toggle!("Sidebar: Shorts", Section::Shorts),
                section_toggle!("Sidebar: Channels", Section::Channels),
                section_toggle!("Sidebar: History", Section::History),
                section_toggle!("Sidebar: Library", Section::Library),
                section_toggle!("Sidebar: My videos", Section::MyVideos),
                section_toggle!("Sidebar: Music", Section::Music),
                section_toggle!("Sidebar: Gaming", Section::Gaming),
                section_toggle!("Sidebar: News", Section::News),
                section_toggle!("Sidebar: Podcasts", Section::Podcasts),
                section_toggle!("Sidebar: Live", Section::Live),
                section_toggle!("Sidebar: Movies", Section::Movies),
                section_toggle!("Sidebar: Sports", Section::Sports),
                section_toggle!("Sidebar: Trending", Section::Trending),
                section_toggle!("Sidebar: Kids", Section::Kids),
                section_toggle!("Sidebar: Notifications", Section::Notifications),
            ]);
            v.push(Page("Card menu", SettingsPage::CardMenu));
            v
        }
        SettingsPage::Remote => vec![
            Opt("Link with TV code (after restart)", |s| on_off(s.lounge_enabled), |s, _| s.lounge_enabled = !s.lounge_enabled),
            Opt("Show a message when a phone connects or disconnects", |s| on_off(s.lounge_connect_notice), |s, _| {
                s.lounge_connect_notice = !s.lounge_connect_notice
            }),
            Opt("Close the player when the phone disconnects", |s| on_off(s.lounge_close_on_disconnect), |s, _| {
                s.lounge_close_on_disconnect = !s.lounge_close_on_disconnect
            }),
            Opt("No watch history for videos started from the phone", |s| on_off(s.lounge_no_history), |s, _| s.lounge_no_history = !s.lounge_no_history),
            Opt("Red button", |s| s.color_keys[0].clone(), |s, d| cycle_key(&mut s.color_keys[0], d)),
            Opt("Green button", |s| s.color_keys[1].clone(), |s, d| cycle_key(&mut s.color_keys[1], d)),
            Opt("Yellow button", |s| s.color_keys[2].clone(), |s, d| cycle_key(&mut s.color_keys[2], d)),
            Opt("Blue button", |s| s.color_keys[3].clone(), |s, d| cycle_key(&mut s.color_keys[3], d)),
            Opt("DNS over HTTPS (after restart)", |s| match s.doh.as_str() {
                "cloudflare" => "Cloudflare".into(),
                "google" => "Google".into(),
                "quad9" => "Quad9".into(),
                o => o.into(),
            }, |s, d| {
                const D: &[&str] = &["off", "cloudflare", "google", "quad9"];
                let i = D.iter().position(|x| *x == s.doh).unwrap_or(0) as i32;
                s.doh = D[((i + d).rem_euclid(D.len() as i32)) as usize].into();
            }),
            Info("Proxy (proxy_url in settings.json)", |cx| {
                let p = &cx.views.store.settings.proxy_url;
                if p.is_empty() { crate::i18n::tr("none").into() } else { p.clone() }
            }),
            Opt("Show in the phone's cast menu (after restart)", |s| on_off(s.dial_enabled), |s, _| s.dial_enabled = !s.dial_enabled),
            Info("TV code", |cx| match &cx.views.store.lounge_code {
                Some(c) => c.clone(),
                None if cx.views.store.settings.lounge_enabled => crate::i18n::tr("getting a code\u{2026}").into(),
                None => crate::i18n::tr("off").into(),
            }),
            Info("On your phone", |_| crate::i18n::tr("YouTube \u{203A} Cast \u{203A} Link with TV code").into()),
        ],
        SettingsPage::About => {
            let mut v = vec![
            Action("Accounts (switch, add, remove)", |fx| fx.push(Fx::Nav(NavOp::Push(YtArg::Accounts)))),
            Action("Sign in with a code", |fx| fx.push(Fx::Nav(NavOp::Push(YtArg::SignIn)))),
            Action("Sign out", |fx| {
                fx.push(Fx::App(YtFx::SignOut));
                fx.push(Fx::App(YtFx::Toast("Signed out".into())));
            }),
            Action("Back up settings (USB or app storage)", |fx| fx.push(Fx::App(YtFx::Act(crate::host::UserAction::Backup)))),
            Action("Restore latest backup", |fx| fx.push(Fx::App(YtFx::Act(crate::host::UserAction::Restore)))),
            // Developer Mode on webOS 1-3 (or a package without its data folder): nothing the app
            // may write survives a restart, so say so instead of signing the viewer out silently.
            Info("Sign-in and settings", |_| {
                crate::i18n::tr(if crate::DATA_TEMPORARY.load(std::sync::atomic::Ordering::Relaxed) { "Lost at every restart (no writable folder)" } else { "Kept across restarts" }).into()
            }),
            Action("Problem report", |fx| fx.push(Fx::Nav(NavOp::Push(YtArg::ProblemReport)))),
            Info("Version", |_| app_version().into()),
            Info("Update", |_| update_text()),
            Action("Check for updates", |_| crate::update::check_now()),
            Info("Source", |_| "github.com/danzig666/ytnative".into()),
            ];
            // Below "Check for updates", so the focus stays on it when this row appears.
            if crate::update::found() {
                let at = v.len() - 1;
                v.insert(at, Action("Install the update (closes the app)", |_| crate::update::install()));
            }
            v
        }
    }
}

fn title(page: SettingsPage) -> &'static str {
    match page {
        SettingsPage::Root => "Settings",
        SettingsPage::Playback => "Playback",
        SettingsPage::Subtitles => "Subtitles",
        SettingsPage::SponsorBlock => "SponsorBlock & DeArrow",
        SettingsPage::Content => "Content filters",
        SettingsPage::Interface => "Interface",
        SettingsPage::Remote => "Remote control",
        SettingsPage::About => "Account & about",
        SettingsPage::Blocked => "Blocked channels",
        SettingsPage::Markers => "Segment markers",
        SettingsPage::CardMenu => "Card menu",
    }
}

struct State(String);
impl LogicalState for State {
    fn write(&self, c: &mut Canon) {
        c.str(&self.0);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&self.0);
    }
}

pub struct SettingsScreen {
    /// `Root`: the two panes with the categories focusable; a sub-page (blocked channels, segment
    /// markers): its rows on the right, its category shown selected on the left.
    page: SettingsPage,
    /// "Search settings" and the categories (left pane).
    left: VList,
    /// The options of the category shown, or the search results (right pane).
    list: VList,
    /// The slot shown on the right (a category or [`SEARCH`]): the focused one while the focus is
    /// on the left.
    cat: std::cell::Cell<usize>,
    /// The option row last focused in each slot (Right returns to it).
    right_last: std::cell::Cell<[usize; SLOTS]>,
    /// The first option row shown, per slot ([`Self::top`]).
    tops: std::cell::Cell<[usize; SLOTS]>,
    /// Which option rows are on / off switches, by page and row (worked out once: it never changes).
    switches: std::cell::RefCell<std::collections::HashMap<(u8, usize), bool>>,
    /// A focus the screen moved itself (into a category's options, BACK out of them).
    focus_req: std::cell::Cell<Option<u32>>,
    /// An option's drop-down list, while open.
    choice: Option<ui::ChoiceBox>,
    /// The row a closed drop-down belonged to (focus returns there).
    closed_row: std::cell::Cell<usize>,
    /// The words last looked for among the settings, and the options found.
    query: String,
    hits: Vec<Hit>,
    /// The row that opened a sub-page for a search result: the focus returns to it, not to the
    /// result, when the sub-page closes (the result list is no longer shown then).
    back_to: std::cell::Cell<Option<usize>>,
    /// The row a sub-page starts on (the option a search result led to).
    start: usize,
    /// The card-menu item picked up to move (Right on it): Up / Down move it, OK puts it down.
    moving: Option<String>,
    state: State,
}

impl SettingsScreen {
    pub fn new(entry: EntryId, page: SettingsPage) -> Self {
        let start = if page == SettingsPage::Root { None } else { super::settings_search::take_start(page) };
        let list = VList {
            entry,
            group: G_RIGHT,
            base: RIGHT_BASE,
            rect: if page == SettingsPage::CardMenu {
                Rect::new(RIGHT_X, LIST_Y + NOTE_H, RIGHT_W, RIGHT_ROW_H * (RIGHT_ROWS - 2.0))
            } else {
                Rect::new(RIGHT_X, LIST_Y, RIGHT_W, RIGHT_ROW_H * RIGHT_ROWS)
            },
            row_h: RIGHT_ROW_H,
            text_size: TEXT,
        };
        SettingsScreen {
            page,
            choice: None,
            closed_row: std::cell::Cell::new(0),
            left: VList {
                entry,
                group: G_LEFT,
                base: LEFT_BASE,
                rect: Rect::new(36.0, LIST_Y, LEFT_W - 72.0, 84.0 * LEFT_N as f32),
                row_h: 84.0,
                text_size: 30,
            },
            cat: std::cell::Cell::new(category_of(page)),
            right_last: std::cell::Cell::new([0; SLOTS]),
            tops: std::cell::Cell::new([0; SLOTS]),
            switches: Default::default(),
            focus_req: std::cell::Cell::new(start.map(|r| list.elem(r))),
            list,
            query: String::new(),
            hits: Vec::new(),
            back_to: std::cell::Cell::new(None),
            start: start.unwrap_or(0),
            moving: None,
            state: State(format!("{page:?}")),
        }
    }

    fn root(&self) -> bool {
        self.page == SettingsPage::Root
    }

    /// The slot shown: follows the focus while it is on the left pane.
    fn category(&self, cx: &Cx<'_, YtHost>) -> usize {
        if self.root() {
            if let Some(i) = self.left.focused(cx, LEFT_N) {
                self.cat.set(slot_of_row(i));
            }
        }
        self.cat.get()
    }

    /// The page whose rows the right pane shows (`Root` for the search results).
    fn shown(&self, cx: &Cx<'_, YtHost>) -> SettingsPage {
        if !self.root() {
            return self.page;
        }
        match self.category(cx) {
            SEARCH => SettingsPage::Root,
            c => CATS[c].0,
        }
    }

    fn rows(&self, cx: &Cx<'_, YtHost>) -> Vec<Item> {
        if self.root() && self.category(cx) == SEARCH {
            return self.hits.iter().map(|&(p, i, l)| Item::Hit(p, i, l)).collect();
        }
        items(self.shown(cx), &cx.views.store.settings)
    }

    /// The option row Right lands on in slot `c`: the one last focused there.
    fn right_key(&self, c: usize, cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        let n = if c == SEARCH { self.hits.len() } else { items(CATS[c].0, &cx.views.store.settings).len() };
        self.list.key(self.right_last.get()[c].min(n.saturating_sub(1)))
    }

    /// OK on a search result: the option in its category, with the focus on it. An option on a
    /// sub-page opens that page focused on it, the row opening it focused beneath.
    fn go_to(&self, page: SettingsPage, row: usize, label: &'static str, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        let s = &cx.views.store.settings;
        // Found again by its label: rows may have come or gone since the search (an update found).
        let row = items(page, s).iter().position(|it| label_of(it, s) == Some(label)).unwrap_or(row);
        let c = category_of(page);
        let target = if CATS[c].0 == page {
            row
        } else {
            let opener = items(CATS[c].0, s).iter().position(|it| matches!(it, Item::Page(_, p) if *p == page)).unwrap_or(0);
            super::settings_search::set_start(page, row);
            self.back_to.set(Some(opener));
            fx.push(Fx::Nav(NavOp::Push(YtArg::Settings(page))));
            opener
        };
        self.cat.set(c);
        let mut last = self.right_last.get();
        last[c] = target;
        self.right_last.set(last);
        self.focus(self.list.elem(target), fx);
    }

    /// The focus is on an option row (not a category, not an open drop-down).
    fn on_right(&self, cx: &Cx<'_, YtHost>) -> bool {
        cx.focus.current.is_some_and(|k| k.entry == self.list.entry && (RIGHT_BASE..ui::CHOICE_BASE).contains(&k.elem))
    }

    /// The values option `i` can take and the current one's index.
    fn values(&self, i: usize, cx: &Cx<'_, YtHost>) -> Option<(Vec<String>, usize)> {
        let Some(Item::Opt(_, get, change)) = self.rows(cx).into_iter().nth(i) else { return None };
        Some(option_values(&cx.views.store.settings, get, change))
    }

    fn change(&self, i: usize, dir: i32, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        let list = self.rows(cx);
        match list.get(i) {
            Some(Item::Blocked(b)) if dir >= 0 => {
                if let Some(name) = cx.views.store.settings.blocked_channels.get(*b).cloned() {
                    fx.push(Fx::App(YtFx::Toast(crate::i18n::tr("{} unblocked").replace("{}", &name))));
                    fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| s.blocked_channels.retain(|c| *c != name)))));
                    fx.invalidate(plx_machine::present::Provenance::Input);
                }
            }
            Some(Item::Page(_, p)) if dir >= 0 => fx.push(Fx::Nav(NavOp::Push(YtArg::Settings(*p)))),
            Some(Item::Opt(_, _, change)) => {
                let change = *change;
                fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| change(s, dir)))));
                fx.invalidate(plx_machine::present::Provenance::Input);
            }
            Some(Item::Action(_, act)) if dir >= 0 => act(fx),
            Some(Item::CardItem(c)) => {
                if let Some(key) = cx.views.store.settings.card_menu.get(*c).cloned() {
                    fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| card_toggle(s, &key)))));
                    fx.invalidate(plx_machine::present::Provenance::Input);
                }
            }
            Some(Item::Hit(page, row, label)) if dir >= 0 => self.go_to(*page, *row, label, cx, fx),
            _ => {}
        }
    }

    /// The option row the right pane scrolls to: the focused one, the one an open drop-down
    /// belongs to, or (while a category has the focus) the one last focused in it, so the pane
    /// does not jump to its top and back.
    fn anchor(&self, cx: &Cx<'_, YtHost>, n: usize) -> Option<usize> {
        if let Some(c) = &self.choice {
            return Some(c.row);
        }
        self.list.focused(cx, n).or_else(|| self.root().then(|| self.right_last.get()[self.category(cx)].min(n.saturating_sub(1))))
    }

    /// The first option row shown: it stays where it is and only moves to keep the anchor row
    /// in view, so a pointer moving over the rows after the pane scrolled (the pointer resting at
    /// the screen's edge steps it) does not scroll it back.
    fn top(&self, cx: &Cx<'_, YtHost>, n: usize) -> usize {
        let vis = self.list.rows_visible();
        let mut t = self.tops.get()[self.category(cx)];
        if let Some(a) = self.anchor(cx, n) {
            if a < t {
                t = a;
            } else if a >= t + vis {
                t = a + 1 - vis;
            }
        }
        t.min(n.saturating_sub(vis))
    }

    /// Moves the focus to `elem` (applied in `reconcile`).
    fn focus(&self, elem: u32, fx: &mut Effects<'_, YtHost>) {
        self.focus_req.set(Some(elem));
        fx.invalidate(plx_machine::present::Provenance::Input);
    }
}

impl SettingsScreen {
    /// The "Card menu" page's moving: Right on an item picks it up, Up / Down then move it one
    /// place, and OK, Left, Right or BACK put it down. `None`: the event is not about moving.
    fn step_moving(&mut self, ev: &ScreenEvent<YtHost>, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Option<Handled> {
        if self.page != SettingsPage::CardMenu || self.choice.is_some() {
            return None;
        }
        let order = &cx.views.store.settings.card_menu;
        let focused = self.list.focused(cx, self.rows(cx).len()).filter(|i| *i < order.len());
        // The focus went elsewhere (the pointer, a click): the item is put down where it is.
        if self.moving.is_some() && (focused.is_none() || focused.map(|i| &order[i]) != self.moving.as_ref()) {
            self.moving = None;
            fx.invalidate(plx_machine::present::Provenance::Input);
        }
        let key = |ev: &ScreenEvent<YtHost>| match ev {
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key, edge, .. }, .. }) => Some((*key, *edge)),
            _ => None,
        };
        match (&self.moving, ev) {
            (Some(_), ScreenEvent::Activate(_)) => {
                self.moving = None;
                fx.invalidate(plx_machine::present::Provenance::Input);
                Some(Handled::Yes)
            }
            (Some(m), _) => match key(ev)? {
                (Key::Up | Key::Down, Edge::Down | Edge::Repeat) => {
                    let i = focused?;
                    let j = if key(ev)?.0 == Key::Up { i.checked_sub(1) } else { Some(i + 1).filter(|j| *j < order.len()) };
                    if let Some(j) = j {
                        let (a, b) = (m.clone(), order[j].clone());
                        fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| card_swap(s, &a, &b)))));
                        self.focus(self.list.elem(j), fx);
                    }
                    Some(Handled::Yes)
                }
                (Key::Left | Key::Right | Key::Back, edge) => {
                    if edge == Edge::Down {
                        self.moving = None;
                        fx.invalidate(plx_machine::present::Provenance::Input);
                    }
                    Some(Handled::Yes)
                }
                (Key::Up | Key::Down, _) => Some(Handled::Yes),
                _ => None,
            },
            (None, _) => match key(ev)? {
                (Key::Right, Edge::Down) => {
                    self.moving = Some(order[focused?].clone());
                    fx.invalidate(plx_machine::present::Provenance::Input);
                    Some(Handled::Yes)
                }
                _ => None,
            },
        }
    }
}

/// The values an option can take (its `change` applied to a copy of the settings until it comes
/// back round), and the current one's index. Numbers (qualities, speeds, sizes, minutes) are put
/// in ascending order; others keep their cycle order.
fn option_values(settings: &Settings, get: Get, change: Change) -> (Vec<String>, usize) {
    let mut s = settings.clone();
    let first = get(&s);
    let mut v = vec![first.clone()];
    for _ in 0..300 {
        change(&mut s, 1);
        let x = get(&s);
        if x == first || v.contains(&x) {
            break;
        }
        v.push(x);
    }
    let num = |x: &str| {
        let lead: String = x.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        if lead.is_empty() || lead.starts_with('.') {
            return None;
        }
        lead.parse::<f64>().ok()
    };
    if v.len() > 2 && v.iter().all(|x| num(x).is_some()) {
        v.sort_by(|a, b| num(a).partial_cmp(&num(b)).unwrap_or(std::cmp::Ordering::Equal));
    }
    let cur = v.iter().position(|x| *x == first).unwrap_or(0);
    (v, cur)
}

/// An on / off option, drawn as a switch: it reads "On" or "Off", and changing it flips between
/// the two (a SponsorBlock category reads "Off" too, but has more values; "Settings PIN" can only
/// be turned off).
fn is_switch(s: &Settings, get: Get, change: Change) -> bool {
    let v = get(s);
    if v != "On" && v != "Off" {
        return false;
    }
    let mut t = s.clone();
    change(&mut t, 1);
    let flipped = get(&t);
    change(&mut t, 1);
    flipped != v && (flipped == "On" || flipped == "Off") && get(&t) == v
}

impl Machine<YtHost> for SettingsScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        if let Some(a) = super::common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        let n = self.rows(cx).len();
        if let Some(h) = self.step_moving(ev, cx, fx) {
            return h;
        }
        match ev {
            ScreenEvent::Activate(e) if self.choice.as_ref().is_some_and(|c| c.owns(*e)) => {
                let c = self.choice.take().unwrap();
                self.closed_row.set(c.row);
                let k = c.list.index(*e, c.options.len()).unwrap_or(0);
                if let Some(Item::Opt(_, get, change)) = self.rows(cx).into_iter().nth(c.row) {
                    let target = c.options[k].clone();
                    fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| {
                        for _ in 0..300 {
                            if get(s) == target {
                                break;
                            }
                            change(s, 1);
                        }
                    }))));
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
                Handled::Yes
            }
            // OK (or a click) on a category: into its options. On "Search settings": the keyboard,
            // with the words looked for last.
            ScreenEvent::Activate(e) if self.root() && self.left.index(*e, LEFT_N).is_some() => {
                let c = slot_of_row(self.left.index(*e, LEFT_N).unwrap_or(1));
                self.cat.set(c);
                if c == SEARCH {
                    fx.push(Fx::Nav(NavOp::Push(YtArg::NameEntry(crate::host::NamePurpose::SettingsSearch(self.query.clone())))));
                } else {
                    self.focus(self.right_key(c, cx).elem, fx);
                }
                Handled::Yes
            }
            ScreenEvent::Activate(e) => {
                if let Some(i) = self.list.index(*e, n) {
                    // More than two values: a drop-down list; two (on/off): flip.
                    match self.values(i, cx) {
                        Some((v, cur)) if v.len() > 2 => {
                            let anchor = self.list.place(*e, n, Some(i)).map(|p| p.rect).unwrap_or(self.list.rect);
                            self.choice = Some(ui::ChoiceBox::new(self.list.entry, i, v, cur, anchor));
                            fx.invalidate(plx_machine::present::Provenance::Input);
                        }
                        _ => self.change(i, 1, cx, fx),
                    }
                }
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Back, edge: Edge::Down, .. }, .. }) if self.choice.is_some() => {
                if let Some(c) = self.choice.take() {
                    self.closed_row.set(c.row);
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Back, .. }, .. }) if self.choice.is_some() => Handled::Yes,
            // BACK in the options: back to their category (BACK there closes the settings).
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Back, edge, .. }, .. }) if self.root() && self.on_right(cx) => {
                if *edge == Edge::Down {
                    self.focus(self.left.elem(row_of_slot(self.cat.get())), fx);
                }
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Left | Key::Right, .. }, .. }) if self.choice.is_some() => Handled::Yes,
            ScreenEvent::Uncover => {
                let s = &cx.views.store.settings;
                if let Some(q) = super::settings_search::take_query().filter(|_| self.root()) {
                    // Back from the keyboard: the results on the right, the focus on the first
                    // (on "Search settings" when nothing matched).
                    self.hits = find(&q, s);
                    self.query = q;
                    self.cat.set(SEARCH);
                    let mut last = self.right_last.get();
                    last[SEARCH] = 0;
                    self.right_last.set(last);
                    let mut tops = self.tops.get();
                    tops[SEARCH] = 0;
                    self.tops.set(tops);
                    self.focus(if self.hits.is_empty() { self.left.elem(0) } else { self.list.elem(0) }, fx);
                } else {
                    if !self.query.is_empty() {
                        // Options may have come or gone meanwhile (an update found).
                        self.hits = find(&self.query, s);
                    }
                    if let Some(r) = self.back_to.take() {
                        self.focus(self.list.elem(r), fx);
                    }
                }
                fx.invalidate(plx_machine::present::Provenance::Nav);
                Handled::Yes
            }
            ScreenEvent::Input(_) => Handled::No,
            _ => Handled::Yes,
        }
    }
}

impl Focusable<YtHost> for SettingsScreen {
    fn groups(&self, cx: &Cx<'_, YtHost>, out: &mut Vec<GroupSpec>) {
        if let Some(c) = &self.choice {
            out.push(c.list.spec(c.options.len()));
            return;
        }
        if self.root() {
            out.push(self.left.spec(LEFT_N));
        }
        out.push(self.list.spec(self.rows(cx).len()));
    }
    fn group_of(&self, key: &u32, _cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        Some(if self.choice.as_ref().is_some_and(|c| c.owns(*key)) {
            ui::CHOICE_GROUP
        } else if *key < RIGHT_BASE {
            G_LEFT
        } else {
            G_RIGHT
        })
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, YtHost>) -> Step<u32> {
        if let Some(c) = &self.choice {
            return c.list.neighbour(key, dir, c.options.len());
        }
        if let Some(r) = self.left.index(key.elem, LEFT_N).filter(|_| self.root()) {
            let c = slot_of_row(r);
            return match dir {
                // No results to go into: the focus stays.
                Dir::Right if c == SEARCH && self.hits.is_empty() => Step::Move(key),
                Dir::Right => Step::Move(self.right_key(c, cx)),
                _ => self.left.neighbour(key, dir, LEFT_N),
            };
        }
        match self.list.neighbour(key, dir, self.rows(cx).len()) {
            Step::Edge if dir == Dir::Left && self.root() => Step::Move(self.left.key(row_of_slot(self.cat.get()))),
            s => s,
        }
    }
    fn place(&self, key: &u32, cx: &Cx<'_, YtHost>, _at: At) -> Option<Placed> {
        if let Some(c) = self.choice.as_ref().filter(|c| c.owns(*key)) {
            let n = c.options.len();
            return c.list.place(*key, n, c.list.focused(cx, n));
        }
        if self.root() && self.left.index(*key, LEFT_N).is_some() {
            return self.left.place(*key, LEFT_N, None);
        }
        let n = self.rows(cx).len();
        self.list.place_top(*key, n, self.top(cx, n))
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        let want = match self.focus_req.take() {
            Some(e) => FocusKey { entry: self.list.entry, elem: e },
            None => want,
        };
        if let Some(c) = &self.choice {
            return if c.owns(want.elem) { want } else { c.list.key(c.current) };
        }
        if want.elem >= ui::CHOICE_BASE {
            return self.list.key(self.closed_row.get());
        }
        if self.root() && self.left.index(want.elem, LEFT_N).is_some() {
            return want;
        }
        if self.list.index(want.elem, self.rows(cx).len()).is_some() {
            return want;
        }
        if self.root() {
            self.left.key(row_of_slot(self.cat.get()))
        } else {
            self.list.key(0)
        }
    }
    fn seat(&self, g: GroupId, _from: Placed, cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        if let (Some(c), true) = (&self.choice, g == ui::CHOICE_GROUP) {
            return c.list.key(c.current);
        }
        if g == G_LEFT {
            return self.left.key(row_of_slot(self.cat.get()));
        }
        if self.root() {
            self.right_key(self.cat.get(), cx)
        } else {
            self.list.key(self.start)
        }
    }
}

impl Screen<YtHost> for SettingsScreen {
    fn name(&self) -> &'static str {
        "settings"
    }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, YtHost>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, YtHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, YtHost>) {
        let p = f.painter;
        let cx = f.cx;
        let cat = self.category(cx);
        let rows = self.rows(cx);
        let rf = self.list.focused(cx, rows.len());
        if let Some(i) = rf.filter(|_| self.root() && self.choice.is_none()) {
            let mut last = self.right_last.get();
            last[cat] = i;
            self.right_last.set(last);
        }
        let lf = if self.root() { self.left.focused(cx, LEFT_N) } else { None };
        self.draw_left(p, f.measure, cat, lf);
        // The row an open drop-down belongs to stays lit.
        let lit = self.choice.as_ref().map(|c| c.row).or(rf);
        let top = self.top(cx, rows.len());
        let mut tops = self.tops.get();
        tops[cat] = top;
        self.tops.set(tops);
        self.draw_right(p, f.measure, cx, cat, &rows, lit, top);
        if let Some(c) = &self.choice {
            c.draw(f);
        } else {
            if self.root() {
                // While the options have the focus, the pointer passing over a category does not
                // take it (a click still does): the options stay put under a resting pointer.
                let hover = if self.on_right(cx) { Hover::OnlyIfFocused } else { Hover::Focus };
                for i in 0..LEFT_N {
                    let r = self.left.row_rect(i, 0);
                    f.stop(p, Stop { key: self.left.key(i), rect: r, rest_rect: r, clip: self.left.rect, hover, activate: Activate::Press });
                }
            }
            for i in (top..rows.len()).take(self.list.rows_visible()) {
                ui::stop(f, self.list.key(i), self.list.row_rect(i, top), self.list.rect);
            }
        }
        super::common::toast(p, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

impl SettingsScreen {
    /// The left pane: the title, "Search settings" (a search field, showing the words looked
    /// for), the categories, the version at the bottom.
    fn draw_left(&self, p: Painter, m: &dyn Measure, cat: usize, focus: Option<usize>) {
        ui::fill(p, Rect::new(0.0, 0.0, LEFT_W, 1080.0), 0.0, [1.0, 1.0, 1.0, 0.035]);
        ui::icon::draw(p, ui::icon::Kind::Gear, Rect::new(64.0, 70.0, 46.0, 46.0), ui::WHITE, false);
        ui::text(p, "Settings", 130.0, 64.0, 46, ui::WHITE, ui::ALIGN_LEFT, true);
        for i in 0..LEFT_N {
            let slot = slot_of_row(i);
            let (label, icon) = match slot {
                SEARCH if self.query.is_empty() => (crate::i18n::tr("Search settings"), ui::icon::Kind::Search),
                // The words typed: shown as they are.
                SEARCH => (self.query.as_str(), ui::icon::Kind::Search),
                c => (crate::i18n::tr(title(CATS[c].0)), CATS[c].1),
            };
            let r = self.left.row_rect(i, 0);
            let focused = focus == Some(i);
            let selected = slot == cat;
            if focused {
                ui::fill(p, r, 14.0, ui::ROW_FOCUS);
            } else if selected || slot == SEARCH {
                // The search row always looks like a field.
                ui::fill(p, r, 14.0, [1.0, 1.0, 1.0, 0.06]);
            }
            if selected {
                ui::fill(p, Rect::new(r.x, r.y + 18.0, 6.0, r.h - 36.0), 3.0, ui::accent());
            }
            let ink = if focused || selected { ui::WHITE } else { ui::DIM };
            let s = 36.0;
            ui::icon::draw(p, icon, Rect::new(r.x + 30.0, r.y + (r.h - s) / 2.0, s, s), if selected { ui::accent() } else { ink }, false);
            let ty = r.y + (r.h - 30.0 * 1.3) / 2.0;
            ui::text_cut(p, m, label, r.x + 92.0, ty, r.w - 116.0, 30, ink, ui::ALIGN_LEFT, focused || selected);
        }
        ui::text_raw(p, &format!("ytnative {}", app_version()), 64.0, 1008.0, 22, ui::FAINT, ui::ALIGN_LEFT, false);
    }

    /// The right pane: the category's (or sub-page's) title and its rows.
    fn draw_right(&self, p: Painter, m: &dyn Measure, cx: &Cx<'_, YtHost>, cat: usize, rows: &[Item], focus: Option<usize>, top: usize) {
        let s = &cx.views.store.settings;
        if self.root() && cat == SEARCH {
            ui::text(p, "Search settings", RIGHT_X + 28.0, 64.0, 46, ui::WHITE, ui::ALIGN_LEFT, true);
            if rows.is_empty() {
                let note = if self.query.is_empty() {
                    crate::i18n::tr("Press OK and type part of an option's name.").to_string()
                } else {
                    crate::i18n::tr("No option matches \u{201C}{}\u{201D}.").replace("{}", &self.query)
                };
                ui::text_cut(p, m, &note, RIGHT_X + 28.0, LIST_Y + 14.0, RIGHT_W - 56.0, TEXT, ui::FAINT, ui::ALIGN_LEFT, false);
            }
        } else if self.root() {
            ui::text(p, title(CATS[cat].0), RIGHT_X + 28.0, 64.0, 46, ui::WHITE, ui::ALIGN_LEFT, true);
        } else {
            // A sub-page: its category above its title.
            let (top_page, icon) = CATS[cat];
            ui::icon::draw(p, icon, Rect::new(RIGHT_X + 28.0, 40.0, 26.0, 26.0), ui::DIM, false);
            ui::text(p, title(top_page), RIGHT_X + 66.0, 38.0, 24, ui::DIM, ui::ALIGN_LEFT, false);
            ui::text(p, title(self.page), RIGHT_X + 28.0, 82.0, 46, ui::WHITE, ui::ALIGN_LEFT, true);
            if self.page == SettingsPage::CardMenu {
                let note = crate::i18n::tr(CARD_MENU_NOTE);
                for (k, line) in ui::wrap_lines(m, note, RIGHT_W - 56.0, 24, false, 3).iter().enumerate() {
                    ui::text_raw(p, line, RIGHT_X + 28.0, LIST_Y - 8.0 + 34.0 * k as f32, 24, ui::DIM, ui::ALIGN_LEFT, false);
                }
            }
        }
        let n = rows.len();
        let vis = self.list.rows_visible();
        for (i, item) in rows.iter().enumerate().skip(top).take(vis) {
            let r = self.list.row_rect(i, top);
            let focused = focus == Some(i);
            let moving = matches!(item, Item::CardItem(c) if s.card_menu.get(*c).is_some_and(|k| Some(k) == self.moving.as_ref()));
            if moving {
                // Picked up: lifted in the accent colour while Up / Down move it.
                let a = ui::accent();
                ui::fill(p, r, 12.0, [a[0], a[1], a[2], 0.45]);
            } else if focused {
                ui::fill(p, r, 12.0, ui::ROW_FOCUS);
            }
            let ty = r.y + (r.h - TEXT as f32 * 1.3) / 2.0;
            let ink = if focused { ui::WHITE } else { ui::DIM };
            let right = r.x + r.w - 28.0;
            // What the row shows at its right end, and the room that takes.
            let used = match item {
                Item::Opt(_, get, change) => {
                    let v = get(s);
                    let on_off = *self.switches.borrow_mut().entry((self.shown(cx) as u8, i)).or_insert_with(|| is_switch(s, *get, *change));
                    if on_off {
                        switch(p, right - SWITCH_W, r.y + (r.h - SWITCH_H) / 2.0, v == "On", focused);
                        SWITCH_W
                    } else {
                        let c = chevron(p, right, r, ink);
                        c + value(p, m, crate::i18n::tr(&v), right - c, ty, r.w, ink)
                    }
                }
                Item::Page(..) | Item::Hit(..) => chevron(p, right, r, ink),
                // The value is content (a code, an address, a version) or translated where made.
                Item::Info(_, get) => value(p, m, &get(cx), right, ty, r.w, if focused { ui::WHITE } else { [1.0, 1.0, 1.0, 0.8] }),
                Item::Blocked(_) => value(p, m, crate::i18n::tr("Unblock"), right, ty, r.w, if focused { ui::WHITE } else { ui::FAINT }),
                Item::Action(..) => 0.0,
                Item::CardItem(_) if moving => value(p, m, crate::i18n::tr("Up / Down to move, OK to place"), right, ty, r.w, ui::WHITE),
                Item::CardItem(c) => {
                    let shown = s.card_menu.get(*c).is_some_and(|k| !s.card_menu_hidden.contains(k));
                    switch(p, right - SWITCH_W, r.y + (r.h - SWITCH_H) / 2.0, shown, focused);
                    SWITCH_W
                }
            };
            let lx = r.x + 28.0;
            let room = (right - lx - if used > 0.0 { used + 28.0 } else { 0.0 }).max(40.0);
            let label = match item {
                // A channel's name: shown as it is.
                Item::Blocked(b) => s.blocked_channels.get(*b).cloned().unwrap_or_default(),
                Item::CardItem(c) => s.card_menu.get(*c).map(|k| card_name(k)).unwrap_or_default(),
                Item::Opt(l, ..) | Item::Page(l, _) | Item::Action(l, _) | Item::Info(l, _) | Item::Hit(_, _, l) => crate::i18n::tr(l).to_string(),
            };
            let label_ink = match item {
                _ if focused || moving => ui::WHITE,
                // A hidden card-menu item: dimmed beside the shown ones.
                Item::CardItem(c) if s.card_menu.get(*c).is_some_and(|k| s.card_menu_hidden.contains(k)) => ui::FAINT,
                Item::Info(..) => ui::DIM,
                _ => [1.0, 1.0, 1.0, 0.84],
            };
            ui::text_cut(p, m, &label, lx, ty, room, TEXT, label_ink, ui::ALIGN_LEFT, focused);
            // A search result: its category after it, dimmed.
            if let Item::Hit(page, ..) = item {
                let w = ui::text_width(m, &label, TEXT, focused).min(room) + 22.0;
                if room - w > 60.0 {
                    ui::text_cut(p, m, &hit_place(*page), lx + w, ty, room - w, TEXT, if focused { ui::DIM } else { ui::FAINT }, ui::ALIGN_LEFT, false);
                }
            }
        }
        // A scroll bar when the rows do not all fit.
        if n > vis {
            let track = self.list.rect;
            let (x, h) = (track.x + track.w + 22.0, track.h - 6.0);
            ui::fill(p, Rect::new(x, track.y, 6.0, h), 3.0, [1.0, 1.0, 1.0, 0.08]);
            let th = (h * vis as f32 / n as f32).max(40.0);
            let ty = track.y + (h - th) * top as f32 / (n - vis) as f32;
            ui::fill(p, Rect::new(x, ty, 6.0, th), 3.0, [1.0, 1.0, 1.0, 0.4]);
        }
    }
}

/// An on / off switch with its top-left at x, y.
fn switch(p: Painter, x: f32, y: f32, on: bool, focused: bool) {
    let track = if on {
        ui::accent()
    } else if focused {
        [1.0, 1.0, 1.0, 0.32]
    } else {
        [1.0, 1.0, 1.0, 0.18]
    };
    ui::fill(p, Rect::new(x, y, SWITCH_W, SWITCH_H), SWITCH_H / 2.0, track);
    let k = SWITCH_H - 8.0;
    let kx = if on { x + SWITCH_W - k - 4.0 } else { x + 4.0 };
    ui::fill(p, Rect::new(kx, y + 4.0, k, k), k / 2.0, if on || focused { ui::WHITE } else { [0.85, 0.85, 0.88, 1.0] });
}

/// The chevron of a row that opens a page or a list, at the row's right end; returns its room.
fn chevron(p: Painter, right: f32, r: Rect, ink: [f32; 4]) -> f32 {
    let s = 26.0;
    // The glyph's stroke ends 15/24 of the box across: line it up with the switches' right edge.
    ui::icon::draw(p, ui::icon::Kind::Chevron, Rect::new(right - s * 16.2 / 24.0, r.y + (r.h - s) / 2.0, s, s), ink, false);
    30.0
}

/// A value right-aligned at `right`, at most half the row wide; returns the room it takes.
fn value(p: Painter, m: &dyn Measure, v: &str, right: f32, y: f32, row_w: f32, ink: [f32; 4]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let w = ui::text_width(m, v, TEXT, false).min((row_w - 56.0) / 2.0);
    ui::text_cut(p, m, v, right, y, w, TEXT, ink, ui::ALIGN_RIGHT, false);
    w
}

/// The version the package is released as (`pkg/appinfo.json`, the one place it is bumped).
fn update_text() -> String {
    use crate::i18n::tr;
    use crate::update::State;
    match crate::update::state() {
        State::Unknown => tr("Not checked yet").into(),
        State::Checking => tr("Checking\u{2026}").into(),
        State::UpToDate => tr("Up to date").into(),
        State::Available(r) => tr("Version {} available").replace("{}", &r.version),
        State::CheckFailed(e) => tr("Could not check: {}").replace("{}", &e),
        State::Downloading(Some(p)) => format!("{} {p}%", tr("Downloading\u{2026}")),
        State::Downloading(None) => tr("Downloading\u{2026}").into(),
        State::Installing => tr("Installing\u{2026} the app will close").into(),
        State::Installed => tr("Installed: open the app again").into(),
        State::InstallFailed(e) => tr("Update failed: {}").replace("{}", tr(&e)),
    }
}

fn app_version() -> &'static str {
    const INFO: &str = include_str!("../../pkg/appinfo.json");
    INFO.split("\"version\"")
        .nth(1)
        .and_then(|rest| rest.split('"').nth(1))
        .unwrap_or(env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod version_tests {
    #[test]
    fn the_version_is_the_packages() {
        let v = super::app_version();
        assert!(v.split('.').count() == 3 && v.split('.').all(|n| n.parse::<u32>().is_ok()), "{v}");
        assert_ne!(v, "0.1.0");
    }
}

#[cfg(test)]
mod search_tests {
    use super::*;

    #[test]
    fn the_search_row_heads_the_left_pane() {
        assert_eq!(slot_of_row(0), SEARCH);
        for c in 0..CATS.len() {
            assert_eq!(slot_of_row(row_of_slot(c)), c);
        }
        assert_eq!(row_of_slot(SEARCH), 0);
    }

    #[test]
    fn options_are_found_on_every_page_and_sub_page() {
        let s = Settings::default();
        let hits = find("speed", &s);
        assert!(hits.iter().any(|h| h.0 == SettingsPage::Playback && h.2 == "Default speed"), "{hits:?}");
        // A sub-page's option, with its row there, listed right after its category's.
        let hits = find("outro", &s);
        let pages: Vec<SettingsPage> = hits.iter().map(|h| h.0).collect();
        assert_eq!(pages, [SettingsPage::SponsorBlock, SettingsPage::Markers]);
        let (_, row, label) = hits[1];
        assert!(matches!(items(SettingsPage::Markers, &s)[row], Item::Opt(l, ..) if l == label));
        // Words in any order, capitals and accents ignored; nothing for a blank query.
        assert!(find("LIVE keep", &s).iter().any(|h| h.2 == "Keep up with live"));
        assert!(find("s\u{e9}ek st\u{e9}p", &s).iter().any(|h| h.2 == "Seek step"));
        assert!(find("  ", &s).is_empty());
        assert!(find("no option is called this", &s).is_empty());
    }

    #[test]
    fn the_card_menu_page_lists_the_items_in_their_order() {
        let mut s = Settings::default();
        s.card_menu.swap(0, 1);
        let rows = items(SettingsPage::CardMenu, &s);
        assert_eq!(rows.len(), super::super::menu::CARD_ITEMS.len() + 1);
        assert!(matches!(rows[0], Item::CardItem(0)));
        assert_eq!(s.card_menu[0], "play_start");
        assert!(matches!(rows.last(), Some(Item::Action("Reset the card menu", _))));
        // Moved by name and hidden / shown again.
        card_swap(&mut s, "play", "play_start");
        assert_eq!(s.card_menu[..2], ["play", "play_start"]);
        card_toggle(&mut s, "comments");
        assert_eq!(s.card_menu_hidden, ["comments"]);
        card_toggle(&mut s, "comments");
        assert!(s.card_menu_hidden.is_empty());
        // Still found by a settings search, on its page.
        let hits = find("card menu comments", &s);
        assert!(hits.iter().any(|h| h.0 == SettingsPage::CardMenu), "{hits:?}");
    }

    #[test]
    fn card_items_are_named_without_the_prefix() {
        assert_eq!(without_prefix("Kártyamenü: Összes lejátszása és keverés"), "Összes lejátszása és keverés");
        assert_eq!(without_prefix("Menu de la carte : S'abonner / se désabonner"), "S'abonner / se désabonner");
    }

    #[test]
    fn a_result_names_its_category() {
        assert_eq!(hit_place(SettingsPage::Playback), "Playback");
        assert_eq!(hit_place(SettingsPage::Markers), "SponsorBlock & DeArrow \u{203A} Segment markers");
    }
}

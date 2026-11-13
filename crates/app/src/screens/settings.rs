//! Settings: a root page of categories and one page per category. Each option row shows its
//! value; OK or Right steps forward, Left steps back. Changes are saved immediately.

use crate::host::{SettingsPage, YtArg, YtFx, YtHost};
use crate::settings::{secs_label, Codec, ControlsTime, OkAction, PlaybackMode, RememberSpeed, SbAction, Settings, SLEEP_MINS, SPEEDS};
use crate::store::Section;
use crate::ui::{self, Row, VList};
use plx_machine::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, Key, LogicalState, Machine, NavOp, ScreenEvent,
};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use std::borrow::Cow;

const G: GroupId = GroupId(1);
const BASE: u32 = 0x3000_0000;

type Get = fn(&Settings) -> String;
type Change = fn(&mut Settings, i32);

enum Item {
    Page(&'static str, SettingsPage),
    Opt(&'static str, Get, Change),
    Action(&'static str, fn(&mut Effects<'_, YtHost>)),
    Info(&'static str, fn(&Cx<'_, YtHost>) -> String),
    /// A blocked channel (by index in the settings): OK unblocks it.
    Blocked(usize),
}

fn cycle_key(k: &mut String, d: i32) {
    const A: &[&str] = &["none", "search", "settings", "accounts", "subtitles", "speed"];
    let i = A.iter().position(|x| *x == k).unwrap_or(0) as i32;
    *k = A[((i + d).rem_euclid(A.len() as i32)) as usize].into();
}

/// Card-menu row value: its position, or hidden.
fn card_item_status(s: &Settings, key: &str) -> String {
    if s.card_menu_hidden.iter().any(|h| h == key) {
        return "hidden".into();
    }
    match s.card_menu.iter().position(|k| k == key) {
        Some(i) => format!("#{}", i + 1),
        None => "shown".into(),
    }
}

/// LEFT moves the item one place up; OK / RIGHT shows or hides it.
fn card_item_edit(s: &mut Settings, key: &str, d: i32) {
    if !s.card_menu.iter().any(|k| k == key) {
        s.card_menu.push(key.to_string());
    }
    if d < 0 {
        if let Some(i) = s.card_menu.iter().position(|k| k == key).filter(|i| *i > 0) {
            s.card_menu.swap(i, i - 1);
        }
    } else if let Some(i) = s.card_menu_hidden.iter().position(|h| h == key) {
        s.card_menu_hidden.remove(i);
    } else {
        s.card_menu_hidden.push(key.to_string());
    }
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
            Page("Account", SettingsPage::About),
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
            Opt("Playback engine", |s| if s.engine == "auto" { "4K / VP9 / AV1 (engine B)".into() } else { "System player".into() }, |s, _| {
                s.engine = if s.engine == "auto" { "system".into() } else { "auto".into() }
            }),
            Opt("When a video ends", |s| s.playback_mode.label().into(), |s, d| s.playback_mode = cycle(PlaybackMode::ALL, s.playback_mode, d)),
            Opt("Default speed", |s| format!("{}x", s.speed), |s, d| {
                let i = crate::settings::speed_index(s.speed) as i32;
                s.speed = SPEEDS[((i + d).rem_euclid(SPEEDS.len() as i32)) as usize];
            }),
            Opt("Remember speed", |s| s.remember_speed.label().into(), |s, d| s.remember_speed = cycle(RememberSpeed::ALL, s.remember_speed, d)),
            Opt("OK button during playback", |s| s.ok_action.label().into(), |s, d| s.ok_action = cycle(OkAction::ALL, s.ok_action, d)),
            Opt("Hide controls after", |s| if s.hud_timeout_secs == 0 { "Never".into() } else { secs_label(s.hud_timeout_secs) }, |s, d| {
                s.hud_timeout_secs = cycle(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15], s.hud_timeout_secs, d)
            }),
            Opt("Seek step", |s| secs_label(s.seek_step_secs), |s, d| s.seek_step_secs = cycle(&[1, 2, 3, 5, 7, 10, 15, 20, 30, 60], s.seek_step_secs, d)),
            Opt("Seek with number keys", |s| on_off(s.seek_number_keys), |s, _| s.seek_number_keys = !s.seek_number_keys),
            Opt("Time in the controls", |s| s.controls_time.label().into(), |s, d| s.controls_time = cycle(ControlsTime::ALL, s.controls_time, d)),
            Opt("Clock in player", |s| on_off(s.show_clock), |s, _| s.show_clock = !s.show_clock),
            Opt("Sleep timer", |s| crate::settings::sleep_label(s.sleep_timer_mins), |s, d| s.sleep_timer_mins = cycle(SLEEP_MINS, s.sleep_timer_mins, d)),
            Opt("Resume where you left off", |s| on_off(s.resume_playback), |s, _| s.resume_playback = !s.resume_playback),
            Opt("Resume short videos (under 5 minutes)", |s| on_off(s.resume_short_videos), |s, _| s.resume_short_videos = !s.resume_short_videos),
            Opt("Remember live stream position", |s| on_off(s.resume_live), |s, _| s.resume_live = !s.resume_live),
            Opt("Loop Shorts", |s| on_off(s.loop_shorts), |s, _| s.loop_shorts = !s.loop_shorts),
            Opt("Leave the player with a double BACK", |s| on_off(s.double_back_exit), |s, _| s.double_back_exit = !s.double_back_exit),
            Opt("Show the controls when the next video starts", |s| on_off(s.hud_on_next_video), |s, _| s.hud_on_next_video = !s.hud_on_next_video),
            Opt("Preferred audio language", |s| if s.audio_language.is_empty() { "Original".into() } else { language_name(&s.audio_language) }, |s, d| {
                let mut l = vec![""];
                l.extend(language_codes());
                cycle_str(&l, &mut s.audio_language, d);
            }),
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
            Opt("Position from bottom", |s| format!("{} px", s.subtitle_bottom_offset), |s, d| s.subtitle_bottom_offset = cycle(&[40, 90, 140, 200], s.subtitle_bottom_offset, d)),
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
            Opt("Hide watched videos on Home", |s| on_off(s.hide_watched_home), |s, _| s.hide_watched_home = !s.hide_watched_home),
            Opt("Hide watched videos in Subscriptions", |s| on_off(s.hide_watched_subscriptions), |s, _| {
                s.hide_watched_subscriptions = !s.hide_watched_subscriptions
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
            Opt("Search history", |s| on_off(s.search_history), |s, _| {
                s.search_history = !s.search_history;
                if !s.search_history {
                    let _ = yt_core::search_history::clear();
                }
            }),
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
                Opt("Screen size", |s| format!("{}%", s.ui_scale), |s, d| s.ui_scale = cycle(&[100u8, 97, 95, 92, 90], s.ui_scale, d)),
                Opt("Titles under thumbnails", |s| on_off(s.titles_under), |s, _| s.titles_under = !s.titles_under),
                Opt("Card size", |s| match s.card_columns { 3 => "large".into(), 5 => "small".into(), _ => "medium".into() }, |s, d| {
                    s.card_columns = cycle(&[3u8, 4, 5], s.card_columns, d)
                }),
                Opt("Interface language", |s| language_name(&s.ui_language), |s, d| {
                    let mut l = vec!["system", "en"];
                    l.extend(crate::i18n::languages());
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
                Opt("24-hour clock", |s| on_off(s.time_24h), |s, _| s.time_24h = !s.time_24h),
            Opt("Accent colour", |s| s.accent.clone(), |s, d| {
                const C: &[&str] = &["red", "blue", "green", "purple", "orange"];
                let i = C.iter().position(|c| *c == s.accent).unwrap_or(0) as i32;
                s.accent = C[((i + d).rem_euclid(C.len() as i32)) as usize].into();
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
                            let order: Vec<&str> = Section::ALL.iter().map(|x| x.key()).collect();
                            s.sections.push(k);
                            s.sections.sort_by_key(|x| order.iter().position(|o| o == x).unwrap_or(99));
                        }
                    })
                };
            }
            v.extend([
                section_toggle!("Sidebar: Home", Section::Home),
                section_toggle!("Sidebar: Subscriptions", Section::Subscriptions),
                section_toggle!("Sidebar: Channels", Section::Channels),
                section_toggle!("Sidebar: History", Section::History),
                section_toggle!("Sidebar: Library", Section::Library),
                section_toggle!("Sidebar: Music", Section::Music),
                section_toggle!("Sidebar: Gaming", Section::Gaming),
                section_toggle!("Sidebar: News", Section::News),
                section_toggle!("Sidebar: Podcasts", Section::Podcasts),
                section_toggle!("Sidebar: Live", Section::Live),
                section_toggle!("Sidebar: Movies", Section::Movies),
                section_toggle!("Sidebar: Sports", Section::Sports),
                section_toggle!("Sidebar: Trending", Section::Trending),
            ]);
            macro_rules! card_item {
                ($i:expr) => {
                    Opt(super::menu::CARD_ITEMS[$i].1, |s| card_item_status(s, super::menu::CARD_ITEMS[$i].0), |s, d| {
                        card_item_edit(s, super::menu::CARD_ITEMS[$i].0, d)
                    })
                };
            }
            v.extend([
                card_item!(0),
                card_item!(1),
                card_item!(2),
                card_item!(3),
                card_item!(4),
                card_item!(5),
                card_item!(6),
                card_item!(7),
                card_item!(8),
                card_item!(9),
                card_item!(10),
                card_item!(11),
                card_item!(12),
                card_item!(13),
            ]);
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
            Opt("DNS over HTTPS (after restart)", |s| s.doh.clone(), |s, d| {
                const D: &[&str] = &["off", "cloudflare", "google", "quad9"];
                let i = D.iter().position(|x| *x == s.doh).unwrap_or(0) as i32;
                s.doh = D[((i + d).rem_euclid(D.len() as i32)) as usize].into();
            }),
            Info("Proxy (proxy_url in settings.json)", |cx| {
                let p = &cx.views.store.settings.proxy_url;
                if p.is_empty() { "none".into() } else { p.clone() }
            }),
            Opt("Show in the phone's cast menu (after restart)", |s| on_off(s.dial_enabled), |s, _| s.dial_enabled = !s.dial_enabled),
            Info("TV code", |cx| match &cx.views.store.lounge_code {
                Some(c) => c.clone(),
                None if cx.views.store.settings.lounge_enabled => "getting a code\u{2026}".into(),
                None => "off".into(),
            }),
            Info("On your phone", |_| "YouTube \u{203A} Cast \u{203A} Link with TV code".into()),
        ],
        SettingsPage::About => vec![
            Action("Accounts (switch, add, remove)", |fx| fx.push(Fx::Nav(NavOp::Push(YtArg::Accounts)))),
            Action("Sign in with a code", |fx| fx.push(Fx::Nav(NavOp::Push(YtArg::SignIn)))),
            Action("Sign out", |fx| {
                fx.push(Fx::App(YtFx::SignOut));
                fx.push(Fx::App(YtFx::Toast("Signed out".into())));
            }),
            Action("Back up settings (USB or app storage)", |fx| fx.push(Fx::App(YtFx::Act(crate::host::UserAction::Backup)))),
            Action("Restore latest backup", |fx| fx.push(Fx::App(YtFx::Act(crate::host::UserAction::Restore)))),
            Info("Version", |_| env!("CARGO_PKG_VERSION").into()),
            Info("Source", |_| "github.com/danzig666/ytnative".into()),
        ],
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
    page: SettingsPage,
    list: VList,
    /// An option's drop-down list, while open.
    choice: Option<ui::ChoiceBox>,
    /// The row a closed drop-down belonged to (focus returns there).
    closed_row: std::cell::Cell<usize>,
    state: State,
}

impl SettingsScreen {
    pub fn new(entry: EntryId, page: SettingsPage) -> Self {
        SettingsScreen {
            page,
            choice: None,
            closed_row: std::cell::Cell::new(0),
            list: VList { entry, group: G, base: BASE, rect: Rect::new(360.0, 150.0, 1200.0, 880.0), row_h: 58.0, text_size: 28 },
            state: State(format!("{page:?}")),
        }
    }

    /// The values option `i` can take, from its own `change` (applied to a copy until it comes
    /// back round), and the current one's index.
    fn values(&self, i: usize, cx: &Cx<'_, YtHost>) -> Option<(Vec<String>, usize)> {
        let Some(Item::Opt(_, get, change)) = items(self.page, &cx.views.store.settings).into_iter().nth(i) else { return None };
        let mut s = cx.views.store.settings.clone();
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
        // Numbers (qualities, speeds, sizes, minutes) in ascending order; others in cycle order.
        let num = |x: &str| x.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect::<String>().parse::<f64>().ok();
        if v.len() > 2 && v.iter().all(|x| num(x).is_some()) {
            v.sort_by(|a, b| num(a).partial_cmp(&num(b)).unwrap_or(std::cmp::Ordering::Equal));
        }
        let cur = v.iter().position(|x| *x == first).unwrap_or(0);
        Some((v, cur))
    }

    fn change(&self, i: usize, dir: i32, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        let list = items(self.page, &cx.views.store.settings);
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
            _ => {}
        }
    }
}

impl Machine<YtHost> for SettingsScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        if let Some(a) = super::common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        let n = items(self.page, &cx.views.store.settings).len();
        match ev {
            ScreenEvent::Activate(e) if self.choice.as_ref().is_some_and(|c| c.owns(*e)) => {
                let c = self.choice.take().unwrap();
                self.closed_row.set(c.row);
                let k = c.list.index(*e, c.options.len()).unwrap_or(0);
                if let Some(Item::Opt(_, get, change)) = items(self.page, &cx.views.store.settings).into_iter().nth(c.row) {
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
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Left | Key::Right, .. }, .. }) if self.choice.is_some() => Handled::Yes,
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: key @ (Key::Left | Key::Right), edge: Edge::Down | Edge::Repeat, .. }, .. }) => {
                if let Some(i) = self.list.focused(cx, n) {
                    if matches!(items(self.page, &cx.views.store.settings).get(i), Some(Item::Opt(..))) {
                        self.change(i, if *key == Key::Left { -1 } else { 1 }, cx, fx);
                        return Handled::Yes;
                    }
                }
                Handled::No
            }
            ScreenEvent::Uncover => {
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
        out.push(self.list.spec(items(self.page, &cx.views.store.settings).len()));
    }
    fn group_of(&self, key: &u32, _cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        Some(if self.choice.as_ref().is_some_and(|c| c.owns(*key)) { ui::CHOICE_GROUP } else { G })
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, YtHost>) -> Step<u32> {
        if let Some(c) = &self.choice {
            return c.list.neighbour(key, dir, c.options.len());
        }
        self.list.neighbour(key, dir, items(self.page, &cx.views.store.settings).len())
    }
    fn place(&self, key: &u32, cx: &Cx<'_, YtHost>, _at: At) -> Option<Placed> {
        if let Some(c) = self.choice.as_ref().filter(|c| c.owns(*key)) {
            let n = c.options.len();
            return c.list.place(*key, n, c.list.focused(cx, n));
        }
        let n = items(self.page, &cx.views.store.settings).len();
        self.list.place(*key, n, self.list.focused(cx, n))
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        if let Some(c) = &self.choice {
            return if c.owns(want.elem) { want } else { c.list.key(c.current) };
        }
        if want.elem >= ui::CHOICE_BASE {
            return self.list.key(self.closed_row.get());
        }
        if self.list.index(want.elem, items(self.page, &cx.views.store.settings).len()).is_some() { want } else { self.list.key(0) }
    }
    fn seat(&self, g: GroupId, _from: Placed, _cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        if let (Some(c), true) = (&self.choice, g == ui::CHOICE_GROUP) {
            return c.list.key(c.current);
        }
        self.list.key(0)
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
        ui::text(p, title(self.page), 360.0, 70.0, 48, ui::WHITE, ui::ALIGN_LEFT, true);
        let s = &cx.views.store.settings;
        let rows: Vec<Row> = items(self.page, s)
            .iter()
            .map(|it| match it {
                Item::Blocked(i) => Row::new(s.blocked_channels.get(*i).cloned().unwrap_or_default()).value(crate::i18n::tr("Unblock")),
                Item::Page(l, _) => Row::new(*l).value("\u{203A}"),
                Item::Opt(l, get, _) => Row::new(*l).value(format!("{}  \u{203A}", crate::i18n::tr(&get(s)))),
                Item::Action(l, _) => Row::new(*l),
                Item::Info(l, get) => Row::new(*l).value(get(cx)),
            })
            .collect();
        self.list.draw(p, &rows, self.list.focused(cx, rows.len()));
        if let Some(c) = &self.choice {
            c.draw(f);
        } else {
            self.list.stops(f, rows.len());
        }
        super::common::toast(p, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

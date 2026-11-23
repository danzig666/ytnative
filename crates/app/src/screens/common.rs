//! Pieces several screens share.

use crate::host::{YtArg, YtFx, YtHost, Views};
use crate::play_queue::PlayQueue;
use crate::store::{Card, Feed, FeedKey};
use crate::ui;
use plx_machine::machine::{Cx, Effects, Fx, NavOp};
use plx_ui::{Painter, Rect};
use yt_core::model::{Endpoint, ItemKind};

/// Opens a card that sits in `list`: remembers the list as the play queue first. A card that
/// plays inside a playlist of its own (a mix, a playlist card that starts its first video) starts
/// that playlist instead.
pub fn open_card_in(card: &Card, list: &[Card], fx: &mut Effects<'_, YtHost>) {
    if let Some(v) = card.video_id() {
        let q = match &card.item.endpoint {
            Some(Endpoint::Watch { playlist_id: Some(pl), .. }) if !pl.is_empty() => {
                let mut q = PlayQueue::playlist(Some(pl.clone()), card_playlist_title(card), vec![v.to_string()]);
                if card.item.kind == ItemKind::Video {
                    q.add_items(vec![card.item.clone()], None, 0);
                }
                q
            }
            _ => {
                let shorts = card.item.kind == ItemKind::Short;
                let q: Vec<&Card> = list.iter().filter(|c| !shorts || c.item.kind == ItemKind::Short).collect();
                let mut queue = PlayQueue::list(Vec::new(), shorts);
                queue.add_items(q.iter().filter_map(|c| card_video(c)).collect(), None, 0);
                queue
            }
        };
        fx.push(Fx::App(YtFx::SetQueue(q)));
    }
    open_card(card, fx);
}

/// The title a mix / playlist card names its list by (a mix's "Mix – <artist>"); a video card in
/// a playlist has none (the watch page's playlist panel tells it).
fn card_playlist_title(card: &Card) -> String {
    match card.item.kind {
        ItemKind::Mix | ItemKind::Playlist => card.item.title.clone(),
        _ => String::new(),
    }
}

/// A card's video as an item for the play queue (its id the video's, whatever the card's own id).
fn card_video(c: &Card) -> Option<yt_core::model::Item> {
    let v = c.video_id()?;
    Some(yt_core::model::Item { id: v.to_string(), title: c.title.clone(), ..c.item.clone() })
}

/// Opens a card of a playlist: the YouTube playlist `playlist_id` ("VL" taken off; None for a
/// playlist on this TV) titled `title`, its videos those of `list` so far (the rest of a YouTube
/// playlist is loaded in the background). Next / Previous and every playback mode follow it.
pub fn open_card_in_playlist(card: &Card, list: &[Card], playlist_id: Option<&str>, title: &str, fx: &mut Effects<'_, YtHost>) {
    if card.video_id().is_some() {
        fx.push(Fx::App(YtFx::SetQueue(playlist_queue(list, playlist_id, title))));
    }
    open_card(card, fx);
}

/// The play queue of a playlist's page ([`open_card_in_playlist`]).
pub fn playlist_queue(list: &[Card], playlist_id: Option<&str>, title: &str) -> PlayQueue {
    let mut q = PlayQueue::playlist(playlist_id.map(str::to_string), title.to_string(), Vec::new());
    q.add_items(list.iter().filter_map(card_video).collect(), None, 0);
    q
}

/// OK on a `Control` element (the sidebar, `VList` rows, the search keyboard) arms a press in the
/// dispatcher, and its release is delivered as `PressCommit` — never as `Activate`, which only a
/// `Bare` element gets (plx_ui `screen::ElemKind`). The screens are written against `Activate`, so
/// this turns a commit on one of `mine`'s elements into `Activate` of the focused element.
/// (Found with the Windows simulator: without it OK on those elements did nothing.)
pub fn press_commit_as_activate(
    ev: &plx_machine::machine::ScreenEvent<YtHost>,
    cx: &Cx<'_, YtHost>,
    mine: impl Fn(u32) -> bool,
) -> Option<plx_machine::machine::ScreenEvent<YtHost>> {
    match ev {
        plx_machine::machine::ScreenEvent::PressCommit(_) => {
            cx.focus.current.map(|k| k.elem).filter(|e| mine(*e)).map(plx_machine::machine::ScreenEvent::Activate)
        }
        _ => None,
    }
}

/// What activating a card does: play a video, open a channel / playlist, run a search.
pub fn open_card(card: &Card, fx: &mut Effects<'_, YtHost>) {
    let arg = match (&card.item.endpoint, card.item.kind) {
        (Some(Endpoint::Watch { video_id, playlist_id, .. }), _) if !video_id.is_empty() => {
            YtArg::Player { video_id: video_id.clone(), playlist: playlist_id.clone() }
        }
        (Some(Endpoint::Watch { playlist_id: Some(pl), .. }), _) => YtArg::Feed(FeedKey::Browse { id: format!("VL{pl}"), params: None }),
        (Some(Endpoint::Browse { browse_id, params }), _) => YtArg::Feed(FeedKey::browse(browse_id, params.clone())),
        (Some(Endpoint::Search { query, params }), _) => YtArg::Feed(FeedKey::Search { query: query.clone(), params: params.clone() }),
        (None, ItemKind::Video | ItemKind::Short | ItemKind::Live | ItemKind::Upcoming) => {
            YtArg::Player { video_id: card.item.id.clone(), playlist: None }
        }
        (None, ItemKind::Channel) => YtArg::Feed(FeedKey::Browse { id: card.item.id.clone(), params: None }),
        (None, ItemKind::Playlist | ItemKind::Mix) => YtArg::Feed(FeedKey::Browse { id: format!("VL{}", card.item.id), params: None }),
        _ => {
            fx.push(Fx::App(YtFx::Toast("Can't open this item".into())));
            return;
        }
    };
    fx.push(Fx::Nav(NavOp::Push(arg)));
}

/// The app's wordmark: a small red play badge and "ytnative".
pub fn logo(p: Painter, x: f32, y: f32) {
    ui::fill(p, Rect::new(x, y, 54.0, 38.0), 10.0, ui::accent());
    ui::text(p, "\u{25B6}", x + 27.0, y + 3.0, 26, ui::WHITE, ui::ALIGN_CENTER, true);
    ui::text(p, "ytnative", x + 68.0, y + 1.0, 32, ui::WHITE, ui::ALIGN_LEFT, true);
}

/// Right on the last card of a grid row: the first card of the next row (reading order), as a
/// page of videos with no sideways scrolling reads. `None` on the very last card.
pub fn next_row_start(cards: &[Card], key: plx_machine::machine::FocusKey<u32>) -> Option<plx_machine::machine::FocusKey<u32>> {
    let i = cards.iter().position(|c| c.elem == key.elem)?;
    cards.get(i + 1).map(|c| plx_machine::machine::FocusKey { entry: key.entry, elem: c.elem })
}

/// What a page without cards says: the Notifications section explains how channels get there,
/// other pages say there is nothing yet.
pub fn empty_text(key: &FeedKey) -> (&'static str, &'static str) {
    match key {
        FeedKey::Section(crate::store::Section::Notifications) if yt_core::bell_channels::is_empty() => (
            "No channels with notifications yet",
            "Turn on Notify me of new videos from this channel in the player's options to see its new videos here.",
        ),
        _ => ("Nothing here yet", ""),
    }
}

/// A spinner (loading) or an error / empty message in place of a grid.
///
/// `empty`: what a loaded page without cards says, and the line under it ([`empty_text`]).
pub fn empty_state(p: Painter, m: &dyn plx_machine::machine::Measure, feed: Option<&Feed>, empty: (&str, &str), area: Rect, now_ms: u32) {
    let (msg, sub) = match feed {
        None => ("", ""),
        Some(f) if f.loading => ("", ""),
        Some(f) if f.error.is_some() => ("Couldn't load this page", "Press OK on the section to retry."),
        Some(_) => empty,
    };
    if msg.is_empty() {
        ui::spinner(p, now_ms, area.x + area.w / 2.0, area.y + area.h / 2.0 - 20.0);
        return;
    }
    ui::text(p, msg, area.x + area.w / 2.0, area.y + area.h / 2.0 - 40.0, 36, ui::DIM, ui::ALIGN_CENTER, false);
    if !sub.is_empty() {
        ui::text(p, sub, area.x + area.w / 2.0, area.y + area.h / 2.0 + 10.0, 26, ui::FAINT, ui::ALIGN_CENTER, false);
    }
    if let Some(e) = feed.and_then(|f| f.error.as_deref()) {
        ui::text_cut(p, m, e, area.x + area.w / 2.0, area.y + area.h / 2.0 + 56.0, area.w - 160.0, 20, ui::FAINT, ui::ALIGN_CENTER, false);
    }
}

/// A transient message at the bottom of the screen.
pub fn toast(p: Painter, cx: &Cx<'_, YtHost>) {
    toast_at(p, cx, 960.0);
}

/// [`toast`] with its top at `y` (the player raises it above its controls).
pub fn toast_at(p: Painter, cx: &Cx<'_, YtHost>, y: f32) {
    let Some((msg, _)) = &cx.views.store.toast else { return };
    let w = toast_width(crate::i18n::tr(msg));
    let r = Rect::new((1920.0 - w) / 2.0, y, w, 70.0);
    ui::fill(p, r, 35.0, [0.08, 0.08, 0.1, 0.94]);
    ui::text(p, msg, 960.0, r.y + 19.0, 28, ui::WHITE, ui::ALIGN_CENTER, false);
}

/// A toast's pill width for `text` at 28 px: Latin-like letters about 14 px, the full-width ones
/// of Chinese, Japanese and Korean about 28 px; at most the screen.
fn toast_width(text: &str) -> f32 {
    let wide = |c: char| matches!(c as u32, 0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6);
    let w: f32 = text.chars().map(|c| if wide(c) { 28.0 } else { 13.64 }).sum();
    (w + 80.0).min(1840.0)
}

/// The time with the date under it, right-aligned at `right` (SmartTube's corner clock).
pub fn clock_block(p: Painter, cx: &Cx<'_, YtHost>, right: f32, top: f32) {
    ui::text(p, &clock(&cx.views), right, top, 40, ui::WHITE, ui::ALIGN_RIGHT, true);
    ui::text(p, &date_text(), right, top + 50.0, 22, ui::DIM, ui::ALIGN_RIGHT, false);
}

/// How old a video is, for the player and the video info: YouTube's short age ("3d ago"), or for
/// a video a year old or more its date in words ("November 10, 2014").
pub fn video_age(w: &yt_core::watch::WatchInfo) -> Option<String> {
    let today = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |t| t.as_secs() / 86_400) as i64;
    video_age_on(w, today)
}

/// [`video_age`] on day `today` (days since 1970-01-01).
fn video_age_on(w: &yt_core::watch::WatchInfo, today: i64) -> Option<String> {
    if let Some((y, m, d)) = w.published_date {
        if today - days_from_civil(y, m, d) >= 365 {
            return Some(crate::i18n::full_date(y, m, d));
        }
    }
    w.published_text.clone().filter(|t| !t.is_empty())
}

/// Days since 1970-01-01 of a calendar date (Howard Hinnant's algorithm).
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod age_tests {
    #[test]
    fn civil_days() {
        assert_eq!(super::days_from_civil(1970, 1, 1), 0);
        assert_eq!(super::days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(super::days_from_civil(2014, 11, 10), 16_384);
    }

    fn watch(text: &str, date: Option<(i32, u32, u32)>) -> yt_core::watch::WatchInfo {
        yt_core::watch::WatchInfo { published_text: Some(text.into()), published_date: date, ..Default::default() }
    }

    #[test]
    fn a_video_a_year_old_or_more_shows_its_date() {
        let today = super::days_from_civil(2026, 11, 15);
        let old = super::video_age_on(&watch("12 years ago", Some((2014, 11, 10))), today).unwrap();
        assert!(old.contains("2014") && old.contains("10") && !old.contains("ago"), "{old}");
        let exactly = super::video_age_on(&watch("1 year ago", Some((2025, 11, 15))), today).unwrap();
        assert!(exactly.contains("2025"), "365 days counts as a year: {exactly}");
    }

    #[test]
    fn a_newer_video_keeps_youtubes_short_age() {
        let today = super::days_from_civil(2026, 11, 15);
        assert_eq!(super::video_age_on(&watch("3 days ago", Some((2026, 11, 12))), today).as_deref(), Some("3 days ago"));
        assert_eq!(super::video_age_on(&watch("11 months ago", Some((2025, 11, 16))), today).as_deref(), Some("11 months ago"));
        assert_eq!(super::video_age_on(&watch("Streamed live", None), today).as_deref(), Some("Streamed live"), "no date: the text");
        assert_eq!(super::video_age_on(&watch("", None), today), None);
    }

    #[test]
    fn leap_days_count() {
        assert_eq!(super::days_from_civil(2024, 3, 1) - super::days_from_civil(2024, 2, 28), 2);
        assert_eq!(super::days_from_civil(2100, 3, 1) - super::days_from_civil(2100, 2, 28), 1, "2100 is not a leap year");
    }
}

/// Today's date written out in the interface language ("Friday, 13 November"; no abbreviations).
pub fn date_text() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let days = (secs + local_offset_secs()).div_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as usize; // 1..=12
    let weekday = (days + 4).rem_euclid(7) as usize; // 0 = Sunday
    crate::i18n::date_words(weekday, day as u32, month)
}

/// Wall-clock "HH:MM" (local time is the TV's; UTC fallback).
pub fn clock(views: &Views<'_>) -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let local = secs + local_offset_secs();
    let (h, m) = ((local / 3600).rem_euclid(24), (local / 60).rem_euclid(60));
    clock_text(h, m, views.store.settings.time_24h)
}

/// The wall-clock time (as [`clock`] shows it) of `secs` since 1970: when a chat message was
/// sent.
pub fn clock_at(views: &Views<'_>, secs: i64) -> String {
    let local = secs + local_offset_secs();
    let (h, m) = ((local / 3600).rem_euclid(24), (local / 60).rem_euclid(60));
    clock_text(h, m, views.store.settings.time_24h)
}

/// "HH:MM" on the 24-hour clock, or (English with the 12-hour clock chosen) the hour on the
/// 12-hour clock with the part of the day in words ("9:41 in the evening"); never "AM" / "PM".
/// The other interface languages tell the time on the 24-hour clock.
pub fn clock_text(h: i64, m: i64, time_24h: bool) -> String {
    if time_24h || crate::i18n::current() != "en" {
        return format!("{h:02}:{m:02}");
    }
    let h12 = if h % 12 == 0 { 12 } else { h % 12 };
    let part = match h {
        5..=11 => "in the morning",
        12..=17 => "in the afternoon",
        18..=21 => "in the evening",
        _ => "at night",
    };
    format!("{h12}:{m:02} {}", crate::i18n::tr(part))
}

#[cfg(windows)]
fn local_offset_secs() -> i64 {
    crate::sim::local_offset_secs()
}

#[cfg(not(windows))]
fn local_offset_secs() -> i64 {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            0
        } else {
            tm.tm_gmtoff as i64
        }
    }
}

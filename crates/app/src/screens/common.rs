//! Pieces several screens share.

use crate::host::{YtArg, YtFx, YtHost, Views};
use crate::store::{Card, Feed, FeedKey};
use crate::ui;
use plx_machine::machine::{Cx, Effects, Fx, NavOp};
use plx_ui::{Painter, Rect};
use yt_core::model::{Endpoint, ItemKind};

/// Opens a card that sits in `list`: remembers the list as the play queue first.
pub fn open_card_in(card: &Card, list: &[Card], fx: &mut Effects<'_, YtHost>) {
    if card.video_id().is_some() {
        let shorts = card.item.kind == ItemKind::Short;
        let q: Vec<String> = list
            .iter()
            .filter(|c| !shorts || c.item.kind == ItemKind::Short)
            .filter_map(|c| c.video_id().map(str::to_string))
            .collect();
        fx.push(Fx::App(YtFx::SetQueue(q, shorts)));
    }
    open_card(card, fx);
}

/// What activating a card does: play a video, open a channel / playlist, run a search.
pub fn open_card(card: &Card, fx: &mut Effects<'_, YtHost>) {
    let arg = match (&card.item.endpoint, card.item.kind) {
        (Some(Endpoint::Watch { video_id, playlist_id, .. }), _) if !video_id.is_empty() => {
            YtArg::Player { video_id: video_id.clone(), playlist: playlist_id.clone() }
        }
        (Some(Endpoint::Watch { playlist_id: Some(pl), .. }), _) => YtArg::Feed(FeedKey::Browse { id: format!("VL{pl}"), params: None }),
        (Some(Endpoint::Browse { browse_id, params }), _) => YtArg::Feed(FeedKey::Browse { id: browse_id.clone(), params: params.clone() }),
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

/// Loading / error / empty message in place of a grid.
pub fn empty_state(p: Painter, feed: Option<&Feed>, area: Rect) {
    let (msg, sub) = match feed {
        None => ("Loading\u{2026}", ""),
        Some(f) if f.loading => ("Loading\u{2026}", ""),
        Some(f) if f.error.is_some() => ("Couldn't load this page", "Press OK on the section to retry."),
        Some(_) => ("Nothing here yet", ""),
    };
    ui::text(p, msg, area.x + area.w / 2.0, area.y + area.h / 2.0 - 40.0, 36, ui::DIM, ui::ALIGN_CENTER, false);
    if !sub.is_empty() {
        ui::text(p, sub, area.x + area.w / 2.0, area.y + area.h / 2.0 + 10.0, 26, ui::FAINT, ui::ALIGN_CENTER, false);
    }
    if let Some(e) = feed.and_then(|f| f.error.as_deref()) {
        ui::text(p, &ui::ellipsize(e, 110), area.x + area.w / 2.0, area.y + area.h / 2.0 + 56.0, 20, ui::FAINT, ui::ALIGN_CENTER, false);
    }
}

/// A transient message at the bottom of the screen.
pub fn toast(p: Painter, cx: &Cx<'_, YtHost>) {
    let Some((msg, _)) = &cx.views.store.toast else { return };
    let w = 22.0 * msg.chars().count() as f32 * 0.62 + 80.0;
    let r = Rect::new((1920.0 - w) / 2.0, 960.0, w, 70.0);
    ui::fill(p, r, 35.0, [0.08, 0.08, 0.1, 0.94]);
    ui::text(p, msg, 960.0, r.y + 19.0, 28, ui::WHITE, ui::ALIGN_CENTER, false);
}

/// Wall-clock "HH:MM" (local time is the TV's; UTC fallback).
pub fn clock(views: &Views<'_>) -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let local = secs + local_offset_secs();
    let (h, m) = ((local / 3600).rem_euclid(24), (local / 60).rem_euclid(60));
    if views.store.settings.time_24h {
        format!("{h:02}:{m:02}")
    } else {
        let h12 = if h % 12 == 0 { 12 } else { h % 12 };
        format!("{h12}:{m:02} {}", if h < 12 { "AM" } else { "PM" })
    }
}

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

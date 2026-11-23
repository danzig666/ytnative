//! The TV's "Recommended" row: this app's cards in the application manager's content list.
//!
//! LG Home — and the third-party homes built on the same call — read
//! `com.webos.service.applicationManager/getAllContents`, which every app fills for its own launch
//! point with `setContents`. A click comes back as `launch {id, params:{contentTarget}}`, which
//! `deeplink` in `main.rs` already opens (`https://www.youtube.com/tv?v=<id>`). Nothing here goes
//! through LG's servers or the viewer's account.
//!
//! Off by default (Settings › Interface › "Recommendations on the TV's Home"). While it is on, the
//! newest videos of the Subscriptions feed (Home's first row when signed out) are published a
//! minute after a start and then every 30 minutes; switched off, the cards are removed again.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use yt_core::model::{Item, ItemKind, Page};

/// Cards published at most: the stock home shows 16 an app.
const MAX_CARDS: usize = 16;
/// The first publish waits this long after the app starts (the first screen comes first).
const FIRST_DELAY_MS: u32 = 60_000;
const EVERY_MS: u32 = 30 * 60_000;
const SET_CONTENTS: &str = "luna://com.webos.service.applicationManager/setContents";
const REMOVE_ALL: &str = "luna://com.webos.service.applicationManager/removeAllContents";

static ENABLED: AtomicBool = AtomicBool::new(false);
/// A publish is running: the next one waits for it.
static BUSY: AtomicBool = AtomicBool::new(false);
/// The TV refused the call once: no retry until the setting is switched again (see `publish`).
static GAVE_UP: AtomicBool = AtomicBool::new(false);
/// Frame clock (ms) of the next publish; 0 = none armed.
static NEXT_AT: AtomicU32 = AtomicU32::new(0);

/// The app's own launch point: what its cards hang from (`listLaunchPoints` names it so).
fn launch_point() -> String {
    format!("{}_default", plx_base::paths::app_id())
}

/// At start: arms the first publish when the setting is on.
pub fn start(enabled: bool, now_ms: u32) {
    ENABLED.store(enabled, Ordering::SeqCst);
    if enabled {
        NEXT_AT.store(now_ms.wrapping_add(FIRST_DELAY_MS), Ordering::SeqCst);
    }
}

/// Every frame: publishes when the half hour has passed (and never on the simulator).
pub fn tick(now_ms: u32) {
    if cfg!(windows) || !ENABLED.load(Ordering::Relaxed) || BUSY.load(Ordering::Relaxed) || GAVE_UP.load(Ordering::Relaxed) {
        return;
    }
    let due = NEXT_AT.load(Ordering::Relaxed);
    if due == 0 || now_ms.wrapping_sub(due) >= 0x8000_0000 {
        return;
    }
    BUSY.store(true, Ordering::SeqCst);
    NEXT_AT.store(now_ms.wrapping_add(EVERY_MS), Ordering::SeqCst);
    let started = plx_base::task::spawn("home recommendations", || {
        match cards() {
            Ok(items) if !items.is_empty() => {
                if let Err(e) = publish(&items) {
                    // A TV that refuses the call refuses it every time: say so once and stop.
                    GAVE_UP.store(true, Ordering::SeqCst);
                    crate::log(&format!("recommendations: the TV refused the card list, not retrying: {e}"));
                } else {
                    crate::log(&format!("recommendations: {} cards published", items.len()));
                }
            }
            Ok(_) => crate::log("recommendations: nothing to publish (no videos in the feed)"),
            Err(e) => crate::log(&format!("recommendations: no card list this time: {e}")),
        }
        BUSY.store(false, Ordering::SeqCst);
    });
    if started.is_none() {
        BUSY.store(false, Ordering::SeqCst);
    }
}

/// The setting changed: on publishes at once, off removes the cards.
pub fn set_enabled(enabled: bool, now_ms: u32) {
    let was = ENABLED.swap(enabled, Ordering::SeqCst);
    if enabled {
        GAVE_UP.store(false, Ordering::SeqCst);
        if !was {
            NEXT_AT.store(now_ms, Ordering::SeqCst);
        }
    } else if was {
        NEXT_AT.store(0, Ordering::SeqCst);
        clear();
    }
}

/// Removes this app's cards, leaving every other app's list alone.
fn clear() {
    if cfg!(windows) {
        return;
    }
    plx_base::task::spawn("home recommendations: clear", || {
        let payload = serde_json::json!({ "launchPoints": [launch_point()] }).to_string();
        match call(REMOVE_ALL, &payload) {
            Ok(()) => crate::log("recommendations: cards removed"),
            Err(e) => crate::log(&format!("recommendations: the cards were not removed: {e}")),
        }
    });
}

/// The newest videos of the Subscriptions feed, or Home's first row when signed out.
fn cards() -> anyhow::Result<Vec<Item>> {
    let mut session = yt_core::session::Session::new()?;
    let page = if session.signed_in() { session.subscriptions()? } else { session.home()? };
    Ok(pick(&page, session.signed_in()))
}

/// The videos of `page`, in its order: every shelf of the Subscriptions feed, Home's first row
/// alone, videos (live streams too) only, each once, at most [`MAX_CARDS`].
fn pick(page: &Page, signed_in: bool) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let shelves = if signed_in { page.shelves.as_slice() } else { page.shelves.get(..1).unwrap_or(&[]) };
    for item in shelves.iter().flat_map(|s| s.items.iter()) {
        if out.len() >= MAX_CARDS {
            break;
        }
        if !matches!(item.kind, ItemKind::Video | ItemKind::Live) || item.id.is_empty() || item.title.trim().is_empty() {
            continue;
        }
        if seen.insert(item.id.clone()) {
            out.push(item.clone());
        }
    }
    out
}

/// The cards as the TV's list takes them: `contentId` on the click is the deep link
/// `deeplink` opens, and the TV gives each card a UUID of its own.
fn payload(items: &[Item]) -> String {
    let app = plx_base::paths::app_id();
    let mut contents = vec![serde_json::json!({ "type": "title", "titleText": "" })];
    contents.extend(items.iter().map(|item| {
        let image = item.thumbnail.clone().unwrap_or_else(|| format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", item.id));
        serde_json::json!({
            "type": "content",
            "contentId": item.id,
            "titleText": item.title,
            "category": item.channel.clone().unwrap_or_else(|| crate::i18n::tr("Subscriptions").to_string()),
            "image": { "url": image, "width": 480, "height": 270 },
            "onClick": {
                "targetAppId": app,
                "contentId": format!("https://www.youtube.com/tv?v={}", item.id),
                "launchParam": "",
            },
        })
    }));
    serde_json::json!({ "launchPointId": launch_point(), "contents": contents }).to_string()
}

/// Sends the cards over the app's own bus connection (`Luna::shared`), as the stock apps do.
fn publish(items: &[Item]) -> Result<(), String> {
    call(SET_CONTENTS, &payload(items))
}

/// One luna call on the app's own connection; a TV that refuses it there (some do) gets the same
/// call on the wider connection the app already uses for Homebrew Channel and voice search.
fn call(uri: &str, payload: &str) -> Result<(), String> {
    let luna = crate::ls2::Luna::shared(plx_base::paths::app_id()).map_err(|e| format!("no bus: {e}"))?;
    match reply_ok(&luna.call(uri, payload)) {
        Err(e) if refused(&e) => {
            // Said once: on a TV that refuses the narrow role it refuses it every time.
            static SAID: AtomicBool = AtomicBool::new(false);
            if !SAID.swap(true, Ordering::Relaxed) {
                crate::log(&format!("recommendations: the app's own role was refused ({e}), using the wide connection"));
            }
            let wide = crate::ls2::Luna::wide(plx_base::paths::app_id()).map_err(|e| format!("no wide bus: {e}"))?;
            reply_ok(&wide.call(uri, payload))
        }
        r => r,
    }
}

/// The bus said no to this client (as opposed to the service being unavailable or the call failing).
fn refused(reply: &str) -> bool {
    let r = reply.to_lowercase();
    r.contains("not permitted") || r.contains("denied")
}

/// A luna reply: `returnValue: false` and a failed call are errors, with the TV's own words.
fn reply_ok(reply: &str) -> Result<(), String> {
    if reply.trim_start().starts_with('<') {
        return Err(reply.trim().to_string());
    }
    let v: serde_json::Value = serde_json::from_str(reply).map_err(|_| format!("unreadable reply: {reply}"))?;
    if v.get("returnValue").and_then(serde_json::Value::as_bool) == Some(false) {
        return Err(v.get("errorText").and_then(|t| t.as_str()).unwrap_or("refused").to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use yt_core::model::{ItemKind, Page, Shelf};

    fn item(kind: ItemKind, id: &str) -> Item {
        Item { kind, id: id.into(), title: format!("Video {id}"), channel: Some("A channel".into()), ..Item::default() }
    }

    fn page(items: Vec<Item>) -> Page {
        Page { shelves: vec![Shelf { title: "Latest".into(), items, ..Shelf::default() }], ..Page::default() }
    }

    #[test]
    fn picks_videos_only_once_and_capped() {
        let mut items = vec![item(ItemKind::Short, "short"), item(ItemKind::Upcoming, "later"), item(ItemKind::Channel, "chan")];
        items.extend((0..20).map(|i| item(ItemKind::Video, &format!("v{i}"))));
        items.push(item(ItemKind::Video, "v0"));
        let got = pick(&page(items), true);
        assert_eq!(got.len(), MAX_CARDS);
        assert!(got.iter().all(|i| i.kind == ItemKind::Video));
        let mut ids: Vec<&str> = got.iter().map(|i| i.id.as_str()).collect();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n);
    }

    #[test]
    fn signed_out_takes_the_first_row_only() {
        let p = Page {
            shelves: vec![
                Shelf { items: vec![item(ItemKind::Video, "first")], ..Shelf::default() },
                Shelf { items: vec![item(ItemKind::Video, "second")], ..Shelf::default() },
            ],
            ..Page::default()
        };
        assert_eq!(pick(&p, false).iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), vec!["first"]);
        assert_eq!(pick(&p, true).len(), 2);
    }

    #[test]
    fn payload_carries_a_deep_link_and_no_account_details() {
        let v: serde_json::Value = serde_json::from_str(&payload(&[item(ItemKind::Video, "abcdefghijk")])).unwrap();
        assert_eq!(v["launchPointId"], format!("{}_default", plx_base::paths::app_id()));
        let card = &v["contents"][1];
        assert_eq!(card["type"], "content");
        assert_eq!(card["onClick"]["contentId"], "https://www.youtube.com/tv?v=abcdefghijk");
        assert_eq!(card["onClick"]["targetAppId"], plx_base::paths::app_id());
        assert_eq!(card["image"]["width"], 480);
        assert!(card.to_string().to_lowercase().find("token").is_none());
    }

    #[test]
    fn replies() {
        assert!(reply_ok(r#"{"returnValue":true}"#).is_ok());
        assert_eq!(reply_ok(r#"{"returnValue":false,"errorText":"not permitted"}"#), Err("not permitted".into()));
        assert!(reply_ok("<LSCallOneReply failed: not permitted>").is_err());
    }
}

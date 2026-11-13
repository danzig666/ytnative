//! InnerTube response → `model`. Written against TVHTML5 responses (tileRenderer family) and the
//! newer view-model family (lockupViewModel) that search and some shelves use.
//!
//! The parser is deliberately structural rather than path-exact: YouTube moves wrappers around
//! often, so we look for the renderers we understand wherever they occur.

use crate::innertube::text;
use crate::model::{Endpoint, Item, ItemKind, Page, Shelf, Tab};
use serde_json::Value;

/// Parses a browse / search / continuation response into a page.
pub fn page(resp: &Value) -> Page {
    let mut page = Page::default();
    let mut loose = Shelf::default();
    collect(resp, &mut page, &mut loose, 0);
    if !loose.items.is_empty() {
        page.shelves.insert(0, loose);
    }
    if page.continuation.is_none() {
        page.continuation = page_continuation(resp);
    }
    page.shelves.retain(|s| !s.items.is_empty());
    page.shelves = merge_grid_rows(std::mem::take(&mut page.shelves));
    page
}

/// TV grids arrive as a run of untitled shelves, one per row. Join each run into one shelf.
fn merge_grid_rows(shelves: Vec<Shelf>) -> Vec<Shelf> {
    let mut out: Vec<Shelf> = Vec::new();
    for s in shelves {
        match out.last_mut() {
            Some(prev) if s.title.is_empty() && prev.title.is_empty() => {
                prev.items.extend(s.items);
                if s.continuation.is_some() {
                    prev.continuation = s.continuation;
                }
            }
            _ => out.push(s),
        }
    }
    out
}

fn collect(v: &Value, page: &mut Page, loose: &mut Shelf, depth: usize) {
    if depth > 64 {
        return;
    }
    match v {
        Value::Object(m) => {
            if let Some(s) = m.get("shelfRenderer").or_else(|| m.get("gridRenderer").filter(|_| false)) {
                page.shelves.push(shelf(s));
                return;
            }
            if let Some(t) = m.get("tabRenderer") {
                page.tabs.push(tab(t));
                // A selected tab carries the content being shown.
                if t["selected"].as_bool() == Some(true) {
                    if let Some(c) = t.get("content") {
                        collect(c, page, loose, depth + 1);
                    }
                }
                return;
            }
            if let Some(it) = item(v) {
                loose.items.push(it);
                return;
            }
            // Skip menus / overlays (their tiles are actions, not content) and every ad slot.
            for (k, x) in m {
                if matches!(
                    k.as_str(),
                    "menu" | "onLongPressCommand" | "overlay" | "frameworkUpdates" | "responseContext" | "adSlotRenderer" | "promotedSparklesWebRenderer" | "adSlotAndLayoutMetadata"
                ) {
                    continue;
                }
                collect(x, page, loose, depth + 1);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect(x, page, loose, depth + 1)),
        _ => {}
    }
}

fn shelf(s: &Value) -> Shelf {
    let mut out = Shelf {
        title: shelf_title(s).unwrap_or_default(),
        endpoint: s.get("endpoint").and_then(endpoint),
        ..Default::default()
    };
    let content = &s["content"];
    let mut sink_page = Page::default();
    let mut sink = Shelf::default();
    collect(content, &mut sink_page, &mut sink, 0);
    out.items = sink.items;
    // Nested shelves (rare) are flattened into this one.
    for nested in sink_page.shelves {
        out.items.extend(nested.items);
    }
    out.continuation = first_continuation(content);
    out
}

fn shelf_title(s: &Value) -> Option<String> {
    let h = &s["headerRenderer"]["shelfHeaderRenderer"];
    text(&h["avatarLockup"]["avatarLockupRenderer"]["title"])
        .or_else(|| text(&h["title"]))
        .or_else(|| text(&s["title"]))
        .or_else(|| text(&s["headerRenderer"]["shelfHeaderRenderer"]["avatarLockup"]["avatarLockupRenderer"]["title"]))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

fn tab(t: &Value) -> Tab {
    Tab {
        title: text(&t["title"]).unwrap_or_default(),
        thumbnail: best_thumb(&t["thumbnail"]["thumbnails"]),
        selected: t["selected"].as_bool().unwrap_or(false),
        endpoint: t.get("endpoint").and_then(endpoint),
    }
}

/// One content card, from either renderer family.
pub fn item(v: &Value) -> Option<Item> {
    if let Some(t) = v.get("tileRenderer") {
        return tile(t);
    }
    if let Some(l) = v.get("lockupViewModel") {
        return lockup(l);
    }
    None
}

fn tile(t: &Value) -> Option<Item> {
    let id = t["contentId"].as_str()?.to_string();
    let meta = &t["metadata"]["tileMetadataRenderer"];
    let lines: Vec<String> = meta["lines"]
        .as_array()
        .map(|ls| {
            ls.iter()
                .filter_map(|l| {
                    let parts: Vec<String> = l["lineRenderer"]["items"]
                        .as_array()?
                        .iter()
                        .filter_map(|i| text(&i["lineItemRenderer"]["text"]))
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty() && s != "•")
                        .collect();
                    (!parts.is_empty()).then(|| parts.join(" • "))
                })
                .collect()
        })
        .unwrap_or_default();
    let header = &t["header"]["tileHeaderRenderer"];
    let mut badge = None;
    let mut progress = None;
    let mut kind = match t["contentType"].as_str().unwrap_or("") {
        "TILE_CONTENT_TYPE_CHANNEL" => ItemKind::Channel,
        "TILE_CONTENT_TYPE_PLAYLIST" => ItemKind::Playlist,
        "TILE_CONTENT_TYPE_SHORTS" => ItemKind::Short,
        "TILE_CONTENT_TYPE_VIDEO" => ItemKind::Video,
        _ => ItemKind::Unknown,
    };
    for o in header["thumbnailOverlays"].as_array().into_iter().flatten() {
        if let Some(ts) = o.get("thumbnailOverlayTimeStatusRenderer") {
            badge = text(&ts["text"]);
            match ts["style"].as_str() {
                Some("LIVE") => kind = ItemKind::Live,
                Some("UPCOMING") => kind = ItemKind::Upcoming,
                Some("SHORTS") => kind = ItemKind::Short,
                _ => {}
            }
        }
        if let Some(rp) = o.get("thumbnailOverlayResumePlaybackRenderer") {
            progress = rp["percentDurationWatched"].as_u64().map(|p| p.min(100) as u8);
        }
    }
    let endpoint = t.get("onSelectCommand").and_then(endpoint);
    if matches!(endpoint, Some(Endpoint::Watch { .. })) && kind == ItemKind::Unknown {
        kind = ItemKind::Video;
    }
    if t["onSelectCommand"].get("reelWatchEndpoint").is_some() {
        kind = ItemKind::Short;
    }
    let channel = lines.first().filter(|_| matches!(kind, ItemKind::Video | ItemKind::Live | ItemKind::Upcoming)).cloned();
    Some(Item {
        kind,
        id,
        title: text(&meta["title"])
            .or_else(|| header["thumbnailOverlays"].as_array()?.iter().find_map(|o| text(&o["tileMetadataRenderer"]["title"])))
            .unwrap_or_default(),
        channel,
        lines,
        thumbnail: best_thumb(&header["thumbnail"]["thumbnails"]),
        badge,
        progress,
        endpoint,
        menu: crate::actions::menu_actions(t),
    })
}

fn lockup(l: &Value) -> Option<Item> {
    let id = l["contentId"].as_str()?.to_string();
    let meta = &l["metadata"]["lockupMetadataViewModel"];
    let rows = &meta["metadata"]["contentMetadataViewModel"]["metadataRows"];
    let lines: Vec<String> = rows
        .as_array()
        .map(|rs| {
            rs.iter()
                .filter_map(|r| {
                    let parts: Vec<String> = r["metadataParts"].as_array()?.iter().filter_map(|p| text(&p["text"])).collect();
                    (!parts.is_empty()).then(|| parts.join(" • "))
                })
                .collect()
        })
        .unwrap_or_default();
    let img = &l["contentImage"]["thumbnailViewModel"];
    let img = if img.is_null() { &l["contentImage"]["collectionThumbnailViewModel"]["primaryThumbnail"]["thumbnailViewModel"] } else { img };
    let mut badge = None;
    let mut progress = None;
    for o in img["overlays"].as_array().into_iter().flatten() {
        for b in o["thumbnailBottomOverlayViewModel"]["badges"].as_array().into_iter().flatten() {
            if let Some(t) = b["thumbnailBadgeViewModel"]["text"].as_str() {
                badge = Some(t.to_string());
            }
        }
        if let Some(t) = o["thumbnailOverlayBadgeViewModel"]["thumbnailBadges"][0]["thumbnailBadgeViewModel"]["text"].as_str() {
            badge = Some(t.to_string());
        }
        if let Some(p) = o["thumbnailBottomOverlayViewModel"]["progressBar"]["thumbnailOverlayProgressBarViewModel"]["startPercent"].as_u64() {
            progress = Some(p.min(100) as u8);
        }
    }
    let mut kind = match l["contentType"].as_str().unwrap_or("") {
        "LOCKUP_CONTENT_TYPE_VIDEO" => ItemKind::Video,
        "LOCKUP_CONTENT_TYPE_PLAYLIST" => ItemKind::Playlist,
        "LOCKUP_CONTENT_TYPE_ALBUM" => ItemKind::Playlist,
        "LOCKUP_CONTENT_TYPE_PODCAST" => ItemKind::Playlist,
        "LOCKUP_CONTENT_TYPE_CHANNEL" => ItemKind::Channel,
        _ => ItemKind::Unknown,
    };
    if badge.as_deref().is_some_and(|b| b.eq_ignore_ascii_case("live")) {
        kind = ItemKind::Live;
    }
    if badge.as_deref().is_some_and(|b| b.to_ascii_lowercase().contains("mix")) {
        kind = ItemKind::Mix;
    }
    let cmd = &l["rendererContext"]["commandContext"]["onTap"]["innertubeCommand"];
    let endpoint = endpoint(cmd).or_else(|| match kind {
        ItemKind::Video | ItemKind::Live => Some(Endpoint::Watch { video_id: id.clone(), playlist_id: None, params: None }),
        _ => None,
    });
    let channel = lines.first().filter(|_| matches!(kind, ItemKind::Video | ItemKind::Live)).cloned();
    Some(Item {
        kind,
        id,
        title: text(&meta["title"]).unwrap_or_default(),
        channel,
        lines,
        thumbnail: img["image"]["sources"].as_array().and_then(|s| pick_largest(s)),
        badge,
        progress,
        endpoint,
        menu: crate::actions::menu_actions(l),
    })
}

/// Endpoint from any command object (`onSelectCommand`, `navigationEndpoint`, `innertubeCommand`).
pub fn endpoint(c: &Value) -> Option<Endpoint> {
    if let Some(w) = c.get("watchEndpoint") {
        return Some(Endpoint::Watch {
            video_id: w["videoId"].as_str()?.to_string(),
            playlist_id: w["playlistId"].as_str().map(str::to_string),
            params: w["params"].as_str().map(str::to_string),
        });
    }
    if let Some(w) = c.get("reelWatchEndpoint") {
        return Some(Endpoint::Watch { video_id: w["videoId"].as_str()?.to_string(), playlist_id: None, params: None });
    }
    if let Some(w) = c.get("watchPlaylistEndpoint") {
        return Some(Endpoint::Watch {
            video_id: String::new(),
            playlist_id: w["playlistId"].as_str().map(str::to_string),
            params: w["params"].as_str().map(str::to_string),
        });
    }
    if let Some(b) = c.get("browseEndpoint") {
        return Some(Endpoint::Browse {
            browse_id: b["browseId"].as_str()?.to_string(),
            params: b["params"].as_str().map(str::to_string),
        });
    }
    if let Some(s) = c.get("searchEndpoint") {
        return Some(Endpoint::Search { query: s["query"].as_str()?.to_string(), params: s["params"].as_str().map(str::to_string) });
    }
    None
}

fn pick_largest(list: &[Value]) -> Option<String> {
    let best = list.iter().max_by_key(|t| t["width"].as_u64().unwrap_or(0))?;
    let url = best["url"].as_str()?;
    Some(if url.starts_with("//") { format!("https:{url}") } else { url.to_string() })
}

fn best_thumb(list: &Value) -> Option<String> {
    list.as_array().and_then(|l| pick_largest(l))
}

/// The next items of ONE shelf (a horizontal continuation answer) and its next token, in
/// MediaServiceCore's order: horizontalListContinuation, gridContinuation,
/// playlistVideoListContinuation, tvSurfaceContentContinuation's grid, else every shelf of a
/// sectionListContinuation flattened (token: the last shelf's).
pub fn shelf_continuation(resp: &Value) -> (Vec<Item>, Option<String>) {
    let cc = &resp["continuationContents"];
    let lists = [
        (&cc["horizontalListContinuation"]["items"], &cc["horizontalListContinuation"]["continuations"]),
        (&cc["gridContinuation"]["items"], &cc["gridContinuation"]["continuations"]),
        (&cc["playlistVideoListContinuation"]["contents"], &cc["playlistVideoListContinuation"]["continuations"]),
        (
            &cc["tvSurfaceContentContinuation"]["content"]["gridRenderer"]["items"],
            &cc["tvSurfaceContentContinuation"]["content"]["gridRenderer"]["continuations"],
        ),
    ];
    for (items, conts) in lists {
        if let Some(a) = items.as_array() {
            return (a.iter().filter_map(item).collect(), continuation_token(conts));
        }
    }
    // A section list: all its shelves' items in one row, the last shelf's token (else the list's).
    let p = page(resp);
    let token = p.shelves.last().and_then(|s| s.continuation.clone()).or(p.continuation);
    (p.shelves.into_iter().flat_map(|s| s.items).collect(), token)
}

/// The vertical continuation of a page: the first `nextContinuationData` / continuation command
/// that belongs to the section list rather than to a shelf.
fn page_continuation(resp: &Value) -> Option<String> {
    let candidates = [
        &resp["contents"]["tvBrowseRenderer"]["content"]["tvSurfaceContentRenderer"]["content"]["sectionListRenderer"]["continuations"],
        &resp["contents"]["sectionListRenderer"]["continuations"],
        &resp["continuationContents"]["sectionListContinuation"]["continuations"],
        &resp["continuationContents"]["gridContinuation"]["continuations"],
        &resp["continuationContents"]["horizontalListContinuation"]["continuations"],
    ];
    candidates.iter().find_map(|c| continuation_token(c))
}

fn first_continuation(v: &Value) -> Option<String> {
    match v {
        Value::Object(m) => {
            if let Some(c) = m.get("continuations") {
                if let Some(t) = continuation_token(c) {
                    return Some(t);
                }
            }
            m.values().find_map(first_continuation)
        }
        Value::Array(a) => a.iter().find_map(first_continuation),
        _ => None,
    }
}

fn continuation_token(c: &Value) -> Option<String> {
    c.as_array()?.iter().find_map(|x| {
        x["nextContinuationData"]["continuation"]
            .as_str()
            .or_else(|| x["reloadContinuationData"]["continuation"].as_str())
            .map(str::to_string)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture(name: &str) -> Option<Value> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/private").join(name);
        let raw = std::fs::read_to_string(p).ok()?;
        serde_json::from_str(&raw).ok()
    }

    #[test]
    fn private_fixtures_parse() {
        for name in ["home.json", "subs.json", "history.json", "library.json", "search.json", "next.json", "channel.json"] {
            let Some(v) = fixture(name) else {
                eprintln!("skip {name} (no private fixture)");
                continue;
            };
            let p = page(&v);
            eprintln!(
                "{name}: {} shelves, {} items, {} tabs, continuation={}",
                p.shelves.len(),
                p.item_count(),
                p.tabs.len(),
                p.continuation.is_some()
            );
            for s in p.shelves.iter().take(4) {
                eprintln!("   [{}] {} items; first: {:?} {:?} {:?}", s.title, s.items.len(), s.items[0].kind, s.items[0].title, s.items[0].badge);
            }
            assert!(p.item_count() > 0, "{name} produced no items");
            for s in &p.shelves {
                for it in &s.items {
                    assert!(!it.id.is_empty());
                    assert!(!it.title.is_empty() || it.kind == ItemKind::Unknown, "{name}: untitled {:?}", it.id);
                }
            }
        }
    }
}

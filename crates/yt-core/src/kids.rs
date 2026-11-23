//! YouTube Kids' home (SmartTube's "Kids" section): the youtubekids.com web client's
//! `FEkids_home`, one shelf per category (Shows, Music, Learning, Explore), asked anonymously in
//! the interface language and content country. The videos are ordinary YouTube videos.

use crate::innertube::text;
use crate::model::{Endpoint, Item, ItemKind, Page, Shelf};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};

const URL: &str = "https://www.youtubekids.com/youtubei/v1/browse?prettyPrint=false";
const VERSION: &str = "2.20251120.00.00";

fn context() -> Value {
    let hl = crate::innertube::content_language().unwrap_or_else(|| "en".into());
    let gl = crate::innertube::content_country().unwrap_or_else(|| "US".into());
    json!({ "client": { "clientName": "WEB_KIDS", "clientVersion": VERSION, "hl": hl, "gl": gl,
        "kidsAppInfo": { "contentSettings": { "corpusPreference": "KIDS_CORPUS_PREFERENCE_YOUNGER", "kidsNoSearchMode": "YT_KIDS_NO_SEARCH_MODE_OFF" } } } })
}

fn browse(http: &reqwest::blocking::Client, params: Option<&str>) -> Result<Value> {
    let mut body = json!({ "context": context(), "browseId": "FEkids_home" });
    if let Some(p) = params {
        body["params"] = json!(p);
    }
    let r = http
        .post(URL)
        .header("Content-Type", "application/json")
        .header("Origin", "https://www.youtubekids.com")
        .header("User-Agent", crate::comment_threads::UA)
        .body(body.to_string())
        .send()?;
    if !r.status().is_success() {
        return Err(anyhow!("kids: HTTP {}", r.status()));
    }
    Ok(r.json()?)
}

/// The home page: a shelf per category, the first from the home answer itself, the others asked
/// with their category's params.
pub fn home(http: &reqwest::blocking::Client) -> Result<Page> {
    let r = browse(http, None)?;
    let anchors = r["contents"]["kidsHomeScreenRenderer"]["anchors"].as_array().cloned().unwrap_or_default();
    if anchors.is_empty() {
        return Err(anyhow!("kids: no categories"));
    }
    let mut page = Page::default();
    for a in anchors.iter().map(|a| &a["anchoredSectionRenderer"]) {
        let title = text(&a["title"]).unwrap_or_default();
        let items = if a["content"].is_object() {
            items(&a["content"])
        } else {
            let params = a["navigationEndpoint"]["browseEndpoint"]["params"].as_str().map(|p| p.replace("%3D", "="));
            match browse(http, params.as_deref()) {
                // The answer to a category's params has that category's anchor filled in.
                Ok(c) => c["contents"]["kidsHomeScreenRenderer"]["anchors"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|x| &x["anchoredSectionRenderer"])
                    .find(|x| text(&x["title"]).as_deref() == Some(title.as_str()) && x["content"].is_object())
                    .map(|x| items(&x["content"]))
                    .unwrap_or_default(),
                Err(e) => {
                    eprintln!("[kids] {title}: {e:#}");
                    Vec::new()
                }
            }
        };
        if !items.is_empty() {
            page.shelves.push(Shelf { title, items, ..Default::default() });
        }
    }
    Ok(page)
}

/// The videos and playlists anywhere under `v`.
fn items(v: &Value) -> Vec<Item> {
    let mut out = Vec::new();
    collect(v, &mut out);
    out
}

fn collect(v: &Value, out: &mut Vec<Item>) {
    match v {
        Value::Object(o) => {
            if let Some(c) = o.get("compactVideoRenderer") {
                out.extend(video(c));
            } else if let Some(c) = o.get("compactPlaylistRenderer") {
                out.extend(playlist(c));
            } else {
                o.values().for_each(|x| collect(x, out));
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect(x, out)),
        _ => {}
    }
}

fn thumb(v: &Value) -> Option<String> {
    v["thumbnails"].as_array()?.last()?["url"].as_str().map(String::from)
}

fn video(c: &Value) -> Option<Item> {
    let id = c["videoId"].as_str()?.to_string();
    let channel = text(&c["longBylineText"]).or_else(|| text(&c["shortBylineText"]));
    let stats: Vec<String> = [text(&c["viewCountText"]), text(&c["publishedTimeText"])].into_iter().flatten().collect();
    let mut lines: Vec<String> = channel.iter().cloned().collect();
    if !stats.is_empty() {
        lines.push(stats.join(" \u{2022} "));
    }
    Some(Item {
        kind: ItemKind::Video,
        title: text(&c["title"]).unwrap_or_default(),
        lines,
        channel,
        thumbnail: thumb(&c["thumbnail"]).or_else(|| Some(format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"))),
        badge: text(&c["lengthText"]),
        endpoint: Some(Endpoint::Watch { video_id: id.clone(), playlist_id: None, params: None }),
        id,
        ..Default::default()
    })
}

fn playlist(c: &Value) -> Option<Item> {
    let id = c["playlistId"].as_str()?.to_string();
    let channel = text(&c["longBylineText"]).or_else(|| text(&c["shortBylineText"]));
    Some(Item {
        kind: ItemKind::Playlist,
        title: text(&c["title"]).unwrap_or_default(),
        lines: [channel.clone(), text(&c["videoCountText"])].into_iter().flatten().collect(),
        channel,
        thumbnail: thumb(&c["thumbnail"]),
        endpoint: Some(Endpoint::Browse { browse_id: format!("VL{id}"), params: None }),
        id,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kids_card() {
        let c = json!({ "videoId": "bo5YKMjG0Qw", "title": { "runs": [{ "text": "Hungry Plant" }] },
            "longBylineText": { "runs": [{ "text": "Hello Kitty and Friends" }] },
            "viewCountText": { "runs": [{ "text": "703,724 views" }] }, "publishedTimeText": { "runs": [{ "text": "1 month ago" }] },
            "lengthText": { "runs": [{ "text": "4:02" }] }, "thumbnail": { "thumbnails": [{ "url": "a" }, { "url": "b" }] } });
        let page = json!({ "x": [{ "compactVideoRenderer": c }] });
        let it = &items(&page)[0];
        assert_eq!((it.id.as_str(), it.title.as_str(), it.badge.as_deref(), it.thumbnail.as_deref()), ("bo5YKMjG0Qw", "Hungry Plant", Some("4:02"), Some("b")));
        assert_eq!(it.lines, vec!["Hello Kitty and Friends".to_string(), "703,724 views \u{2022} 1 month ago".into()]);
    }

    #[test]
    #[ignore]
    fn live_kids_home() {
        let http = crate::net::builder().build().unwrap();
        let p = home(&http).unwrap();
        eprintln!("{:?}", p.shelves.iter().map(|s| (&s.title, s.items.len())).collect::<Vec<_>>());
        assert!(p.shelves.len() >= 2 && p.shelves.iter().all(|s| !s.items.is_empty()));
    }
}

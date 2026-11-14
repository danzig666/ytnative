//! Watch page details from the signed-in TV client's `next` response: metadata, channel, like /
//! subscribe state and the tokens to change them, chapters, suggestions, playlist queue, and the
//! entry points for comments and live chat.
//!
//! Layout (TVHTML5, 2026-11): `contents.singleColumnWatchNextResults.{results, pivot, autoplay,
//! playlist, conversationBar}`, `transportControls` (like / subscribe buttons), `engagementPanels`
//! (comments, description, chapters) and `frameworkUpdates` entities (chapters, like and
//! subscription state). Chapter sources follow SmartTube's WatchNextHelper, with a description
//! timestamp fallback.

use crate::innertube::text;
use crate::model::{Endpoint, Item, ItemKind, Shelf};
use crate::parse;
use crate::session::Session;
use anyhow::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LikeStatus {
    Like,
    Dislike,
    #[default]
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chapter {
    pub title: String,
    pub start_ms: u64,
    pub thumbnail: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlaylistPanel {
    pub id: String,
    pub title: String,
    pub items: Vec<Item>,
    /// Index of the playing video inside `items`.
    pub current_index: Option<usize>,
    /// Mixes are endless: more items come from `continuation`.
    pub infinite: bool,
    pub continuation: Option<String>,
}

/// Tokens for `like/like`, `like/dislike`, `like/removelike` (body: `{"target":{"videoId"},"params"}`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LikeActions {
    pub like_params: Option<String>,
    pub dislike_params: Option<String>,
    pub remove_like_params: Option<String>,
}

/// Tokens for `subscription/subscribe` and `subscription/unsubscribe` (body: `{"channelIds","params"}`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubscribeActions {
    pub channel_ids: Vec<String>,
    pub subscribe_params: Option<String>,
    pub unsubscribe_params: Option<String>,
}

/// A comment sort option ("Top", "Newest") and the continuation that loads it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentSort {
    pub title: String,
    pub selected: bool,
    pub continuation: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WatchInfo {
    pub video_id: String,
    pub title: String,
    pub channel_id: String,
    pub channel_name: String,
    pub channel_avatar: Option<String>,
    pub subscribed: Option<bool>,
    pub subscriber_count_text: Option<String>,
    /// Full ("23,534,821 views").
    pub view_count_text: Option<String>,
    /// Short ("23M views").
    pub short_view_count_text: Option<String>,
    pub published_text: Option<String>,
    /// The upload (or premiere / stream) date, (year, month 1..=12, day), when the page gives
    /// one ("Published on Nov 10, 2014"); `published_text` is the short age ("12y ago").
    #[serde(default)]
    pub published_date: Option<(i32, u32, u32)>,
    pub description: String,
    pub like_count_text: Option<String>,
    pub like_status: LikeStatus,
    pub chapters: Vec<Chapter>,
    pub suggestions: Vec<Shelf>,
    /// More suggestion shelves: `Session::more("next", token)`.
    pub suggestions_continuation: Option<String>,
    pub playlist: Option<PlaylistPanel>,
    /// Top comments: `Session::comments(token)`.
    pub comments_continuation: Option<String>,
    pub comments_count_text: Option<String>,
    pub comment_sorts: Vec<CommentSort>,
    pub live_chat_continuation: Option<String>,
    pub autoplay_next: Option<Item>,
    pub like_actions: LikeActions,
    pub subscribe_actions: SubscribeActions,
}

impl Session {
    /// The watch page for a video (optionally inside a playlist / mix).
    pub fn watch(&mut self, video_id: &str, playlist_id: Option<&str>) -> Result<WatchInfo> {
        let mut body = json!({ "videoId": video_id });
        if let Some(p) = playlist_id {
            body["playlistId"] = json!(p);
        }
        Ok(parse_watch(&self.raw("next", body)?))
    }
}

/// Parses a TV `next` response.
pub fn parse_watch(resp: &Value) -> WatchInfo {
    let r = &resp["contents"]["singleColumnWatchNextResults"];
    let mut w = WatchInfo::default();

    // --- primary metadata ---
    let meta = find_renderer(&r["results"], "videoMetadataRenderer").unwrap_or(&Value::Null);
    w.video_id = meta["videoId"]
        .as_str()
        .or(resp["currentVideoEndpoint"]["watchEndpoint"]["videoId"].as_str())
        .unwrap_or("")
        .to_string();
    w.title = text(&meta["title"]).unwrap_or_default();
    w.published_text = text(&meta["dateText"]).or_else(|| text(&meta["publishedTimeText"]));
    w.published_date = [&meta["publishedTimeText"], &meta["dateText"]].into_iter().filter_map(text).find_map(|t| parse_date(&t));
    let vc = &meta["viewCount"]["videoViewCountRenderer"];
    w.view_count_text = text(&vc["viewCount"]);
    w.short_view_count_text = text(&vc["shortViewCount"]);
    let owner = &meta["owner"]["videoOwnerRenderer"];
    w.channel_name = text(&owner["title"]).unwrap_or_default();
    w.channel_id = owner["navigationEndpoint"]["browseEndpoint"]["browseId"].as_str().unwrap_or("").to_string();
    w.channel_avatar = largest(&owner["thumbnail"]["thumbnails"]);
    w.subscriber_count_text = text(&owner["subscriberCountText"]);
    if let Some(d) = text(&meta["description"]) {
        w.description = d;
    }

    let panels: Vec<&Value> = resp["engagementPanels"]
        .as_array()
        .map(|a| a.iter().map(|p| &p["engagementPanelSectionListRenderer"]).collect())
        .unwrap_or_default();

    // --- description (structured description panel, or the expanded-description panel) ---
    for p in &panels {
        for it in p["content"]["structuredDescriptionContentRenderer"]["items"].as_array().into_iter().flatten() {
            let body = &it["expandableVideoDescriptionBodyRenderer"];
            if w.description.is_empty() {
                if let Some(d) = text(&body["descriptionBodyText"]).or_else(|| text(&body["attributedDescriptionBodyText"])) {
                    w.description = d;
                }
            }
            let h = &it["videoDescriptionHeaderRenderer"];
            if w.title.is_empty() {
                w.title = text(&h["title"]).unwrap_or_default();
            }
            if w.view_count_text.is_none() {
                w.view_count_text = text(&h["views"]);
            }
            if w.published_text.is_none() {
                w.published_text = text(&h["publishDate"]);
            }
            if w.published_date.is_none() {
                w.published_date = text(&h["publishDate"]).and_then(|t| parse_date(&t));
            }
        }
        if w.description.is_empty() {
            if let Some(segs) = p["content"]["richTextListViewModel"]["segments"].as_array() {
                let d: Vec<String> = segs.iter().filter_map(|s| text(&s["sectionString"])).collect();
                w.description = d.join("\n");
            }
        }
    }

    // --- like / subscribe (transport controls; framework entities are authoritative) ---
    let buttons = resp["transportControls"]["transportControlsRenderer"]["buttons"].as_array();
    let like = buttons
        .and_then(|bs| bs.iter().find_map(|b| b["button"].get("likeButtonRenderer")))
        .or_else(|| find_renderer(resp, "likeButtonRenderer"));
    if let Some(l) = like {
        w.like_count_text = text(&l["likeCountWithLikeText"]).or_else(|| text(&l["likeCountText"]));
        w.like_status = like_status(l["likeStatus"].as_str());
        for ep in l["serviceEndpoints"].as_array().into_iter().flatten() {
            let le = &ep["likeEndpoint"];
            if let Some(p) = le["likeParams"].as_str() {
                w.like_actions.like_params = Some(p.into());
            }
            if let Some(p) = le["dislikeParams"].as_str() {
                w.like_actions.dislike_params = Some(p.into());
            }
            if let Some(p) = le["removeLikeParams"].as_str() {
                w.like_actions.remove_like_params = Some(p.into());
            }
        }
    }
    let sub = buttons
        .and_then(|bs| bs.iter().find_map(|b| b["button"].get("subscribeButtonRenderer")))
        .or_else(|| owner.get("subscribeButton").and_then(|s| s.get("subscribeButtonRenderer")));
    if let Some(s) = sub {
        w.subscribed = s["subscribed"].as_bool();
        if let Some(id) = s["channelId"].as_str() {
            w.subscribe_actions.channel_ids = vec![id.to_string()];
            if w.channel_id.is_empty() {
                w.channel_id = id.to_string();
            }
        }
        let mut eps = Vec::new();
        collect_key(s, "subscribeEndpoint", &mut eps);
        if let Some(e) = eps.first() {
            w.subscribe_actions.subscribe_params = e["params"].as_str().map(str::to_string);
            if let Some(ids) = e["channelIds"].as_array() {
                w.subscribe_actions.channel_ids = ids.iter().filter_map(|i| i.as_str().map(str::to_string)).collect();
            }
        }
        let mut eps = Vec::new();
        collect_key(s, "unsubscribeEndpoint", &mut eps);
        if let Some(e) = eps.first() {
            w.subscribe_actions.unsubscribe_params = e["params"].as_str().map(str::to_string);
        }
    }
    if w.channel_name.is_empty() {
        if let Some(o) = buttons.and_then(|bs| bs.iter().find_map(|b| b["button"].get("videoOwnerRenderer"))) {
            w.channel_name = text(&o["title"]).unwrap_or_default();
            w.channel_avatar = w.channel_avatar.take().or_else(|| largest(&o["thumbnail"]["thumbnails"]));
        }
    }
    let mutations = resp["frameworkUpdates"]["entityBatchUpdate"]["mutations"].as_array();
    for m in mutations.into_iter().flatten() {
        let p = &m["payload"];
        if let Some(s) = p["subscriptionStateEntity"]["subscribed"].as_bool() {
            w.subscribed = Some(s);
        }
        if let Some(s) = p["likeStatusEntity"]["likeStatus"].as_str() {
            w.like_status = like_status(Some(s));
        }
    }

    // --- chapters ---
    w.chapters = chapters(resp, &panels);
    if w.chapters.is_empty() {
        w.chapters = chapters_from_description(&w.description);
    }

    // --- comments / live chat ---
    for p in &panels {
        if p["panelIdentifier"].as_str() != Some("comment-item-section") {
            continue;
        }
        w.comments_continuation = first_token(&p["content"]);
        let hdr = &p["header"]["engagementPanelTitleHeaderRenderer"];
        w.comments_count_text = text(&hdr["contextualInfo"]);
        for s in hdr["menu"]["sortFilterSubMenuRenderer"]["subMenuItems"].as_array().into_iter().flatten() {
            if let Some(c) = token_of(&s["continuation"]) {
                w.comment_sorts.push(CommentSort {
                    title: s["title"].as_str().unwrap_or("").to_string(),
                    selected: s["selected"].as_bool().unwrap_or(false),
                    continuation: c,
                });
            }
        }
    }
    w.live_chat_continuation = first_token(&r["conversationBar"]["liveChatRenderer"]);

    // --- playlist panel + suggestions ---
    let pl = &r["playlist"]["playlist"];
    let pivot = &r["pivot"]["sectionListRenderer"];
    let shelves: Vec<&Value> = pivot["contents"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    let mut queue_idx = None;
    if let Some(pid) = pl["playlistId"].as_str() {
        queue_idx = shelves.iter().position(|s| is_queue_shelf(&s["shelfRenderer"], pid));
        let mut panel = PlaylistPanel {
            id: pid.to_string(),
            title: pl["title"].as_str().map(str::to_string).or_else(|| text(&pl["titleText"])).unwrap_or_default(),
            current_index: pl["currentIndex"].as_u64().map(|i| i as usize),
            infinite: pl["isInfinite"].as_bool().unwrap_or(false),
            ..Default::default()
        };
        if let Some(i) = queue_idx {
            let content = &shelves[i]["shelfRenderer"]["content"];
            let mut tiles = Vec::new();
            collect_items(content, &mut tiles);
            panel.items = tiles;
            panel.continuation = first_token(&content["horizontalListRenderer"]);
            // The TV's queue shelf marks the playing tile.
            if let Some(sel) = content["horizontalListRenderer"]["selectedIndex"].as_u64() {
                panel.current_index.get_or_insert(sel as usize);
            }
            // Prefer the position of the playing video when it is in the window we got.
            if let Some(pos) = panel.items.iter().position(|it| it.id == w.video_id) {
                panel.current_index = Some(pos);
            }
        }
        w.playlist = Some(panel);
    }
    let rest: Vec<Value> = shelves.iter().enumerate().filter(|(i, _)| Some(*i) != queue_idx).map(|(_, s)| (*s).clone()).collect();
    let mut wrapped = json!({ "contents": { "sectionListRenderer": { "contents": rest } } });
    wrapped["contents"]["sectionListRenderer"]["continuations"] = pivot["continuations"].clone();
    let page = parse::page(&wrapped);
    w.suggestions = page.shelves;
    w.suggestions_continuation = page.continuation.or_else(|| first_token(pivot));

    // --- autoplay ---
    // The playing video (YouTube's "watch again" candidate is it): from the metadata, else the
    // response's top-level currentVideoEndpoint (not under singleColumnWatchNextResults).
    w.autoplay_next = autoplay_next(&r["autoplay"]["autoplay"], &w.video_id);
    w
}

/// A date in YouTube's English wording ("Published on Nov 10, 2014", "Premiered Jul 1, 2020",
/// "Streamed live on Mar 3, 2021"): (year, month, day).
pub fn parse_date(s: &str) -> Option<(i32, u32, u32)> {
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let words: Vec<&str> = s.split(|c: char| c.is_whitespace() || c == ',').filter(|w| !w.is_empty()).collect();
    words.windows(3).find_map(|w| {
        let m = MONTHS.iter().position(|m| w[0].len() >= 3 && w[0][..3].eq_ignore_ascii_case(m))? as u32 + 1;
        let d: u32 = w[1].parse().ok().filter(|d| (1..=31).contains(d))?;
        let y: i32 = w[2].parse().ok().filter(|y| (1990..=2200).contains(y))?;
        Some((y, m, d))
    })
}

fn like_status(s: Option<&str>) -> LikeStatus {
    match s {
        Some("LIKE") => LikeStatus::Like,
        Some("DISLIKE") => LikeStatus::Dislike,
        _ => LikeStatus::None,
    }
}

fn is_queue_shelf(s: &Value, playlist_id: &str) -> bool {
    if s["headerRenderer"]["shelfHeaderRenderer"]["icon"]["iconType"].as_str() == Some("PLAYLISTS") {
        return true;
    }
    let items = s["content"]["horizontalListRenderer"]["items"].as_array();
    items.is_some_and(|its| {
        !its.is_empty()
            && its.iter().all(|i| i["tileRenderer"]["onSelectCommand"]["watchEndpoint"]["playlistId"].as_str() == Some(playlist_id))
    })
}

/// YouTube's up-next video. `current` is the video playing: for a video in the watch history the
/// first candidate can be that same video ("watch again"), which is skipped.
fn autoplay_next(a: &Value, current: &str) -> Option<Item> {
    let set = a["sets"].as_array()?.first()?;
    let nv = &set["nextVideoRenderer"];
    let candidates = [
        nv.get("maybeHistoryEndpointRenderer"),
        nv.get("autoplayEndpointRenderer"),
        set["autoplayVideoRenderer"].get("autoplayEndpointRenderer"),
        set["autoplayVideoRenderer"].get("autonavEndpointRenderer"),
    ];
    let (src, ep) = candidates.into_iter().flatten().find_map(|src| {
        let ep = parse::endpoint(&src["endpoint"])?;
        match &ep {
            Endpoint::Watch { video_id, .. } if video_id != current => Some((src, ep)),
            _ => None,
        }
    })?;
    let Endpoint::Watch { video_id, .. } = &ep else { return None };
    let preview = &src["item"]["previewButtonRenderer"];
    let byline = text(&preview["byline"]);
    Some(Item {
        kind: ItemKind::Video,
        id: video_id.clone(),
        title: text(&preview["title"]).unwrap_or_default(),
        lines: byline.clone().into_iter().collect(),
        channel: byline,
        thumbnail: largest(&preview["thumbnail"]["thumbnails"]),
        endpoint: Some(ep),
        ..Default::default()
    })
}

// ---------- chapters ----------

fn chapters(resp: &Value, panels: &[&Value]) -> Vec<Chapter> {
    // V3: frameworkUpdates macroMarkersListEntity with MARKER_TYPE_CHAPTERS.
    let mut ents = Vec::new();
    collect_key(&resp["frameworkUpdates"], "macroMarkersListEntity", &mut ents);
    for e in ents {
        if e["markersList"]["markerType"].as_str() == Some("MARKER_TYPE_CHAPTERS") {
            let out = marker_list(&e["markersList"]["markers"]);
            if !out.is_empty() {
                return out;
            }
        }
    }
    // V1: player bar markers map.
    let mm = &resp["playerOverlays"]["playerOverlayRenderer"]["decoratedPlayerBarRenderer"]["decoratedPlayerBarRenderer"]["playerBar"]
        ["multiMarkersPlayerBarRenderer"]["markersMap"];
    for m in mm.as_array().into_iter().flatten() {
        let out = marker_list(&m["value"]["chapters"]);
        if !out.is_empty() {
            return out;
        }
    }
    // V2: macroMarkersListRenderer engagement panel.
    for p in panels {
        let out = marker_list(&p["content"]["macroMarkersListRenderer"]["contents"]);
        if !out.is_empty() {
            return out;
        }
    }
    Vec::new()
}

fn marker_list(list: &Value) -> Vec<Chapter> {
    let mut out: Vec<Chapter> = list.as_array().into_iter().flatten().filter_map(chapter).collect();
    out.sort_by_key(|c| c.start_ms);
    out
}

fn chapter(c: &Value) -> Option<Chapter> {
    if let Some(r) = c.get("chapterRenderer") {
        return Some(Chapter {
            title: text(&r["title"]).unwrap_or_default(),
            start_ms: r["timeRangeStartMillis"].as_u64()?,
            thumbnail: largest(&r["thumbnail"]["thumbnails"]),
        });
    }
    if let Some(r) = c.get("macroMarkersListItemRenderer") {
        let secs = r["onTap"]["watchEndpoint"]["startTimeSeconds"].as_u64()?;
        return Some(Chapter {
            title: text(&r["title"]).unwrap_or_default(),
            start_ms: secs * 1000,
            thumbnail: largest(&r["thumbnail"]["thumbnails"]),
        });
    }
    let start = c["startMillis"].as_str().and_then(|s| s.parse().ok()).or(c["startMillis"].as_u64())?;
    Some(Chapter { title: text(&c["title"]).unwrap_or_default(), start_ms: start, thumbnail: largest(&c["thumbnailDetails"]["thumbnails"]) })
}

/// YouTube's own rule: a description lists chapters when it has at least 3 timestamped lines,
/// the first at 0:00, in increasing order.
pub fn chapters_from_description(desc: &str) -> Vec<Chapter> {
    let re = Regex::new(r"^\s*(?:[-•*▶►]\s*)?\(?((?:\d{1,2}:)?\d{1,2}:\d{2})\)?\s*(?:[-–—:|]\s*)?(.*)$").unwrap();
    let mut out: Vec<Chapter> = Vec::new();
    for line in desc.lines() {
        let Some(c) = re.captures(line) else { continue };
        let Some(ms) = parse_clock(&c[1]) else { continue };
        if out.last().is_some_and(|l| l.start_ms >= ms) {
            continue;
        }
        let title = c[2].trim().trim_start_matches(['-', '–', '—', ':', '|']).trim().to_string();
        out.push(Chapter { title, start_ms: ms, thumbnail: None });
    }
    if out.len() < 3 || out[0].start_ms != 0 {
        return Vec::new();
    }
    out
}

fn parse_clock(s: &str) -> Option<u64> {
    let mut total = 0u64;
    for p in s.split(':') {
        total = total * 60 + p.parse::<u64>().ok()?;
    }
    Some(total * 1000)
}

/// Index of the chapter playing at `pos_ms`.
pub fn chapter_at(chapters: &[Chapter], pos_ms: u64) -> Option<usize> {
    let n = chapters.partition_point(|c| c.start_ms <= pos_ms);
    n.checked_sub(1)
}

// ---------- helpers ----------

fn find_renderer<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    crate::innertube::find(v, key)
}

fn collect_key<'a>(v: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if k == key {
                    out.push(x);
                } else {
                    collect_key(x, key, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_key(x, key, out)),
        _ => {}
    }
}

fn collect_items(v: &Value, out: &mut Vec<Item>) {
    if let Some(it) = parse::item(v) {
        out.push(it);
        return;
    }
    match v {
        Value::Object(m) => m.values().for_each(|x| collect_items(x, out)),
        Value::Array(a) => a.iter().for_each(|x| collect_items(x, out)),
        _ => {}
    }
}

/// Token of one continuation entry (`{nextContinuationData|reloadContinuationData|...: {continuation}}`).
pub(crate) fn token_of(c: &Value) -> Option<String> {
    let m = c.as_object()?;
    m.values().find_map(|d| d["continuation"].as_str().map(str::to_string)).or_else(|| {
        c["continuationCommand"]["token"].as_str().map(str::to_string)
    })
}

/// First continuation token under `v` (looks in `continuations` arrays).
pub(crate) fn first_token(v: &Value) -> Option<String> {
    match v {
        Value::Object(m) => {
            if let Some(cs) = m.get("continuations").and_then(Value::as_array) {
                if let Some(t) = cs.iter().find_map(token_of) {
                    return Some(t);
                }
            }
            m.values().find_map(first_token)
        }
        Value::Array(a) => a.iter().find_map(first_token),
        _ => None,
    }
}

pub(crate) fn largest(list: &Value) -> Option<String> {
    let best = list.as_array()?.iter().max_by_key(|t| t["width"].as_u64().unwrap_or(0))?;
    let url = best["url"].as_str()?;
    Some(if url.starts_with("//") { format!("https:{url}") } else { url.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture(name: &str) -> Option<Value> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/private").join(name);
        serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
    }

    #[test]
    fn description_chapters() {
        let d = "Intro text\n0:00 Intro\n1:05 - Setup\n12:30 Deep dive\n1:02:03 Outro\nnot 5:00 a chapter";
        let c = chapters_from_description(d);
        assert_eq!(c.len(), 4);
        assert_eq!(c[1], Chapter { title: "Setup".into(), start_ms: 65_000, thumbnail: None });
        assert_eq!(c[3].start_ms, 3_723_000);
        assert_eq!(chapter_at(&c, 70_000), Some(1));
        assert_eq!(chapter_at(&c, 0), Some(0));
        // Not starting at 0:00 -> no chapters.
        assert!(chapters_from_description("1:00 a\n2:00 b\n3:00 c").is_empty());
    }

    #[test]
    fn synthetic_next() {
        let v = json!({
            "contents": {"singleColumnWatchNextResults": {
                "results": {"results": {"contents": [{"itemSectionRenderer": {"contents": [{"videoMetadataRenderer": {
                    "videoId": "abc", "title": {"runs": [{"text": "T"}]},
                    "dateText": {"simpleText": "1y ago"},
                    "viewCount": {"videoViewCountRenderer": {"viewCount": {"simpleText": "10 views"}}},
                    "owner": {"videoOwnerRenderer": {"title": {"simpleText": "Ch"},
                        "navigationEndpoint": {"browseEndpoint": {"browseId": "UC1"}},
                        "thumbnail": {"thumbnails": [{"url": "//a/s48", "width": 48}, {"url": "//a/s88", "width": 88}]}}}
                }}]}}]}},
                "conversationBar": {"liveChatRenderer": {"continuations": [{"reloadContinuationData": {"continuation": "LIVE"}}]}}
            }},
            "frameworkUpdates": {"entityBatchUpdate": {"mutations": [
                {"payload": {"likeStatusEntity": {"likeStatus": "LIKE"}}},
                {"payload": {"subscriptionStateEntity": {"subscribed": true}}},
                {"payload": {"macroMarkersListEntity": {"markersList": {"markerType": "MARKER_TYPE_CHAPTERS", "markers": [
                    {"startMillis": "60000", "title": {"simpleText": "B"}},
                    {"startMillis": "0", "title": {"simpleText": "A"}}
                ]}}}}
            ]}},
            "engagementPanels": [{"engagementPanelSectionListRenderer": {"panelIdentifier": "comment-item-section",
                "content": {"sectionListRenderer": {"contents": [{"itemSectionRenderer": {"continuations": [{"reloadContinuationData": {"continuation": "COMM"}}]}}]}}}}]
        });
        let w = parse_watch(&v);
        assert_eq!(w.video_id, "abc");
        assert_eq!(w.channel_id, "UC1");
        assert_eq!(w.channel_avatar.as_deref(), Some("https://a/s88"));
        assert_eq!(w.like_status, LikeStatus::Like);
        assert_eq!(w.subscribed, Some(true));
        assert_eq!(w.chapters.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(), ["A", "B"]);
        assert_eq!(w.comments_continuation.as_deref(), Some("COMM"));
        assert_eq!(w.live_chat_continuation.as_deref(), Some("LIVE"));
    }

    #[test]
    fn private_next_fixtures() {
        for name in ["next.json", "next_chapters.json", "next_mix.json"] {
            let Some(v) = fixture(name) else {
                eprintln!("skip {name} (no private fixture)");
                continue;
            };
            let w = parse_watch(&v);
            eprintln!(
                "{name}: {:?} by {:?} ({}) sub={:?} views={:?} likes={:?} {:?} chapters={} shelves={} items={} pl={:?} comments={} sorts={} next={:?}",
                w.title,
                w.channel_name,
                w.channel_id,
                w.subscribed,
                w.short_view_count_text,
                w.like_count_text,
                w.like_status,
                w.chapters.len(),
                w.suggestions.len(),
                w.suggestions.iter().map(|s| s.items.len()).sum::<usize>(),
                w.playlist.as_ref().map(|p| (p.title.clone(), p.items.len(), p.current_index)),
                w.comments_continuation.is_some(),
                w.comment_sorts.len(),
                w.autoplay_next.as_ref().map(|i| (i.id.clone(), i.title.clone())),
            );
            assert!(!w.video_id.is_empty() && !w.title.is_empty() && !w.channel_id.is_empty() && !w.channel_name.is_empty());
            assert!(!w.description.is_empty(), "{name}: no description");
            assert!(w.like_actions.like_params.is_some() && w.like_actions.remove_like_params.is_some());
            assert!(w.subscribe_actions.subscribe_params.is_some() && w.subscribe_actions.unsubscribe_params.is_some());
            assert!(w.comments_continuation.is_some());
            assert!(!w.suggestions.is_empty());
            assert!(w.autoplay_next.is_some());
            if name == "next_chapters.json" {
                assert!(w.chapters.len() > 3 && w.chapters[0].start_ms == 0 && w.chapters[0].thumbnail.is_some());
            }
            if name == "next_mix.json" {
                let p = w.playlist.as_ref().expect("playlist");
                assert!(!p.items.is_empty() && p.continuation.is_some());
                assert_eq!(p.current_index, Some(0));
            }
        }
    }

    #[test]
    #[ignore]
    fn live_watch() {
        crate::paths::set_data_dir("C:/Download/prog/ytnative".into());
        let mut s = Session::new().unwrap();
        let w = s.watch("rfscVS0vtbw", None).unwrap();
        eprintln!("{} | {} | chapters {} | sugg {} | comments {}", w.title, w.channel_name, w.chapters.len(), w.suggestions.len(), w.comments_continuation.is_some());
        assert!(w.chapters.len() > 3);
        assert!(w.comments_continuation.is_some());
        let w = s.watch("dQw4w9WgXcQ", Some("RDdQw4w9WgXcQ")).unwrap();
        let p = w.playlist.unwrap();
        eprintln!("mix {} items, current {:?}", p.items.len(), p.current_index);
        assert!(!p.items.is_empty());
    }
}

#[cfg(test)]
mod date_tests {
    use super::parse_date;

    #[test]
    fn youtube_dates() {
        assert_eq!(parse_date("Published on Nov 10, 2014"), Some((2014, 11, 10)));
        assert_eq!(parse_date("Premiered Jul 1, 2020"), Some((2020, 7, 1)));
        assert_eq!(parse_date("Streamed live on Mar 3, 2021"), Some((2021, 3, 3)));
        assert_eq!(parse_date("12y ago"), None);
        assert_eq!(parse_date("3 days ago"), None);
    }
}

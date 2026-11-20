//! The account's notifications inbox (new uploads from channels with the bell on, replies...),
//! fetched the way MediaServiceCore's `NotificationsServiceInt` does: `notification/
//! get_notification_menu` with the TVHTML5 client and the TV sign-in token, asking for the
//! `NOTIFICATIONS_MENU_REQUEST_TYPE_INBOX` menu. YouTube answers with a popup menu of
//! `notificationRenderer`s; each one that points at a video becomes a card.
//!
//! YouTube now answers that request with HTTP 400 "Precondition check failed" for every client
//! (TV or web, any version or context extra; see the `live` diagnostics below), as it does for
//! SmartTube. Like MediaServiceCore's `NotificationsServiceIntWrapper`, the section then falls
//! back to [`rss_page`]: the newest videos of the channels in the local bell list
//! ([`crate::bell_channels`]), from their public RSS feeds.

use crate::channel_groups::{self, ChannelFeed, FeedCache};
use crate::innertube::text;
use crate::model::{Endpoint, Item, ItemKind, Page, Shelf};
use anyhow::Result;
use serde_json::{json, Value};

/// The endpoint (under `youtubei/v1/`) that serves the inbox.
pub const ENDPOINT: &str = "notification/get_notification_menu";

/// At most this many videos in the page built from the RSS feeds.
pub const MAX_RSS_ITEMS: usize = 60;

/// Request body for the first page of the inbox, or for the page after `ctoken`.
pub fn request(ctoken: Option<&str>) -> Value {
    match ctoken {
        Some(t) => json!({ "notificationsMenuRequestType": "NOTIFICATIONS_MENU_REQUEST_TYPE_INBOX", "ctoken": t }),
        None => json!({ "notificationsMenuRequestType": "NOTIFICATIONS_MENU_REQUEST_TYPE_INBOX" }),
    }
}

/// The inbox answer as one untitled shelf (a plain grid). The first page holds the items in
/// `openPopupAction.popup.multiPageMenuRenderer.sections[].multiPageMenuNotificationSectionRenderer`;
/// later pages come as `appendContinuationItemsAction.continuationItems`. A trailing
/// `continuationItemRenderer` carries the token of the next page.
pub fn page(resp: &Value) -> Page {
    let mut entries: Vec<&Value> = Vec::new();
    for a in resp["actions"].as_array().into_iter().flatten() {
        let sections = a["openPopupAction"]["popup"]["multiPageMenuRenderer"]["sections"].as_array();
        for s in sections.into_iter().flatten() {
            entries.extend(s["multiPageMenuNotificationSectionRenderer"]["items"].as_array().into_iter().flatten());
        }
        entries.extend(a["appendContinuationItemsAction"]["continuationItems"].as_array().into_iter().flatten());
    }
    let mut items = Vec::new();
    let mut continuation = None;
    for e in entries {
        if let Some(n) = e.get("notificationRenderer") {
            items.extend(item(n));
        } else if let Some(c) = e.get("continuationItemRenderer") {
            continuation = next_token(c);
        }
    }
    if items.is_empty() && continuation.is_none() {
        return Page::default();
    }
    Page { shelves: vec![Shelf { items, ..Default::default() }], continuation, ..Default::default() }
}

/// The token of the next page from a `continuationItemRenderer`.
fn next_token(c: &Value) -> Option<String> {
    let ep = &c["continuationEndpoint"];
    ep["getNotificationMenuEndpoint"]["ctoken"]
        .as_str()
        .or(ep["continuationCommand"]["token"].as_str())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// One notification as a video card: the video's title as the card title (the part of the
/// message after its colon, as SmartTube shows it) and the rest of the message ("Channel
/// uploaded") with the time it was sent as the detail line. Notifications that open no video
/// (a channel, a community post) are left out: OK could not play them.
///
/// The card's id is the notification's own, not the video's: lists drop cards whose id they
/// already hold, and one video can have several notifications (an upload, then a reply to a
/// comment on it, or a premiere and then its going live). The video id is in the endpoint.
fn item(n: &Value) -> Option<Item> {
    let ep = &n["navigationEndpoint"]["watchEndpoint"];
    let video_id = ep["videoId"].as_str().filter(|v| !v.is_empty())?.to_string();
    let message = text(&n["shortMessage"]).unwrap_or_default();
    let (head, title) = split_message(&message);
    let mut lines = Vec::new();
    if let Some(h) = head {
        lines.push(h);
    }
    if let Some(t) = text(&n["sentTimeText"]).filter(|t| !t.trim().is_empty()) {
        lines.push(t.trim().to_string());
    }
    let thumbnail = n["videoThumbnail"]["thumbnails"].as_array().and_then(|l| largest(l));
    let id = match n["notificationId"].as_str().filter(|v| !v.is_empty()) {
        Some(nid) => format!("notification:{nid}"),
        // Without YouTube's id, what tells two notifications of one video apart: their message
        // and the time they were sent.
        None => format!("notification:{video_id}:{message}:{}", lines.last().map(String::as_str).unwrap_or_default()),
    };
    Some(Item {
        kind: ItemKind::Video,
        id,
        title,
        lines,
        thumbnail,
        endpoint: Some(Endpoint::Watch {
            video_id,
            playlist_id: ep["playlistId"].as_str().map(str::to_string),
            params: ep["params"].as_str().map(str::to_string),
        }),
        ..Default::default()
    })
}

/// The fallback page: the newest videos of `channel_ids` from their RSS feeds (fetched a few at
/// a time, cached for a while: [`channel_groups::fetch_feeds`]), newest first, at most
/// [`MAX_RSS_ITEMS`], as one untitled shelf without a continuation. No channels: an empty page.
/// Fails only when not one feed could be loaded.
pub fn rss_page(http: &reqwest::blocking::Client, channel_ids: &[String]) -> Result<Page> {
    if channel_ids.is_empty() {
        return Ok(Page::default());
    }
    let mut feeds = Vec::new();
    let mut last_err = None;
    for (_, r) in channel_groups::fetch_feeds(http, channel_ids, FeedCache::global()) {
        match r {
            Ok(f) => feeds.push(f),
            Err(e) => last_err = Some(e),
        }
    }
    if let (true, Some(e)) = (feeds.is_empty(), last_err) {
        return Err(e.context("no channel's feed could be loaded"));
    }
    Ok(feeds_page(feeds.iter().map(|f| f.as_ref()), crate::fsutil::now_secs()))
}

/// Feeds merged into the notifications page (see [`rss_page`]); `now` in unix seconds, for the
/// cards' ages. Each card: the video's title, the channel's name, then its views and age
/// ("3 days ago"), as the channel groups' feeds show them.
pub fn feeds_page<'a>(feeds: impl IntoIterator<Item = &'a ChannelFeed>, now: u64) -> Page {
    let mut p = channel_groups::merge_feeds("", feeds, now);
    p.title = None;
    for s in &mut p.shelves {
        s.items.truncate(MAX_RSS_ITEMS);
    }
    p.shelves.retain(|s| !s.items.is_empty());
    p
}

/// "Channel uploaded: Title" -> (Some("Channel uploaded"), "Title"); a message without a colon
/// is the title on its own.
fn split_message(m: &str) -> (Option<String>, String) {
    match m.split_once(':') {
        Some((head, rest)) if !head.trim().is_empty() && !rest.trim().is_empty() => {
            (Some(head.trim().to_string()), rest.trim().to_string())
        }
        _ => (None, m.trim().to_string()),
    }
}

fn largest(list: &[Value]) -> Option<String> {
    let best = list.iter().max_by_key(|t| t["width"].as_u64().unwrap_or(0))?;
    let url = best["url"].as_str()?;
    Some(if url.starts_with("//") { format!("https:{url}") } else { url.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand-made inbox answer shaped like YouTube's: an upload, a reply, a notification
    /// without a video, and the token of the next page.
    fn fixture() -> Value {
        json!({
            "actions": [{
                "openPopupAction": { "popup": { "multiPageMenuRenderer": { "sections": [{
                    "multiPageMenuNotificationSectionRenderer": { "items": [
                        { "notificationRenderer": {
                            "shortMessage": { "simpleText": "Some Channel uploaded: A video about cats" },
                            "sentTimeText": { "simpleText": "2 hours ago" },
                            "videoThumbnail": { "thumbnails": [
                                { "url": "//i.ytimg.com/vi/vid00000001/small.jpg", "width": 120 },
                                { "url": "https://i.ytimg.com/vi/vid00000001/big.jpg", "width": 640 }
                            ] },
                            "notificationId": "1111",
                            "navigationEndpoint": { "watchEndpoint": { "videoId": "vid00000001" } }
                        } },
                        { "notificationRenderer": {
                            "shortMessage": { "runs": [ { "text": "@someone replied" }, { "text": ": Nice one" } ] },
                            "sentTimeText": { "runs": [ { "text": "1 day ago" } ] },
                            "videoThumbnail": { "thumbnails": [ { "url": "//i.ytimg.com/vi/vid00000002/a.jpg", "width": 320 } ] },
                            "navigationEndpoint": { "watchEndpoint": { "videoId": "vid00000002", "params": "cGFyYW1z" } }
                        } },
                        { "notificationRenderer": {
                            "shortMessage": { "simpleText": "Some Channel posted an update" },
                            "navigationEndpoint": { "browseEndpoint": { "browseId": "UCxxxxxxxx" } }
                        } },
                        { "continuationItemRenderer": { "continuationEndpoint": {
                            "getNotificationMenuEndpoint": { "ctoken": "NEXT_TOKEN" }
                        } } }
                    ] }
                }] } } }
            }]
        })
    }

    #[test]
    fn parses_inbox() {
        let p = page(&fixture());
        assert_eq!(p.continuation.as_deref(), Some("NEXT_TOKEN"));
        assert_eq!(p.shelves.len(), 1);
        let items = &p.shelves[0].items;
        assert_eq!(items.len(), 2, "the notification without a video is left out");
        assert_eq!(items[0].id, "notification:1111");
        assert_eq!(items[0].title, "A video about cats");
        assert_eq!(items[0].lines, ["Some Channel uploaded", "2 hours ago"]);
        assert_eq!(items[0].thumbnail.as_deref(), Some("https://i.ytimg.com/vi/vid00000001/big.jpg"));
        assert_eq!(items[1].title, "Nice one");
        assert_eq!(items[1].lines, ["@someone replied", "1 day ago"]);
        assert_eq!(items[1].thumbnail.as_deref(), Some("https://i.ytimg.com/vi/vid00000002/a.jpg"));
        assert!(matches!(
            &items[1].endpoint,
            Some(Endpoint::Watch { video_id, params: Some(p), .. }) if video_id == "vid00000002" && p == "cGFyYW1z"
        ));
    }

    /// Two notifications of one video (an upload, then a reply) are two cards: their ids differ,
    /// and both open the video.
    #[test]
    fn one_video_two_notifications() {
        let resp = json!({ "actions": [{ "appendContinuationItemsAction": { "continuationItems": [
            { "notificationRenderer": {
                "shortMessage": { "simpleText": "Some Channel uploaded: Cats" },
                "sentTimeText": { "simpleText": "2 days ago" },
                "navigationEndpoint": { "watchEndpoint": { "videoId": "vid00000004" } }
            } },
            { "notificationRenderer": {
                "shortMessage": { "simpleText": "@someone replied: Nice" },
                "sentTimeText": { "simpleText": "1 day ago" },
                "navigationEndpoint": { "watchEndpoint": { "videoId": "vid00000004", "params": "cGFyYW1z" } }
            } }
        ] } }] });
        let items = &page(&resp).shelves[0].items;
        assert_eq!(items.len(), 2);
        assert_ne!(items[0].id, items[1].id);
        for it in items {
            assert!(matches!(&it.endpoint, Some(Endpoint::Watch { video_id, .. }) if video_id == "vid00000004"));
        }
    }

    #[test]
    fn parses_continuation_page() {
        let resp = json!({ "actions": [{ "appendContinuationItemsAction": { "continuationItems": [
            { "notificationRenderer": {
                "shortMessage": { "simpleText": "Plain message" },
                "navigationEndpoint": { "watchEndpoint": { "videoId": "vid00000003" } }
            } }
        ] } }] });
        let p = page(&resp);
        assert_eq!(p.continuation, None);
        let items = &p.shelves[0].items;
        assert_eq!(items[0].title, "Plain message");
        assert!(items[0].lines.is_empty());
    }

    #[test]
    fn empty_inbox() {
        assert_eq!(page(&json!({})).item_count(), 0);
        assert_eq!(page(&json!({ "actions": [] })).shelves.len(), 0);
    }

    #[test]
    fn requests() {
        assert_eq!(request(None)["notificationsMenuRequestType"], "NOTIFICATIONS_MENU_REQUEST_TYPE_INBOX");
        assert_eq!(request(Some("T"))["ctoken"], "T");
    }

    /// A hand-made channel feed shaped like YouTube's: entities in the title, a Short, an entry
    /// without views or thumbnail, and one without a video id (left out).
    const FEED_A: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns:yt="http://www.youtube.com/xml/schemas/2015" xmlns:media="http://search.yahoo.com/mrss/" xmlns="http://www.w3.org/2005/Atom">
 <title>Cats &amp; Dogs</title>
 <author><name>Cats &amp; Dogs</name></author>
 <entry>
  <id>yt:video:vidA0000001</id>
  <yt:videoId>vidA0000001</yt:videoId>
  <yt:channelId>UCaaaaaaaaaaaaaaaaaaaaaa</yt:channelId>
  <title>Tom &amp; Jerry &#8211; &quot;live&quot;</title>
  <link rel="alternate" href="https://www.youtube.com/watch?v=vidA0000001"/>
  <author><name>Cats &amp; Dogs</name></author>
  <published>2026-11-17T12:00:00+00:00</published>
  <media:group>
   <media:thumbnail url="https://i1.ytimg.com/vi/vidA0000001/hqdefault.jpg" width="480" height="360"/>
   <media:community><media:statistics views="1234"/></media:community>
  </media:group>
 </entry>
 <entry>
  <yt:videoId>vidA0000002</yt:videoId>
  <title>A short one</title>
  <link rel="alternate" href="https://www.youtube.com/shorts/vidA0000002"/>
  <published>2026-11-19T12:00:00Z</published>
 </entry>
 <entry>
  <title>No video id</title>
  <published>2026-11-20T00:00:00Z</published>
 </entry>
</feed>"#;

    const FEED_B: &str = r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>Birds</title>
 <entry><yt:videoId>vidB0000001</yt:videoId><title>Birds today</title><author><name>Birds</name></author>
  <published>2026-11-18T12:00:00.000+02:00</published><media:statistics views="5"/></entry>
</feed>"#;

    #[test]
    fn rss_page_from_feeds() {
        let a = channel_groups::parse_feed(FEED_A, "UCaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let b = channel_groups::parse_feed(FEED_B, "UCbbbbbbbbbbbbbbbbbbbbbb").unwrap();
        assert_eq!(a.entries.len(), 2, "the entry without a video id is left out");
        let now = channel_groups::parse_rfc3339("2026-11-20T12:00:00Z").unwrap();
        let p = feeds_page([&a, &b], now);
        assert_eq!(p.title, None);
        assert_eq!(p.continuation, None);
        assert_eq!(p.shelves.len(), 1);
        assert_eq!(p.shelves[0].title, "");
        let items = &p.shelves[0].items;
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["vidA0000002", "vidB0000001", "vidA0000001"], "newest first");
        assert_eq!(items[2].title, "Tom & Jerry \u{2013} \"live\"");
        assert_eq!(items[2].lines, ["Cats & Dogs", "1.2K views \u{2022} 3 days ago"]);
        assert_eq!(items[2].thumbnail.as_deref(), Some("https://i1.ytimg.com/vi/vidA0000001/hqdefault.jpg"));
        assert!(matches!(&items[2].endpoint, Some(Endpoint::Watch { video_id, .. }) if video_id == "vidA0000001"));
        // No name in the entry: the feed's; no views: the age alone; no thumbnail: YouTube's.
        assert_eq!(items[0].lines, ["Cats & Dogs", "1 day ago"]);
        assert_eq!(items[0].kind, ItemKind::Short);
        assert_eq!(items[0].thumbnail.as_deref(), Some("https://i.ytimg.com/vi/vidA0000002/hqdefault.jpg"));
        assert_eq!(items[1].lines, ["Birds", "5 views \u{2022} 2 days ago"]);
    }

    #[test]
    fn rss_page_caps_and_empties() {
        let mut f = ChannelFeed { channel_id: "UC1".into(), title: "One".into(), entries: Vec::new() };
        for i in 0..(MAX_RSS_ITEMS as u64 + 15) {
            f.entries.push(channel_groups::FeedEntry { video_id: format!("v{i}"), title: format!("T{i}"), published: 1000 + i, ..Default::default() });
        }
        let p = feeds_page([&f], 5000);
        assert_eq!(p.item_count(), MAX_RSS_ITEMS);
        assert_eq!(p.shelves[0].items[0].id, format!("v{}", MAX_RSS_ITEMS as u64 + 14));
        assert_eq!(feeds_page([], 0).shelves.len(), 0);
        let http = reqwest::blocking::Client::new();
        assert_eq!(rss_page(&http, &[]).unwrap().item_count(), 0, "no channels: nothing fetched");
    }

    #[test]
    fn message_split() {
        assert_eq!(split_message("A: b: c"), (Some("A".into()), "b: c".into()));
        assert_eq!(split_message("No colon"), (None, "No colon".into()));
        assert_eq!(split_message("Trailing:"), (None, "Trailing:".into()));
    }
}

/// Live diagnostics (ignored: they need the network; the inbox ones also `token.json` in the
/// repository root): `cargo test -p yt-core notifications::live -- --ignored --nocapture`.
#[cfg(test)]
mod live {
    /// The repository root, where a developer's `token.json` lives.
    fn repo_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// What the inbox answers today (an HTTP 400 "Precondition check failed" since late 2026).
    #[test]
    #[ignore]
    fn live_inbox() {
        crate::paths::set_data_dir(repo_root());
        let mut s = crate::session::Session::new().unwrap();
        assert!(s.signed_in(), "no token.json");
        match s.raw(super::ENDPOINT, super::request(None)) {
            Ok(v) => {
                let p = super::page(&v);
                println!("ok: {} items, continuation {}", p.item_count(), p.continuation.is_some());
            }
            Err(e) => println!("error: {e:#}"),
        }
    }

    /// The inbox with other client names, versions and headers, to see whether any of them is
    /// still served.
    #[test]
    #[ignore]
    fn live_inbox_variants() {
        use serde_json::json;
        crate::paths::set_data_dir(repo_root());
        let mut s = crate::session::Session::new().unwrap();
        let bearer = s.bearer().unwrap().expect("signed in");
        let api = s.api();
        let (ctx, _, ver) = crate::innertube::Client::Tv.context(api.cfg);
        println!("our version {ver}");
        // (name, client name, client name id, version, referer)
        let variants = [
            ("ours+referer", "TVHTML5", "7", ver.clone(), true),
            ("st-version", "TVHTML5", "7", "7.20260901.15.00".to_string(), true),
            ("downgraded", "TVHTML5", "7", "5.20260901".to_string(), true),
            ("web", "WEB", "1", "2.20260907.06.00".to_string(), false),
        ];
        for (name, cname, cid, cver, referer) in variants {
            let mut c = ctx.clone();
            c["clientName"] = json!(cname);
            c["clientVersion"] = json!(cver);
            let mut body = super::request(None);
            body["context"] = json!({ "client": c });
            let mut req = api
                .http
                .post(format!("https://www.youtube.com/youtubei/v1/{}?prettyPrint=false", super::ENDPOINT))
                .header("User-Agent", crate::bootstrap::TV_UA)
                .header("X-YouTube-Client-Name", cid)
                .header("X-YouTube-Client-Version", cver.as_str())
                .header("Origin", "https://www.youtube.com")
                .bearer_auth(&bearer)
                .json(&body);
            if referer {
                req = req.header("Referer", "https://www.youtube.com/tv");
            }
            let r = req.send().unwrap();
            let st = r.status();
            let v: serde_json::Value = r.json().unwrap_or_default();
            println!("{name}: {st} items={} error={}", super::page(&v).item_count(), v["error"]["message"]);
        }
    }

    /// The fallback: one public channel's RSS feed through the notifications page.
    #[test]
    #[ignore]
    fn live_rss() {
        let http = crate::net::builder().timeout(std::time::Duration::from_secs(30)).build().unwrap();
        let p = super::rss_page(&http, &["UCBJycsmduvYEL83R_U4JriQ".to_string()]).unwrap();
        println!("rss: {} items", p.item_count());
        for it in p.shelves.iter().flat_map(|s| &s.items).take(3) {
            println!("  {} | {}", it.title, it.lines.join(" | "));
        }
        assert!(p.item_count() > 0);
    }
}

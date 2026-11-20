//! The account's notifications inbox (new uploads from channels with the bell on, replies...),
//! fetched the way MediaServiceCore's `NotificationsServiceInt` does: `notification/
//! get_notification_menu` with the TVHTML5 client and the TV sign-in token, asking for the
//! `NOTIFICATIONS_MENU_REQUEST_TYPE_INBOX` menu. YouTube answers with a popup menu of
//! `notificationRenderer`s; each one that points at a video becomes a card.

use crate::innertube::text;
use crate::model::{Endpoint, Item, ItemKind, Page, Shelf};
use serde_json::{json, Value};

/// The endpoint (under `youtubei/v1/`) that serves the inbox.
pub const ENDPOINT: &str = "notification/get_notification_menu";

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

    #[test]
    fn message_split() {
        assert_eq!(split_message("A: b: c"), (Some("A".into()), "b: c".into()));
        assert_eq!(split_message("No colon"), (None, "No colon".into()));
        assert_eq!(split_message("Trailing:"), (None, "Trailing:".into()));
    }
}

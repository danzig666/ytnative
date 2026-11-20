//! Live chat of a live stream, the way SmartTube's MediaServiceCore reads it: the watch page's
//! `conversationBar.liveChatRenderer` carries the first continuation; posting a continuation to
//! `live_chat/get_live_chat` returns `continuationContents.liveChatContinuation` with the new
//! chat actions and the next continuation, together with `timeoutMs`: how long to wait before
//! asking again.
//!
//! The TV client's watch page no longer has the conversation bar (2026-11:
//! `WatchInfo::live_chat_continuation` stays empty for live streams), so the first continuation
//! comes from the web client's watch page ([`first_continuation`]). The chat itself is then
//! polled as the TV client, as SmartTube does: it answers the web site's continuation. The chat
//! is public, so the requests go without the account's token (and without the session lock,
//! which the app's other loads need while the chat is open for minutes).

use crate::bootstrap::TvConfig;
use crate::innertube::{text, Api, Client};
use crate::session::Session;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// What kind of chat item a message is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Kind {
    #[default]
    Text,
    /// A Super Chat or Super Sticker: the amount as YouTube prints it ("€5.00").
    Paid { amount: String },
    /// A new member, a membership milestone or a gift of memberships: the line YouTube puts over
    /// it ("Welcome to the channel!", "Member for 6 months").
    Membership { header: String },
}

/// Who the author is in this chat (their badge).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Role {
    #[default]
    Viewer,
    Member,
    Moderator,
    Owner,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub author: String,
    /// The message, emoji written as their names (":red_heart:"), since the TV's fonts have no
    /// emoji pictures.
    pub text: String,
    pub kind: Kind,
    pub role: Role,
    /// A Super Chat's colour (0xAARRGGBB), the tier YouTube shows it in.
    pub color: Option<u32>,
    /// When it was sent, in seconds since 1970 (`timestampUsec`; `None` when not given).
    #[serde(default)]
    pub sent_secs: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LiveChatPage {
    /// New messages, oldest first.
    pub messages: Vec<ChatMessage>,
    /// Ids of messages a moderator removed since the last page.
    pub removed: Vec<String>,
    /// Ask again with this; None: the chat is over (the stream ended).
    pub continuation: Option<String>,
    /// How long YouTube asks to wait before the next request.
    pub timeout_ms: Option<u64>,
}

impl Session {
    /// The next page of a live chat from a continuation token.
    pub fn live_chat(&self, continuation: &str) -> Result<LiveChatPage> {
        fetch(&self.http, &self.cfg, continuation)
    }
}

/// One `get_live_chat` request; needs only the HTTP client and the TV configuration, so a caller
/// can make it without holding the session.
pub fn fetch(http: &reqwest::blocking::Client, cfg: &TvConfig, continuation: &str) -> Result<LiveChatPage> {
    let api = Api { http, cfg };
    Ok(parse_live_chat(&api.call(Client::Tv, "live_chat/get_live_chat", json!({ "continuation": continuation }), None)?))
}

/// The first continuation of a video's live chat, from the web client's watch page (with the web
/// client settings `comment_threads` keeps; a fixed version if they cannot be read). None: the
/// video has no live chat (not live, or the chat is off).
pub fn first_continuation(http: &reqwest::blocking::Client, video_id: &str) -> Result<Option<String>> {
    let ctx = crate::comment_threads::context(http, false)
        .unwrap_or_else(|_| json!({ "client": { "clientName": "WEB", "clientVersion": WEB_VERSION, "hl": "en", "gl": "US" } }));
    let version = ctx["client"]["clientVersion"].as_str().unwrap_or(WEB_VERSION).to_string();
    let resp: Value = http
        .post("https://www.youtube.com/youtubei/v1/next?prettyPrint=false")
        .header("User-Agent", crate::comment_threads::UA)
        .header("X-YouTube-Client-Name", "1")
        .header("X-YouTube-Client-Version", version)
        .json(&json!({ "context": ctx, "videoId": video_id }))
        .send()?
        .error_for_status()?
        .json()?;
    Ok(chat_of_watch_page(&resp))
}

/// A web client version known to answer, for when the home page's cannot be read.
const WEB_VERSION: &str = "2.20251101.00.00";

/// The live chat continuation of a web watch page: its conversation bar's (the "Top chat" view,
/// as the web site opens with). A finished stream's chat replay is left out: `get_live_chat`
/// does not serve it.
pub fn chat_of_watch_page(resp: &Value) -> Option<String> {
    let bar = &resp["contents"]["twoColumnWatchNextResults"]["conversationBar"];
    let r = bar.get("liveChatRenderer")?;
    if r["isReplay"].as_bool() == Some(true) {
        return None;
    }
    continuation(&r["continuations"]).map(|(t, _)| t)
}

pub fn parse_live_chat(resp: &Value) -> LiveChatPage {
    let c = &resp["continuationContents"]["liveChatContinuation"];
    let mut page = LiveChatPage::default();
    for a in c["actions"].as_array().into_iter().flatten() {
        action(a, &mut page);
    }
    if let Some((token, timeout)) = continuation(&c["continuations"]) {
        page.continuation = Some(token);
        page.timeout_ms = timeout;
    }
    page
}

fn action(a: &Value, page: &mut LiveChatPage) {
    if let Some(item) = a["addChatItemAction"].get("item") {
        page.messages.extend(message(item));
    } else if let Some(id) = a["removeChatItemAction"]["targetItemId"].as_str() {
        page.removed.push(id.to_string());
    } else if let Some(r) = a.get("replayChatItemAction") {
        // A finished stream's chat replay wraps the same actions.
        for x in r["actions"].as_array().into_iter().flatten() {
            action(x, page);
        }
    }
}

/// The continuation to poll next and its wait. A live chat is continued by
/// `invalidationContinuationData` or `timedContinuationData`; the first request's
/// `reloadContinuationData` and a replay's `liveChatReplayContinuationData` carry no wait.
fn continuation(cs: &Value) -> Option<(String, Option<u64>)> {
    const KINDS: [&str; 4] = ["invalidationContinuationData", "timedContinuationData", "reloadContinuationData", "liveChatReplayContinuationData"];
    cs.as_array()?.iter().find_map(|c| {
        KINDS.iter().find_map(|k| {
            let d = c.get(*k)?;
            let token = d["continuation"].as_str().filter(|t| !t.is_empty())?;
            Some((token.to_string(), d["timeoutMs"].as_u64()))
        })
    })
}

fn message(item: &Value) -> Option<ChatMessage> {
    if let Some(r) = item.get("liveChatTextMessageRenderer") {
        return Some(base(r, Kind::Text));
    }
    if let Some(r) = item.get("liveChatPaidMessageRenderer") {
        let mut m = base(r, Kind::Paid { amount: text(&r["purchaseAmountText"]).unwrap_or_default() });
        m.color = r["headerBackgroundColor"].as_u64().or(r["bodyBackgroundColor"].as_u64()).map(|c| c as u32);
        return Some(m);
    }
    if let Some(r) = item.get("liveChatPaidStickerRenderer") {
        let mut m = base(r, Kind::Paid { amount: text(&r["purchaseAmountText"]).unwrap_or_default() });
        m.color = r["backgroundColor"].as_u64().or(r["moneyChipBackgroundColor"].as_u64()).map(|c| c as u32);
        return Some(m);
    }
    if let Some(r) = item.get("liveChatMembershipItemRenderer") {
        // A milestone puts the months in the primary line; a new member only has the subtext.
        let header = text(&r["headerPrimaryText"]).filter(|s| !s.is_empty()).or_else(|| text(&r["headerSubtext"])).unwrap_or_default();
        let mut m = base(r, Kind::Membership { header: header.clone() });
        if m.text.is_empty() {
            // Nothing written with it: the subtext ("Welcome to ...!") is the message.
            m.text = text(&r["headerSubtext"]).filter(|s| *s != header).unwrap_or_default();
        }
        return Some(m);
    }
    if let Some(r) = item.get("liveChatSponsorshipsGiftPurchaseAnnouncementRenderer") {
        let h = &r["header"]["liveChatSponsorshipsHeaderRenderer"];
        let mut m = base(h, Kind::Membership { header: rich(&h["primaryText"]) });
        m.id = r["id"].as_str().unwrap_or_default().to_string();
        return Some(m);
    }
    None
}

/// The fields every message kind shares.
fn base(r: &Value, kind: Kind) -> ChatMessage {
    ChatMessage {
        id: r["id"].as_str().unwrap_or_default().to_string(),
        author: text(&r["authorName"]).unwrap_or_default(),
        text: rich(&r["message"]),
        kind,
        role: role(&r["authorBadges"]),
        color: None,
        sent_secs: sent_secs(r),
    }
}

/// `timestampUsec` (microseconds since 1970, sent as a string) in seconds.
fn sent_secs(r: &Value) -> Option<i64> {
    let us = match &r["timestampUsec"] {
        Value::String(s) => s.parse::<i64>().ok()?,
        v => v.as_i64()?,
    };
    (us > 0).then_some(us / 1_000_000)
}

/// A message's runs as text: emoji runs become their first shortcut (":smiling_face:"), or the
/// emoji itself when it has none.
fn rich(v: &Value) -> String {
    let Some(runs) = v["runs"].as_array() else { return text(v).unwrap_or_default() };
    let mut out = String::new();
    for r in runs {
        if let Some(t) = r["text"].as_str() {
            out.push_str(t);
        } else if let Some(e) = r.get("emoji") {
            let name = e["shortcuts"].as_array().and_then(|s| s.first()).and_then(Value::as_str).or(e["emojiId"].as_str());
            out.push_str(name.unwrap_or_default());
        }
    }
    out
}

/// The highest role among the author's badges (an owner's message carries the owner badge, a
/// member's the custom member badge with a "Member" tooltip).
fn role(badges: &Value) -> Role {
    let mut best = Role::Viewer;
    for b in badges.as_array().into_iter().flatten() {
        let b = &b["liveChatAuthorBadgeRenderer"];
        let r = match b["icon"]["iconType"].as_str() {
            Some("OWNER") => Role::Owner,
            Some("MODERATOR") => Role::Moderator,
            Some(_) => Role::Viewer,
            None if b.get("customThumbnail").is_some() => Role::Member,
            None => Role::Viewer,
        };
        best = best.max_role(r);
    }
    best
}

impl Role {
    fn rank(self) -> u8 {
        match self {
            Role::Viewer => 0,
            Role::Member => 1,
            Role::Moderator => 2,
            Role::Owner => 3,
        }
    }
    fn max_role(self, other: Role) -> Role {
        if other.rank() > self.rank() { other } else { self }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand-written page in the shape `get_live_chat` answers with (no real chat in it).
    const FIXTURE: &str = include_str!("../fixtures/live_chat.json");

    #[test]
    fn fixture_page() {
        let v: Value = serde_json::from_str(FIXTURE).unwrap();
        let p = parse_live_chat(&v);
        assert_eq!(p.continuation.as_deref(), Some("NEXT-TOKEN"));
        assert_eq!(p.timeout_ms, Some(5000));
        assert_eq!(p.removed, vec!["msg-gone".to_string()]);
        let ids: Vec<&str> = p.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["msg-1", "msg-2", "msg-3", "msg-4", "msg-5", "msg-6", "msg-7"]);

        let m = &p.messages[0];
        assert_eq!((m.author.as_str(), m.text.as_str(), m.role), ("Viewer One", "hello :waving_hand: everyone", Role::Viewer));
        assert_eq!(m.kind, Kind::Text);
        // An emoji without shortcuts is written as itself; a custom one by its name.
        assert_eq!(p.messages[1].text, "nice \u{2764} :channel-wave:");
        assert_eq!(p.messages[1].role, Role::Moderator);

        let paid = &p.messages[2];
        assert_eq!(paid.kind, Kind::Paid { amount: "\u{20AC}5.00".into() });
        assert_eq!(paid.text, "great stream");
        assert_eq!(paid.color, Some(0xFF1E88E5));
        assert_eq!(paid.role, Role::Member);

        let sticker = &p.messages[3];
        assert_eq!(sticker.kind, Kind::Paid { amount: "$2.00".into() });
        assert_eq!(sticker.text, "");

        assert_eq!(p.messages[4].kind, Kind::Membership { header: "Welcome to the channel!".into() });
        assert_eq!(p.messages[4].text, "");
        let milestone = &p.messages[5];
        assert_eq!(milestone.kind, Kind::Membership { header: "Member for 6 months".into() });
        assert_eq!(milestone.text, "still here");

        let gift = &p.messages[6];
        assert_eq!(gift.author, "Gift Giver");
        assert_eq!(gift.kind, Kind::Membership { header: "Gifted 5 memberships".into() });
        assert_eq!(p.messages.iter().filter(|m| m.role == Role::Owner).count(), 0);
    }

    #[test]
    fn the_time_a_message_was_sent() {
        let m = |ts: Value| base(&json!({"id": "x", "timestampUsec": ts}), Kind::Text).sent_secs;
        assert_eq!(m(json!("1763650000123456")), Some(1_763_650_000));
        assert_eq!(m(json!(1763650000123456i64)), Some(1_763_650_000));
        assert_eq!(m(json!("")), None);
        assert_eq!(base(&json!({"id": "x"}), Kind::Text).sent_secs, None);
    }

    #[test]
    fn owner_badge_wins() {
        let b = json!([
            {"liveChatAuthorBadgeRenderer": {"customThumbnail": {}, "tooltip": "Member"}},
            {"liveChatAuthorBadgeRenderer": {"icon": {"iconType": "OWNER"}}},
            {"liveChatAuthorBadgeRenderer": {"icon": {"iconType": "VERIFIED"}}}
        ]);
        assert_eq!(role(&b), Role::Owner);
        assert_eq!(role(&Value::Null), Role::Viewer);
    }

    #[test]
    fn first_page_and_end_of_chat() {
        // The first answer (to the watch page's reload token) has a timed continuation.
        let v = json!({"continuationContents": {"liveChatContinuation": {
            "continuations": [{"timedContinuationData": {"continuation": "T", "timeoutMs": 10000}}],
            "actions": [{"addChatItemAction": {"item": {"liveChatViewerEngagementMessageRenderer": {"id": "x"}}}}]
        }}});
        let p = parse_live_chat(&v);
        assert!(p.messages.is_empty(), "notices from YouTube are not chat");
        assert_eq!((p.continuation.as_deref(), p.timeout_ms), (Some("T"), Some(10000)));
        // An ended stream answers without a continuation.
        let p = parse_live_chat(&json!({"continuationContents": {"liveChatContinuation": {"actions": []}}}));
        assert_eq!(p.continuation, None);
        assert_eq!(parse_live_chat(&json!({})), LiveChatPage::default());
    }

    /// Against YouTube: a stream live now (found with the search's "Live" filter), its chat's
    /// first continuation from the web watch page, then two chat requests as the TV client.
    #[test]
    #[ignore]
    fn live_stream_chat() {
        let dir = std::env::temp_dir().join("ytnative-live-chat-test");
        let _ = std::fs::create_dir_all(&dir);
        crate::paths::set_data_dir(dir);
        let mut s = Session::new().unwrap();
        let found = s.search("news", Some("EgJAAQ%3D%3D")).unwrap();
        let ids: Vec<String> = found.shelves.iter().flat_map(|sh| sh.items.iter()).filter(|i| i.kind == crate::model::ItemKind::Live).map(|i| i.id.clone()).take(5).collect();
        let token = ids.iter().find_map(|id| first_continuation(&s.http, id).ok().flatten()).expect("a live stream offers its chat");
        let p = s.live_chat(&token).unwrap();
        eprintln!("first: {} messages ({} with the time sent), wait {:?}", p.messages.len(), p.messages.iter().filter(|m| m.sent_secs.is_some()).count(), p.timeout_ms);
        assert!(p.continuation.is_some());
        let p2 = s.live_chat(p.continuation.as_deref().unwrap()).unwrap();
        eprintln!("second: {} messages, wait {:?}", p2.messages.len(), p2.timeout_ms);
        assert!(p2.continuation.is_some());
    }

    #[test]
    fn web_watch_page_chat() {
        let bar = |extra: Value| {
            let mut r = json!({"continuations": [{"reloadContinuationData": {"continuation": "FIRST"}}]});
            for (k, v) in extra.as_object().unwrap() {
                r[k] = v.clone();
            }
            json!({"contents": {"twoColumnWatchNextResults": {"conversationBar": {"liveChatRenderer": r}}}})
        };
        assert_eq!(chat_of_watch_page(&bar(json!({}))).as_deref(), Some("FIRST"));
        assert_eq!(chat_of_watch_page(&bar(json!({"isReplay": true}))), None);
        assert_eq!(chat_of_watch_page(&json!({"contents": {"twoColumnWatchNextResults": {}}})), None);
    }

    #[test]
    fn replay_actions_are_read() {
        let v = json!({"continuationContents": {"liveChatContinuation": {
            "continuations": [{"liveChatReplayContinuationData": {"continuation": "R"}}],
            "actions": [{"replayChatItemAction": {"actions": [{"addChatItemAction": {"item": {"liveChatTextMessageRenderer": {
                "id": "r1", "authorName": {"simpleText": "A"}, "message": {"runs": [{"text": "old"}]}
            }}}}]}}]
        }}});
        let p = parse_live_chat(&v);
        assert_eq!(p.messages.len(), 1);
        assert_eq!(p.messages[0].text, "old");
        assert_eq!((p.continuation.as_deref(), p.timeout_ms), (Some("R"), None));
    }
}

//! Replies to replies. YouTube threads replies up to five levels deep, but only its web site
//! serves the deeper levels: the TV service returns nothing under a reply. So a reply's replies
//! are loaded from the web `next` endpoint, with a continuation built from the reply's id (the
//! shape the web site itself uses for "N replies"), in the web's comment format (view models
//! plus entities).
//!
//! The web site sends threaded replies only to clients in its threading experiment, which it
//! names in `context.client.configInfo`; that and the client version are read from the YouTube
//! home page and kept for a day (`web_comments.json`).

use crate::comments::{Comment, CommentsPage};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

/// Continuations served by the web site carry this prefix (the TV ones have none).
pub const PREFIX: &str = "web:";

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
const CACHE_FILE: &str = "web_comments.json";
const CACHE_SECS: u64 = 24 * 3600;

/// The continuation for the replies of the reply a TV detail-view token points at: the reply's
/// id, the video and its channel are read from the token and put in the web's shape.
pub fn token_for_reply(tv_token: &str) -> Option<String> {
    let bytes = decode_b64(tv_token)?;
    let f6 = field(&bytes, 6)?;
    let target = field(f6, 3)?;
    let id = std::str::from_utf8(field(target, 2)?).ok()?;
    let channel = std::str::from_utf8(field(target, 5)?).ok()?;
    let video = std::str::from_utf8(field(target, 6)?).ok()?;
    Some(format!("{PREFIX}{}", reply_token(video, channel, id)))
}

/// The web continuation for the replies of comment `id` (any level).
pub fn reply_token(video: &str, channel: &str, id: &str) -> String {
    let mut target = Vec::new();
    put_bytes(2, id.as_bytes(), &mut target);
    put_bytes(4, &[0x08, 0x00], &mut target);
    put_bytes(5, channel.as_bytes(), &mut target);
    put_bytes(6, video.as_bytes(), &mut target);
    put_int(8, 1, &mut target);
    put_int(9, 10, &mut target);
    put_bytes(16, &[0x08, 0x01], &mut target);
    let mut f6 = Vec::new();
    put_bytes(3, &target, &mut f6);
    put_bytes(8, format!("comment-replies-item-{id}").as_bytes(), &mut f6);
    let mut v = Vec::new();
    put_bytes(2, video.as_bytes(), &mut v);
    let mut out = Vec::new();
    put_bytes(2, &v, &mut out);
    put_int(3, 6, &mut out);
    put_bytes(6, &f6, &mut out);
    encode_b64(&out)
}

/// A page of replies from a web continuation (without the [`PREFIX`]).
pub fn replies(http: &reqwest::blocking::Client, token: &str) -> Result<CommentsPage> {
    let (video, channel) = token_target(token).unwrap_or_default();
    let mut ctx = context(http, false)?;
    let mut v = call(http, &ctx, token)?;
    if !threaded(&v) && v["error"].is_null() {
        // The kept settings may have aged out of the experiment: read them again, once.
        ctx = context(http, true)?;
        v = call(http, &ctx, token)?;
    }
    if let Some(e) = v["error"]["message"].as_str() {
        return Err(anyhow!("web comments: {e}"));
    }
    Ok(parse(&v, &video, &channel))
}

fn call(http: &reqwest::blocking::Client, ctx: &Value, token: &str) -> Result<Value> {
    let version = ctx["client"]["clientVersion"].as_str().unwrap_or_default().to_string();
    http.post("https://www.youtube.com/youtubei/v1/next?prettyPrint=false")
        .header("User-Agent", UA)
        .header("X-YouTube-Client-Name", "1")
        .header("X-YouTube-Client-Version", version)
        .json(&json!({ "context": ctx, "continuation": token }))
        .send()?
        .json()
        .context("web comments")
}

/// A response in the threaded format (or one with no replies at all).
fn threaded(v: &Value) -> bool {
    let s = v.to_string();
    s.contains("replyLevel") || !s.contains("commentViewModel")
}

/// The web client's context: version and `configInfo` from the home page, kept for a day.
fn context(http: &reqwest::blocking::Client, fresh: bool) -> Result<Value> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let path = crate::paths::data_file(CACHE_FILE);
    if !fresh {
        if let Some(c) = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
            if now.saturating_sub(c["at"].as_u64().unwrap_or(0)) < CACHE_SECS && c["version"].is_string() {
                return Ok(client(&c["version"], &c["configInfo"]));
            }
        }
    }
    let html = http.get("https://www.youtube.com/?hl=en").header("User-Agent", UA).header("Accept-Language", "en").send()?.text()?;
    let key = "\"INNERTUBE_CONTEXT\":";
    let at = html.find(key).ok_or_else(|| anyhow!("web comments: no client context on the home page"))? + key.len();
    let page_ctx: Value = serde_json::Deserializer::from_str(&html[at..]).into_iter::<Value>().next().ok_or_else(|| anyhow!("web comments: empty context"))??;
    let version = page_ctx["client"]["clientVersion"].clone();
    let config = page_ctx["client"]["configInfo"].clone();
    if !version.is_string() {
        return Err(anyhow!("web comments: no client version"));
    }
    let _ = crate::fsutil::write_atomic(&path, json!({ "at": now, "version": version, "configInfo": config }).to_string().as_bytes());
    Ok(client(&version, &config))
}

fn client(version: &Value, config: &Value) -> Value {
    json!({ "client": { "clientName": "WEB", "clientVersion": version, "hl": "en", "gl": "US", "configInfo": config } })
}

/// The video and channel a web replies continuation is about (for its replies' own tokens).
fn token_target(token: &str) -> Option<(String, String)> {
    let bytes = decode_b64(token)?;
    let target = field(field(&bytes, 6)?, 3)?;
    Some((String::from_utf8(field(target, 6)?.to_vec()).ok()?, String::from_utf8(field(target, 5)?.to_vec()).ok()?))
}

/// The direct replies in a web response (the deeper levels nested in their threads are left for
/// opening those replies), and the token of the next page.
pub fn parse(v: &Value, video: &str, channel: &str) -> CommentsPage {
    let mut entities = std::collections::HashMap::new();
    for m in v["frameworkUpdates"]["entityBatchUpdate"]["mutations"].as_array().into_iter().flatten() {
        if let Some(k) = m["entityKey"].as_str() {
            entities.insert(k.to_string(), &m["payload"]);
        }
    }
    let mut page = CommentsPage::default();
    let items = v["onResponseReceivedEndpoints"].as_array().into_iter().flatten().flat_map(|e| {
        let a = if e["appendContinuationItemsAction"].is_object() { &e["appendContinuationItemsAction"] } else { &e["reloadContinuationItemsCommand"] };
        a["continuationItems"].as_array().into_iter().flatten()
    });
    for it in items {
        let vm = if it["commentThreadRenderer"].is_object() { &it["commentThreadRenderer"]["commentViewModel"]["commentViewModel"] } else { &it["commentViewModel"] };
        if vm.is_object() {
            if let Some(c) = comment(vm, &entities, video, channel) {
                page.comments.push(c);
            }
        } else if let Some(t) = continuation_token(&it["continuationItemRenderer"]) {
            page.next = Some(format!("{PREFIX}{t}"));
        }
    }
    page
}

fn comment(vm: &Value, entities: &std::collections::HashMap<String, &Value>, video: &str, channel: &str) -> Option<Comment> {
    let e = &entities.get(vm["commentKey"].as_str()?)?["commentEntityPayload"];
    let p = &e["properties"];
    let id = p["commentId"].as_str()?.to_string();
    let toolbar = &e["toolbar"];
    let reply_count = toolbar["replyCountA11y"].as_str().and_then(count).or_else(|| toolbar["replyCount"].as_str().and_then(count)).unwrap_or(0);
    let state = vm["toolbarStateKey"].as_str().and_then(|k| entities.get(k)).map(|s| &s["engagementToolbarStateEntityPayload"]);
    let likes = toolbar["likeCountNotliked"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    Some(Comment {
        author: e["author"]["displayName"].as_str().unwrap_or_default().to_string(),
        author_avatar: e["author"]["avatarThumbnailUrl"].as_str().or_else(|| e["avatar"]["image"]["sources"][0]["url"].as_str()).map(str::to_string),
        author_channel_id: e["author"]["channelId"].as_str().map(str::to_string),
        text: p["content"]["content"].as_str().unwrap_or_default().to_string(),
        published_text: p["publishedTime"].as_str().unwrap_or_default().to_string(),
        like_count_text: likes,
        replies_continuation: (reply_count > 0 && !video.is_empty()).then(|| format!("{PREFIX}{}", reply_token(video, channel, &id))),
        reply_count,
        is_pinned: vm["pinnedText"].is_string(),
        is_hearted: state.is_some_and(|s| s["heartState"].as_str() == Some("TOOLBAR_HEART_STATE_HEARTED")),
        liked: state.is_some_and(|s| s["likeState"].as_str() == Some("TOOLBAR_LIKE_STATE_LIKED")),
        id,
    })
}

fn continuation_token(r: &Value) -> Option<String> {
    r["continuationEndpoint"]["continuationCommand"]["token"]
        .as_str()
        .or_else(|| r["button"]["buttonRenderer"]["command"]["continuationCommand"]["token"].as_str())
        .map(str::to_string)
}

/// "3 replies", "1 reply", "1.2K replies".
fn count(s: &str) -> Option<u32> {
    let t: String = s.trim().chars().take_while(|c| c.is_ascii_digit() || matches!(c, ',' | '.' | 'K' | 'k' | 'M' | 'm')).collect();
    let (num, mul) = match t.chars().last()? {
        'K' | 'k' => (&t[..t.len() - 1], 1e3),
        'M' | 'm' => (&t[..t.len() - 1], 1e6),
        _ => (&t[..], 1.0),
    };
    let n: f64 = num.replace(',', "").parse().ok()?;
    Some((n * mul) as u32)
}

// ---- A little protobuf and base64 ---------------------------------------------------------------

fn varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let mut n = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*i)?;
        *i += 1;
        n |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some(n);
        }
    }
    None
}

/// The first length-delimited field `want` of a message.
fn field(b: &[u8], want: u64) -> Option<&[u8]> {
    let mut i = 0;
    while i < b.len() {
        let key = varint(b, &mut i)?;
        match key & 7 {
            0 => {
                varint(b, &mut i)?;
            }
            1 => i += 8,
            5 => i += 4,
            2 => {
                let len = varint(b, &mut i)? as usize;
                let data = b.get(i..i + len)?;
                i += len;
                if key >> 3 == want {
                    return Some(data);
                }
            }
            _ => return None,
        }
    }
    None
}

fn put_varint(mut n: u64, out: &mut Vec<u8>) {
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn put_bytes(f: u64, data: &[u8], out: &mut Vec<u8>) {
    put_varint(f << 3 | 2, out);
    put_varint(data.len() as u64, out);
    out.extend_from_slice(data);
}

fn put_int(f: u64, n: u64, out: &mut Vec<u8>) {
    put_varint(f << 3, out);
    put_varint(n, out);
}

fn decode_b64(s: &str) -> Option<Vec<u8>> {
    let s = s.replace("%3D", "=");
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::new();
    let bytes: Vec<u8> = s.bytes().filter(|c| *c != b'=').collect();
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (k, c) in chunk.iter().enumerate() {
            n |= val(*c)? << (18 - 6 * k);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}

/// URL-safe base64 with the padding percent-encoded, as YouTube writes continuations.
fn encode_b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        for (k, shift) in [(1, 6), (2, 0)] {
            if chunk.len() > k {
                out.push(T[(n >> shift) as usize & 63] as char);
            } else {
                out.push_str("%3D");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TV detail token of a reply (as the TV service sends it) turns into the web token the web
    /// site uses for that reply's "N replies".
    #[test]
    fn tv_reply_token_becomes_the_web_one() {
        let (video, channel, id) = ("dQw4w9WgXcQ", "UCuAXFkgsw1L7xaCfnd5JJOw", "Ugzemy2nvw_7EyaIWj54AaABAg.91V-vzraTSC91_znUCa41-");
        // The TV shape: field 6 { 1: "", 3: { 2: id, 4, 5: channel, 6: video, 9: 0, 16, 18 }, 5: 0 }.
        let mut target = Vec::new();
        put_bytes(2, id.as_bytes(), &mut target);
        put_bytes(4, &[0x08, 0x00], &mut target);
        put_bytes(5, channel.as_bytes(), &mut target);
        put_bytes(6, video.as_bytes(), &mut target);
        put_int(9, 0, &mut target);
        let mut f6 = Vec::new();
        put_bytes(1, b"", &mut f6);
        put_bytes(3, &target, &mut f6);
        let mut v = Vec::new();
        put_bytes(2, video.as_bytes(), &mut v);
        let mut tv = Vec::new();
        put_bytes(2, &v, &mut tv);
        put_int(3, 6, &mut tv);
        put_bytes(6, &f6, &mut tv);
        let web = token_for_reply(&encode_b64(&tv)).unwrap();
        assert_eq!(web, format!("{PREFIX}{}", reply_token(video, channel, id)));
        assert_eq!(token_target(web.strip_prefix(PREFIX).unwrap()), Some((video.to_string(), channel.to_string())));
        let raw = decode_b64(web.strip_prefix(PREFIX).unwrap()).unwrap();
        assert!(raw.windows(id.len()).any(|w| w == id.as_bytes()));
        assert!(raw.windows(21).any(|w| w == b"comment-replies-item-"));
        assert_eq!(token_for_reply("not a token"), None);
    }

    #[test]
    fn base64_round_trips() {
        for n in 0..40u8 {
            let data: Vec<u8> = (0..n).map(|k| k.wrapping_mul(37)).collect();
            assert_eq!(decode_b64(&encode_b64(&data)).unwrap(), data);
        }
    }

    /// The web format: direct replies (threads at the top), their deeper replies left nested, and
    /// the next page's token.
    #[test]
    fn parses_web_replies() {
        let ent = |key: &str, id: &str, text: &str, level: u32, replies: &str| {
            json!({"entityKey": key, "payload": {"commentEntityPayload": {
                "properties": {"commentId": id, "content": {"content": text}, "publishedTime": "2 days ago", "replyLevel": level},
                "author": {"displayName": "@who", "channelId": "UCx", "avatarThumbnailUrl": "https://a/s88"},
                "toolbar": {"likeCountNotliked": " 12 ", "replyCountA11y": replies}
            }}})
        };
        let v = json!({
            "onResponseReceivedEndpoints": [{"appendContinuationItemsAction": {"continuationItems": [
                {"commentThreadRenderer": {"commentViewModel": {"commentViewModel": {"commentKey": "k1", "toolbarStateKey": "s1"}},
                    "replies": {"commentRepliesRenderer": {"subThreads": [{"commentThreadRenderer": {"commentViewModel": {"commentViewModel": {"commentKey": "k2"}}}}]}}}},
                {"commentViewModel": {"commentKey": "k3"}},
                {"continuationItemRenderer": {"button": {"buttonRenderer": {"command": {"continuationCommand": {"token": "MORE"}}}}}}
            ]}}],
            "frameworkUpdates": {"entityBatchUpdate": {"mutations": [
                ent("k1", "P.a", "first", 2, "3 replies"),
                ent("k2", "P.b", "deeper", 3, "0 replies"),
                ent("k3", "P.c", "second", 2, "0 replies"),
                {"entityKey": "s1", "payload": {"engagementToolbarStateEntityPayload": {"heartState": "TOOLBAR_HEART_STATE_HEARTED", "likeState": "TOOLBAR_LIKE_STATE_INDIFFERENT"}}}
            ]}}
        });
        let p = parse(&v, "vid", "UCchan");
        assert_eq!(p.comments.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["P.a", "P.c"], "only the direct replies");
        let a = &p.comments[0];
        assert_eq!((a.text.as_str(), a.author.as_str(), a.like_count_text.as_deref(), a.reply_count), ("first", "@who", Some("12"), 3));
        assert!(a.is_hearted && !a.liked);
        assert_eq!(a.replies_continuation, Some(format!("{PREFIX}{}", reply_token("vid", "UCchan", "P.a"))));
        assert_eq!(p.comments[1].replies_continuation, None, "no replies, nothing to open");
        assert_eq!(p.next.as_deref(), Some("web:MORE"));
    }

    /// Live: a reply with replies on a real video opens them.
    #[test]
    #[ignore]
    fn live_replies_of_a_reply() {
        crate::paths::set_data_dir(std::env::temp_dir().join("ytnative-web-comments-test"));
        let mut s = crate::session::Session::new().unwrap();
        let w = s.watch("dQw4w9WgXcQ", None).unwrap();
        let p = s.comments(w.comments_continuation.as_deref().unwrap()).unwrap();
        let top = p.comments.iter().filter(|c| c.reply_count >= 20).max_by_key(|c| c.reply_count).unwrap();
        let mut tok = top.replies_continuation.clone();
        let mut opened = 0;
        while let (Some(t), true) = (tok.take(), opened == 0) {
            let r = s.comments(&t).unwrap();
            for c in r.comments.iter().filter(|c| c.reply_count > 0) {
                let rr = s.comments(c.replies_continuation.as_deref().unwrap()).unwrap();
                eprintln!("{} ({} replies): {} loaded, next={}", c.id, c.reply_count, rr.comments.len(), rr.next.is_some());
                assert!(!rr.comments.is_empty(), "replies of {} came back empty", c.id);
                if let Some(n) = &rr.next {
                    let more = s.comments(n).unwrap();
                    eprintln!("  next page: {} more", more.comments.len());
                    assert!(!more.comments.is_empty());
                }
                if let Some(deeper) = rr.comments.iter().find(|c| c.reply_count > 0) {
                    let d = s.comments(deeper.replies_continuation.as_deref().unwrap()).unwrap();
                    eprintln!("  deeper {} ({}): {} loaded", deeper.id, deeper.reply_count, d.comments.len());
                    assert!(!d.comments.is_empty());
                }
                opened += 1;
                if opened >= 3 {
                    break;
                }
            }
            tok = r.next;
        }
        assert!(opened > 0, "no reply with replies found");
    }
}

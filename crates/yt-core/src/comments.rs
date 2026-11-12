//! Comments and replies, the way the TV app loads them (and SmartTube does): the watch page's
//! comments engagement panel carries a continuation (`WatchInfo::comments_continuation`, or one of
//! `comment_sorts`); posting it to the TV `next` endpoint returns an `itemSectionContinuation` of
//! `commentThreadRenderer`s plus the next page's token. Each comment's `detailViewEndpoint` holds
//! the continuation of its replies, which come back in the same shape (preceded by the parent).

use crate::innertube::text;
use crate::session::Session;
use crate::watch::{first_token, largest};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub author_avatar: Option<String>,
    pub author_channel_id: Option<String>,
    pub text: String,
    pub published_text: String,
    pub like_count_text: Option<String>,
    pub reply_count: u32,
    /// Load replies with `Session::comments(token)`.
    pub replies_continuation: Option<String>,
    pub is_pinned: bool,
    pub is_hearted: bool,
    /// The signed-in user liked it.
    pub liked: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommentsPage {
    pub comments: Vec<Comment>,
    /// In a replies page: the comment being replied to.
    pub parent: Option<Comment>,
    pub next: Option<String>,
}

impl Session {
    /// A page of comments (or of replies) from a continuation token.
    pub fn comments(&mut self, continuation: &str) -> Result<CommentsPage> {
        Ok(parse_comments(&self.raw("next", json!({ "continuation": continuation }))?))
    }
}

pub fn parse_comments(resp: &Value) -> CommentsPage {
    let sec = &resp["continuationContents"]["itemSectionContinuation"];
    let mut page = CommentsPage::default();
    for (i, c) in sec["contents"].as_array().into_iter().flatten().enumerate() {
        if let Some(r) = c["commentThreadRenderer"]["comment"].get("commentRenderer") {
            if let Some(cm) = comment(r) {
                page.comments.push(cm);
            }
        } else if let Some(r) = c.get("commentRenderer") {
            // A bare comment leads a replies page: it is the parent.
            match comment(r) {
                Some(cm) if i == 0 && page.parent.is_none() => page.parent = Some(cm),
                Some(cm) => page.comments.push(cm),
                None => {}
            }
        }
    }
    page.next = sec["continuations"].as_array().and_then(|cs| cs.iter().find_map(crate::watch::token_of));
    page
}

/// A count as YouTube prints it: "12", "1,234", "1.2K", "3M replies".
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

fn comment(r: &Value) -> Option<Comment> {
    let id = r["commentId"].as_str()?.to_string();
    Some(Comment {
        id,
        author: text(&r["authorText"]).unwrap_or_default(),
        author_avatar: largest(&r["authorThumbnail"]["thumbnails"]),
        author_channel_id: r["authorEndpoint"]["browseEndpoint"]["browseId"].as_str().map(str::to_string),
        text: text(&r["contentText"]).unwrap_or_default(),
        published_text: text(&r["publishedTimeText"]).unwrap_or_default(),
        like_count_text: text(&r["voteCount"]).filter(|s| !s.is_empty()),
        reply_count: r["replyCount"].as_u64().map(|n| n as u32).or_else(|| text(&r["repliesCount"]).and_then(|s| count(&s))).unwrap_or(0),
        replies_continuation: first_token(&r["detailViewEndpoint"]),
        is_pinned: r.get("pinnedCommentBadge").is_some() || r.get("pinnedText").is_some() || r["isPinned"].as_bool() == Some(true),
        is_hearted: hearted(r),
        liked: r["isLiked"].as_bool() == Some(true) || r["voteStatus"].as_str() == Some("LIKE"),
    })
}

/// Creator heart: `creatorHeart.creatorHeartRenderer.isHearted` wherever the client put it.
fn hearted(r: &Value) -> bool {
    if r["isHearted"].as_bool() == Some(true) {
        return true;
    }
    let mut hs = Vec::new();
    collect(r, "creatorHeartRenderer", &mut hs);
    hs.iter().any(|h| h["isHearted"].as_bool() == Some(true))
}

fn collect<'a>(v: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if k == key {
                    out.push(x);
                } else if k != "detailViewEndpoint" {
                    collect(x, key, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect(x, key, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture(name: &str) -> Option<Value> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/private").join(name);
        serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
    }

    fn thread(id: &str, extra: Value) -> Value {
        let mut r = json!({
            "commentId": id, "authorText": {"simpleText": "@a"},
            "authorThumbnail": {"thumbnails": [{"url": "https://y/s48", "width": 48}, {"url": "https://y/s176", "width": 176}]},
            "contentText": {"runs": [{"text": "hello"}, {"text": "\n"}, {"text": "world"}]},
            "publishedTimeText": {"simpleText": "1 day ago"}, "voteCount": {"simpleText": "5"}, "replyCount": 2,
            "detailViewEndpoint": {"showEngagementPanelEndpoint": {"engagementPanel": {"engagementPanelSectionListRenderer": {"content": {"sectionListRenderer": {"contents": [{"itemSectionRenderer": {"continuations": [{"reloadContinuationData": {"continuation": format!("R-{id}")}}]}}]}}}}}}
        });
        for (k, v) in extra.as_object().unwrap() {
            r[k] = v.clone();
        }
        json!({ "commentThreadRenderer": { "comment": { "commentRenderer": r } } })
    }

    #[test]
    fn synthetic_page() {
        let v = json!({"continuationContents": {"itemSectionContinuation": {
            "contents": [
                {"commentRenderer": {"commentId": "P", "contentText": {"simpleText": "parent"}}},
                {"handoffInstructionRenderer": {}},
                thread("A", json!({"pinnedCommentBadge": {}})),
                thread("B", json!({"actionButtons": {"commentActionButtonsRenderer": {"creatorHeart": {"creatorHeartRenderer": {"isHearted": true}}}}}))
            ],
            "continuations": [{"nextContinuationData": {"continuation": "NEXT"}}]
        }}});
        let p = parse_comments(&v);
        assert_eq!(p.parent.as_ref().unwrap().text, "parent");
        assert_eq!(p.comments.len(), 2);
        let a = &p.comments[0];
        assert_eq!(a.text, "hello\nworld");
        assert_eq!(a.author_avatar.as_deref(), Some("https://y/s176"));
        assert_eq!(a.replies_continuation.as_deref(), Some("R-A"));
        assert_eq!(a.reply_count, 2);
        assert!(a.is_pinned && !a.is_hearted);
        assert!(p.comments[1].is_hearted && !p.comments[1].is_pinned);
        assert_eq!(p.next.as_deref(), Some("NEXT"));
    }

    #[test]
    fn private_comment_fixtures() {
        for name in ["comments_rick.json", "comments_fcc.json", "replies_rick.json"] {
            let Some(v) = fixture(name) else {
                eprintln!("skip {name} (no private fixture)");
                continue;
            };
            let p = parse_comments(&v);
            eprintln!("{name}: {} comments, parent={}, next={}", p.comments.len(), p.parent.is_some(), p.next.is_some());
            assert!(!p.comments.is_empty() && p.next.is_some());
            for c in &p.comments {
                assert!(!c.id.is_empty() && !c.author.is_empty() && !c.published_text.is_empty());
            }
            if name == "comments_fcc.json" {
                assert!(p.comments.iter().any(|c| c.is_pinned), "pinned comment");
            }
            if name != "replies_rick.json" {
                assert!(p.comments.iter().any(|c| c.replies_continuation.is_some() && c.reply_count > 0));
            } else {
                assert!(p.parent.is_some());
            }
        }
    }

    #[test]
    #[ignore]
    fn live_comments() {
        crate::paths::set_data_dir("C:/Download/prog/ytnative".into());
        let mut s = Session::new().unwrap();
        let w = s.watch("dQw4w9WgXcQ", None).unwrap();
        let p = s.comments(w.comments_continuation.as_deref().unwrap()).unwrap();
        eprintln!("page 1: {} comments; first: {:?}", p.comments.len(), p.comments.first().map(|c| (&c.author, &c.like_count_text, c.reply_count)));
        assert!(p.comments.len() >= 10);
        let p2 = s.comments(p.next.as_deref().unwrap()).unwrap();
        eprintln!("page 2: {} comments", p2.comments.len());
        assert!(!p2.comments.is_empty());
        let with_replies = p.comments.iter().find(|c| c.replies_continuation.is_some()).unwrap();
        let r = s.comments(with_replies.replies_continuation.as_deref().unwrap()).unwrap();
        eprintln!("replies to {}: {} (parent {:?})", with_replies.id, r.comments.len(), r.parent.as_ref().map(|c| &c.id));
        assert!(!r.comments.is_empty());
        assert_eq!(r.parent.as_ref().map(|c| c.id.as_str()), Some(with_replies.id.as_str()));
        if let Some(newest) = w.comment_sorts.iter().find(|s| !s.selected) {
            let n = s.comments(&newest.continuation).unwrap();
            eprintln!("{}: {} comments", newest.title, n.comments.len());
            assert!(!n.comments.is_empty());
        }
    }
}

#[cfg(test)]
mod count_tests {
    #[test]
    fn counts() {
        assert_eq!(super::count("12"), Some(12));
        assert_eq!(super::count("1,234"), Some(1234));
        assert_eq!(super::count("1.2K replies"), Some(1200));
        assert_eq!(super::count("3M"), Some(3_000_000));
        assert_eq!(super::count("replies"), None);
    }
}

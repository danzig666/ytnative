//! Return YouTube Dislike (https://returnyoutubedislike.com): estimated dislike counts.

use super::get_json;
use anyhow::Result;
use reqwest::blocking::Client;
use serde::Deserialize;

pub const VOTES_URL: &str = "https://returnyoutubedislikeapi.com/votes";

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Votes {
    #[serde(default)]
    pub likes: u64,
    #[serde(default)]
    pub dislikes: u64,
    /// 1..5 stars, derived from likes/dislikes.
    #[serde(default)]
    pub rating: f64,
    #[serde(default)]
    pub view_count: u64,
    #[serde(default)]
    pub deleted: bool,
}

impl Votes {
    /// Share of likes among votes (0..1), `None` without votes.
    pub fn like_ratio(&self) -> Option<f64> {
        let total = self.likes + self.dislikes;
        (total > 0).then(|| self.likes as f64 / total as f64)
    }
}

pub fn parse(json: &str) -> Result<Votes> {
    Ok(serde_json::from_str(json)?)
}

/// Votes for `video_id`; `Ok(None)` when the service does not know the video.
pub fn fetch(http: &Client, video_id: &str) -> Result<Option<Votes>> {
    get_json(http, VOTES_URL, &[("videoId", video_id)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let v = parse(
            r#"{"id":"abc","dateCreated":"2024-01-01T00:00:00Z","likes":900,"rawDislikes":10,"rawLikes":20,
                "dislikes":100,"rating":4.6,"viewCount":123456,"deleted":false}"#,
        )
        .unwrap();
        assert_eq!((v.likes, v.dislikes, v.view_count), (900, 100, 123456));
        assert_eq!(v.rating, 4.6);
        assert_eq!(v.like_ratio(), Some(0.9));
    }

    #[test]
    #[ignore = "hits returnyoutubedislikeapi.com"]
    fn live_fetch() {
        let v = fetch(&Client::new(), "dQw4w9WgXcQ").unwrap().expect("votes");
        println!("{v:?}");
        assert!(v.likes > 1_000_000 && v.dislikes > 0 && v.view_count > 1_000_000);
    }
}

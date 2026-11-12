//! Unlocalized (original-language) titles via YouTube's oEmbed endpoint. InnerTube returns
//! auto-translated titles for the account's language; oEmbed returns what the uploader typed.

use super::get_json;
use anyhow::Result;
use reqwest::blocking::Client;
use serde::Deserialize;

pub const OEMBED_URL: &str = "https://www.youtube.com/oembed";

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct OEmbed {
    pub title: String,
    #[serde(default)]
    pub author_name: String,
    #[serde(default)]
    pub author_url: String,
    #[serde(default)]
    pub thumbnail_url: String,
}

pub fn parse(json: &str) -> Result<OEmbed> {
    Ok(serde_json::from_str(json)?)
}

/// oEmbed data for `video_id`; `Ok(None)` for unknown videos. Private / embed-disabled videos
/// answer 401/403, also mapped to `None`.
pub fn fetch(http: &Client, video_id: &str) -> Result<Option<OEmbed>> {
    let url = format!("https://www.youtube.com/watch?v={video_id}");
    match get_json(http, OEMBED_URL, &[("url", url.as_str()), ("format", "json")]) {
        Err(e) if e.to_string().contains("HTTP 401") || e.to_string().contains("HTTP 403") => Ok(None),
        r => r,
    }
}

/// The original title of `video_id`.
pub fn original_title(http: &Client, video_id: &str) -> Result<Option<String>> {
    Ok(fetch(http, video_id)?.map(|o| o.title))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let o = parse(
            r#"{"title":"Título original","author_name":"Canal","author_url":"https://www.youtube.com/@canal",
                "type":"video","height":113,"width":200,"version":"1.0","provider_name":"YouTube",
                "thumbnail_url":"https://i.ytimg.com/vi/x/hqdefault.jpg","html":"<iframe></iframe>"}"#,
        )
        .unwrap();
        assert_eq!(o.title, "Título original");
        assert_eq!(o.author_name, "Canal");
    }

    #[test]
    #[ignore = "hits youtube.com/oembed"]
    fn live_fetch() {
        let t = original_title(&Client::new(), "dQw4w9WgXcQ").unwrap().expect("title");
        println!("{t}");
        assert!(t.contains("Never Gonna Give You Up"));
    }
}

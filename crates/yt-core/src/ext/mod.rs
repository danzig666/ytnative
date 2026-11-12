//! Third-party integrations: SponsorBlock, DeArrow, Return YouTube Dislike and unlocalized (oEmbed)
//! titles, plus a small in-memory TTL cache so screens can ask for the same video repeatedly.
//!
//! Every call is blocking and meant for a worker thread. Pass the `reqwest::blocking::Client` you
//! already have (e.g. `Session::http`).

pub mod cache;
pub mod dearrow;
pub mod oembed;
pub mod ryd;
pub mod sponsorblock;

pub use cache::TtlCache;

use anyhow::{bail, Result};
use reqwest::blocking::Client;
use serde::de::DeserializeOwned;
use std::time::Duration;

/// Per-request timeout for third-party services: these are nice-to-haves, never block the UI long.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const USER_AGENT: &str = concat!("ytnative/", env!("CARGO_PKG_VERSION"));

/// GET `url` with `query`, decode JSON. `Ok(None)` when the service answers 404 ("no data").
pub(crate) fn get_json<T: DeserializeOwned>(http: &Client, url: &str, query: &[(&str, &str)]) -> Result<Option<T>> {
    let resp = http.get(url).query(query).header("User-Agent", USER_AGENT).timeout(TIMEOUT).send()?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        bail!("{url}: HTTP {status}");
    }
    Ok(Some(resp.json()?))
}

/// Cached front door for the screens: one instance per app, shared across worker threads.
/// "No data" answers are cached too; errors are not (the next call retries).
pub struct Ext {
    pub http: Client,
    sponsor: TtlCache<String, Vec<sponsorblock::Segment>>,
    branding: TtlCache<String, Option<dearrow::Branding>>,
    votes: TtlCache<String, Option<ryd::Votes>>,
    titles: TtlCache<String, Option<String>>,
}

impl Ext {
    pub fn new(http: Client) -> Ext {
        let hour = Duration::from_secs(3600);
        Ext {
            http,
            sponsor: TtlCache::new(hour, 64),
            branding: TtlCache::new(hour, 1024),
            votes: TtlCache::new(Duration::from_secs(600), 256),
            titles: TtlCache::new(Duration::from_secs(6 * 3600), 1024),
        }
    }

    /// SponsorBlock segments for `categories` (the cache key includes the category set).
    pub fn segments(&self, video_id: &str, categories: &[sponsorblock::Category]) -> Result<Vec<sponsorblock::Segment>> {
        let key = format!("{video_id}|{}", categories.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(","));
        self.sponsor.get_or_try_insert_with(key, || sponsorblock::fetch(&self.http, video_id, categories))
    }

    pub fn branding(&self, video_id: &str) -> Result<Option<dearrow::Branding>> {
        self.branding.get_or_try_insert_with(video_id.to_string(), || dearrow::fetch(&self.http, video_id))
    }

    pub fn votes(&self, video_id: &str) -> Result<Option<ryd::Votes>> {
        self.votes.get_or_try_insert_with(video_id.to_string(), || ryd::fetch(&self.http, video_id))
    }

    pub fn original_title(&self, video_id: &str) -> Result<Option<String>> {
        self.titles.get_or_try_insert_with(video_id.to_string(), || oembed::original_title(&self.http, video_id))
    }
}

//! High-level, signed-in access to YouTube content: what the app's screens call.

use crate::auth::Token;
use crate::bootstrap::{self, TvConfig};
use crate::innertube::{Api, Client};
use crate::model::{Endpoint, Page};
use crate::parse;
use anyhow::Result;
use serde_json::{json, Value};

pub struct Session {
    pub http: reqwest::blocking::Client,
    pub cfg: TvConfig,
    pub(crate) token: Option<Token>,
    /// Account this session is pinned to (`Session::with_account`); None follows the current one.
    pub(crate) account: Option<String>,
    /// Brand-account page id, sent as `X-Goog-PageId`.
    pub(crate) page_id: Option<String>,
}

/// Well-known TV browse ids.
pub mod feeds {
    pub const HOME: &str = "FEwhat_to_watch";
    pub const SUBSCRIPTIONS: &str = "FEsubscriptions";
    pub const HISTORY: &str = "FEhistory";
    pub const LIBRARY: &str = "FEmy_youtube";
    pub const PLAYLISTS: &str = "FEplaylist_aggregation";
    pub const TRENDING: &str = "FEtrending";
    pub const SHORTS: &str = "FEshorts";
}

impl Session {
    pub fn new() -> Result<Session> {
        let http = crate::net::builder().timeout(std::time::Duration::from_secs(30)).build()?;
        let cfg = bootstrap::fetch(&http)?;
        let mut acct = crate::accounts::resolve(&http, None)?;
        // An account stored without its profile (a migrated token, or YouTube unreachable at
        // sign-in) gets its name and handle filled in now.
        if let Some(a) = acct.as_ref().filter(|a| a.name.trim().is_empty()) {
            match crate::accounts::fill_profile(&http, &cfg, &a.id) {
                Ok(filled) => acct = Some(filled),
                Err(e) => eprintln!("[accounts] could not fill in the profile: {e:#}"),
            }
        }
        let page_id = acct.as_ref().and_then(|a| a.page_id.clone());
        Ok(Session { http, cfg, token: acct.map(|a| a.token), account: None, page_id })
    }

    pub fn signed_in(&self) -> bool {
        self.token.is_some()
    }

    pub(crate) fn api(&self) -> Api<'_> {
        Api { http: &self.http, cfg: &self.cfg }
    }

    pub(crate) fn bearer(&mut self) -> Result<Option<String>> {
        // Refresh lazily: `accounts::resolve` refreshes when the token is about to expire.
        if self.token.is_some() {
            let a = crate::accounts::resolve(&self.http, self.account.as_deref())?;
            self.page_id = a.as_ref().and_then(|a| a.page_id.clone());
            self.token = a.map(|a| a.token);
        }
        Ok(self.token.as_ref().map(|t| t.access_token.clone()))
    }

    pub fn raw(&mut self, endpoint: &str, body: Value) -> Result<Value> {
        let bearer = self.bearer()?;
        let page = self.page_id.clone();
        let headers: Vec<(&str, &str)> = page.iter().map(|p| ("X-Goog-PageId", p.as_str())).collect();
        self.api().call_with(Client::Tv, endpoint, body, bearer.as_deref(), &headers)
    }

    pub fn browse(&mut self, browse_id: &str, params: Option<&str>) -> Result<Page> {
        let mut body = json!({ "browseId": browse_id });
        if let Some(p) = params {
            body["params"] = json!(p);
        }
        Ok(parse::page(&self.raw("browse", body)?))
    }

    pub fn home(&mut self) -> Result<Page> {
        self.browse(feeds::HOME, None)
    }

    pub fn subscriptions(&mut self) -> Result<Page> {
        self.browse(feeds::SUBSCRIPTIONS, None)
    }

    /// The Shorts section (SmartTube's): the Shorts of the subscribed channels (signed in) and of
    /// Home, as one shelf. The TV client has no Shorts page of its own (`FEshorts` is refused).
    pub fn shorts(&mut self) -> Result<Page> {
        let mut pages = Vec::new();
        if self.signed_in() {
            pages.push(self.subscriptions());
        }
        pages.push(self.home());
        let mut items: Vec<crate::model::Item> = Vec::new();
        let mut first_err = None;
        for p in pages {
            match p {
                Ok(p) => {
                    for it in p.shelves.into_iter().flat_map(|s| s.items) {
                        if it.kind == crate::model::ItemKind::Short && !items.iter().any(|x| x.id == it.id) {
                            items.push(it);
                        }
                    }
                }
                Err(e) => first_err = first_err.or(Some(e)),
            }
        }
        if items.is_empty() {
            if let Some(e) = first_err {
                return Err(e);
            }
        }
        Ok(Page { shelves: vec![crate::model::Shelf { items, ..Default::default() }], ..Default::default() })
    }

    pub fn history(&mut self) -> Result<Page> {
        self.browse(feeds::HISTORY, None)
    }

    pub fn search(&mut self, query: &str, params: Option<&str>) -> Result<Page> {
        let mut body = json!({ "query": query });
        if let Some(p) = params {
            body["params"] = json!(p);
        }
        Ok(parse::page(&self.raw("search", body)?))
    }

    /// Next page of a feed/shelf from a continuation token.
    pub fn more(&mut self, endpoint: &str, continuation: &str) -> Result<Page> {
        Ok(parse::page(&self.raw(endpoint, json!({ "continuation": continuation }))?))
    }

    /// The next items of ONE shelf (horizontal continuation): `endpoint` is "browse" for feeds,
    /// "next" for the player's suggestions.
    pub fn shelf_more(&mut self, endpoint: &str, continuation: &str) -> Result<(Vec<crate::model::Item>, Option<String>)> {
        Ok(parse::shelf_continuation(&self.raw(endpoint, json!({ "continuation": continuation }))?))
    }

    /// The notifications inbox (`ctoken`: the page after that token). Signed out there is none:
    /// an empty page rather than an error, so a section left open after signing out stays quiet.
    ///
    /// YouTube now refuses the inbox (HTTP 400 "Precondition check failed"): the first page then
    /// comes from the RSS feeds of the channels in the local bell list (`bell_channels`), as
    /// SmartTube does. That page has no continuation; an empty list gives an empty page.
    pub fn notifications(&mut self, ctoken: Option<&str>) -> Result<Page> {
        if !self.signed_in() {
            return Ok(Page::default());
        }
        let inbox = self.raw(crate::notifications::ENDPOINT, crate::notifications::request(ctoken));
        match inbox {
            Ok(v) => Ok(crate::notifications::page(&v)),
            Err(e) if ctoken.is_some() => Err(e),
            Err(e) => {
                // Once per run: the inbox fails the same way every time.
                static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    eprintln!("[notifications] inbox unavailable, using the channels' RSS feeds: {e}");
                }
                crate::notifications::rss_page(&self.http, &crate::bell_channels::ids())
            }
        }
    }

    /// Opens whatever a card or link points at, for the browse/search kinds.
    pub fn open(&mut self, ep: &Endpoint) -> Result<Option<Page>> {
        Ok(match ep {
            Endpoint::Browse { browse_id, params } => Some(self.browse(browse_id, params.as_deref())?),
            Endpoint::Search { query, params } => Some(self.search(query, params.as_deref())?),
            Endpoint::Watch { .. } => None,
        })
    }

    /// Resolves playable streams (client ring) for a video.
    pub fn resolve(&mut self, video_id: &str) -> Result<crate::streams::Resolved> {
        let bearer = self.bearer()?;
        crate::streams::resolve(&self.api(), video_id, bearer.as_deref())
    }

    /// Search suggestions (the same endpoint SmartTube uses for the TV keyboard).
    pub fn suggestions(&self, prefix: &str) -> Result<Vec<String>> {
        let url = "https://suggestqueries-clients6.youtube.com/complete/search";
        // In the interface language and content country, as the searches themselves.
        let hl = crate::innertube::content_language().unwrap_or_else(|| "en".into());
        let gl = crate::innertube::content_country().unwrap_or_else(|| "US".into());
        let txt = self
            .http
            .get(url)
            // The answer in UTF-8: by default it comes in Latin-1 (accented queries came back as
            // broken letters).
            .query(&[("client", "youtube-lr"), ("ds", "yt"), ("hl", hl.as_str()), ("gl", gl.as_str()), ("ie", "utf-8"), ("oe", "utf-8"), ("q", prefix)])
            .send()?
            .text()?;
        // JSONP-ish: window.google.ac.h(["q",[["s1",0,[..]],...],...])
        let start = txt.find('[').unwrap_or(0);
        let end = txt.rfind(']').map(|e| e + 1).unwrap_or(txt.len());
        // `get` instead of slicing: an answer whose last ']' comes before its first '[' is not a
        // reason to panic.
        let v: Value = txt.get(start..end).and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null);
        Ok(v[1].as_array().map(|a| a.iter().filter_map(|s| s[0].as_str().map(str::to_string)).collect()).unwrap_or_default())
    }
}

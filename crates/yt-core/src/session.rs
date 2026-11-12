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
        let http = reqwest::blocking::Client::builder().gzip(true).timeout(std::time::Duration::from_secs(30)).build()?;
        let cfg = bootstrap::fetch(&http)?;
        let acct = crate::accounts::resolve(&http, None)?;
        let page_id = acct.as_ref().and_then(|a| a.page_id.clone());
        Ok(Session { http, cfg, token: acct.map(|a| a.token), account: None, page_id })
    }

    pub fn signed_in(&self) -> bool {
        self.token.is_some()
    }

    fn api(&self) -> Api<'_> {
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
        let txt = self
            .http
            .get(url)
            .query(&[("client", "youtube-lr"), ("ds", "yt"), ("hl", "en"), ("q", prefix)])
            .send()?
            .text()?;
        // JSONP-ish: window.google.ac.h(["q",[["s1",0,[..]],...],...])
        let start = txt.find('[').unwrap_or(0);
        let end = txt.rfind(']').map(|e| e + 1).unwrap_or(txt.len());
        let v: Value = serde_json::from_str(&txt[start..end]).unwrap_or(Value::Null);
        Ok(v[1].as_array().map(|a| a.iter().filter_map(|s| s[0].as_str().map(str::to_string)).collect()).unwrap_or_default())
    }
}

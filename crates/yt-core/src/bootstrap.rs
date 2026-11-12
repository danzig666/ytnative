//! Scrapes https://www.youtube.com/tv (the page the LG/Samsung YouTube app is built on) for the
//! values every later request needs: the TV OAuth client id/secret, the current TVHTML5 client
//! version, the InnerTube API key and a visitorData blob. One request bootstraps everything, so a
//! YouTube-side rotation of any of these is picked up automatically.

use anyhow::Result;
use regex::Regex;

pub const TV_UA: &str = "Mozilla/5.0 (ChromiumStylePlatform) Cobalt/25.lts.30.1034943-gold (unlike Gecko), Unknown_TV_Unknown_0/Unknown (Unknown, Unknown)";

// Fallbacks (values observed in Nov 2026) in case the page layout changes.
const FALLBACK_CLIENT_ID: &str = "861556708454-d6dlm3lh05idd8npek18k6be8ba3oc68.apps.googleusercontent.com";
const FALLBACK_CLIENT_SECRET: &str = "SboVhoG9s0rNafixCSGGKXAT";
const FALLBACK_TV_VERSION: &str = "7.20261110.12.00";

#[derive(Debug, Clone)]
pub struct TvConfig {
    pub client_id: String,
    pub client_secret: String,
    pub tv_client_version: String,
    pub visitor_data: Option<String>,
    pub scraped: bool,
}

pub fn fetch(http: &reqwest::blocking::Client) -> Result<TvConfig> {
    let html = http
        .get("https://www.youtube.com/tv")
        .header("User-Agent", TV_UA)
        .send()?
        .error_for_status()?
        .text()?;
    Ok(parse(&html))
}

pub fn parse(html: &str) -> TvConfig {
    let cap = |re: &str| -> Option<String> {
        Regex::new(re).ok()?.captures(html).map(|c| c[1].to_string())
    };
    let creds = Regex::new(r#"clientId:"([^"]+\.apps\.googleusercontent\.com)",\w+:"([^"]+)""#)
        .unwrap()
        .captures(html)
        .map(|c| (c[1].to_string(), c[2].to_string()));
    let scraped = creds.is_some();
    let (client_id, client_secret) = creds.unwrap_or_else(|| {
        (FALLBACK_CLIENT_ID.into(), FALLBACK_CLIENT_SECRET.into())
    });
    TvConfig {
        client_id,
        client_secret,
        // Can be e.g. "7.20261111.09.00-canary_experiment_7.20261110.12.00"; keep the first part.
        tv_client_version: cap(r#""clientName":"TVHTML5","clientVersion":"([0-9.]+)"#)
            .unwrap_or_else(|| FALLBACK_TV_VERSION.into()),
        visitor_data: cap(r#""visitorData":"([^"]+)""#),
        scraped,
    }
}

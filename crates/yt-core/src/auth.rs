//! "Sign in with a code" flow used by YouTube on TVs (youtube.com/activate), the same one SmartTube
//! uses. The TV shows a short code, the user enters it on a phone, the TV polls for a token.
//!
//! Non-blocking API for UIs: [`start_device_login`] once, then [`poll_device_login`] every
//! `DeviceCode::interval` seconds until it returns [`Poll::Done`]. [`device_login`] is the blocking
//! wrapper the CLI uses. Tokens are stored per account by [`crate::accounts`]; [`load`] and
//! [`save`] act on the current account.

use crate::bootstrap::TvConfig;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SCOPE: &str = "http://gdata.youtube.com https://www.googleapis.com/auth/youtube-paid-content";
const CODE_URL: &str = "https://www.youtube.com/o/oauth2/device/code";
const TOKEN_URL: &str = "https://www.youtube.com/o/oauth2/token";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    pub client_id: String,
    pub client_secret: String,
}

impl Token {
    /// True when the access token expires within a minute.
    pub fn needs_refresh(&self) -> bool {
        self.expires_at <= now() + 60
    }
}

/// A pending sign-in: show `user_code` and `verification_url` to the user.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DeviceCode {
    pub user_code: String,
    pub verification_url: String,
    /// Seconds to wait between polls (grows on `Poll::SlowDown`).
    pub interval: u64,
    /// Unix seconds after which the code is no longer valid.
    pub expires_at: u64,
    pub device_code: String,
}

impl DeviceCode {
    pub fn expired(&self) -> bool {
        now() >= self.expires_at
    }
}

#[derive(Debug, Clone)]
pub enum Poll {
    /// The user has not entered the code yet; poll again after `interval`.
    Pending,
    /// Polling too fast: wait longer (the caller should add ~2 s to its interval).
    SlowDown,
    /// Signed in. The token is NOT stored yet: pass it to `accounts::add` (or `save`).
    Done(Token),
    Expired,
    /// The user refused the request.
    Denied,
}

pub(crate) fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// Request body for `device/code`.
pub fn device_code_body(cfg: &TvConfig, device_id: &str) -> Value {
    json!({
        "client_id": cfg.client_id,
        "scope": SCOPE,
        "device_id": device_id,
        "device_model": "ytlr::",
    })
}

pub fn parse_device_code(resp: &Value) -> Result<DeviceCode> {
    let device_code = resp["device_code"].as_str().context(format!("device/code failed: {resp}"))?;
    Ok(DeviceCode {
        device_code: device_code.into(),
        user_code: resp["user_code"].as_str().unwrap_or("?").into(),
        verification_url: resp["verification_url"].as_str().unwrap_or("https://www.youtube.com/activate").into(),
        interval: resp["interval"].as_u64().unwrap_or(5),
        expires_at: now() + resp["expires_in"].as_u64().unwrap_or(1800),
    })
}

/// Asks YouTube for a sign-in code. Returns at once.
pub fn start_device_login(http: &reqwest::blocking::Client, cfg: &TvConfig) -> Result<DeviceCode> {
    let body = device_code_body(cfg, &uuid::Uuid::new_v4().to_string());
    let resp: Value = http.post(CODE_URL).json(&body).send()?.json()?;
    parse_device_code(&resp)
}

/// Interprets a `token` polling response.
pub fn parse_poll(r: &Value, cfg: &TvConfig) -> Result<Poll> {
    Ok(match r["error"].as_str() {
        None => Poll::Done(Token {
            access_token: r["access_token"].as_str().context("no access_token")?.into(),
            refresh_token: r["refresh_token"].as_str().context("no refresh_token")?.into(),
            expires_at: now() + r["expires_in"].as_u64().unwrap_or(3600),
            client_id: cfg.client_id.clone(),
            client_secret: cfg.client_secret.clone(),
        }),
        Some("authorization_pending") => Poll::Pending,
        Some("slow_down") => Poll::SlowDown,
        Some("expired_token") => Poll::Expired,
        Some("access_denied") => Poll::Denied,
        Some(e) => bail!("token polling failed: {e} ({r})"),
    })
}

/// One poll for the token. Does not sleep.
pub fn poll_device_login(http: &reqwest::blocking::Client, cfg: &TvConfig, code: &DeviceCode) -> Result<Poll> {
    if code.expired() {
        return Ok(Poll::Expired);
    }
    let r: Value = http
        .post(TOKEN_URL)
        .json(&json!({
            "client_id": cfg.client_id,
            "client_secret": cfg.client_secret,
            "code": code.device_code,
            "grant_type": "http://oauth.net/grant_type/device/1.0",
        }))
        .send()?
        .json()?;
    parse_poll(&r, cfg)
}

/// Blocking sign-in for the CLI: prints the code, polls, stores the new account as current.
pub fn device_login(http: &reqwest::blocking::Client, cfg: &TvConfig) -> Result<Token> {
    let mut code = start_device_login(http, cfg)?;
    println!("\n  Open {} on your phone and enter:\n\n      {}\n", code.verification_url, code.user_code);
    println!("  Waiting for approval (Ctrl+C to abort)...");
    loop {
        std::thread::sleep(Duration::from_secs(code.interval));
        match poll_device_login(http, cfg, &code)? {
            Poll::Pending => {}
            Poll::SlowDown => code.interval += 2,
            Poll::Expired => bail!("device code expired"),
            Poll::Denied => bail!("sign-in was denied"),
            Poll::Done(t) => {
                crate::accounts::add(http, cfg, t.clone())?;
                return Ok(t);
            }
        }
    }
}

/// The refresh token was revoked or has expired (`invalid_grant`): only a new sign-in helps.
#[derive(Debug)]
pub struct GrantRevoked;
impl std::fmt::Display for GrantRevoked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the sign-in was revoked or has expired (invalid_grant)")
    }
}
impl std::error::Error for GrantRevoked {}

/// Refreshes `t` in place when it is about to expire. Returns true if it changed.
pub fn refresh(http: &reqwest::blocking::Client, t: &mut Token) -> Result<bool> {
    if !t.needs_refresh() {
        return Ok(false);
    }
    let r: Value = http
        .post(TOKEN_URL)
        .json(&json!({
            "client_id": t.client_id,
            "client_secret": t.client_secret,
            "refresh_token": t.refresh_token,
            "grant_type": "refresh_token",
        }))
        .send()?
        .json()?;
    if r["error"].as_str() == Some("invalid_grant") {
        return Err(GrantRevoked.into());
    }
    t.access_token = r["access_token"].as_str().context(format!("refresh failed: {r}"))?.into();
    t.expires_at = now() + r["expires_in"].as_u64().unwrap_or(3600);
    Ok(true)
}

/// Stores `t` as the current account's token (adds an account if there is none with it).
pub fn save(t: &Token) -> Result<()> {
    crate::accounts::save_token(t)
}

/// Loads the current account's token, refreshing it if it expires within a minute.
pub fn load(http: &reqwest::blocking::Client) -> Result<Option<Token>> {
    Ok(crate::accounts::resolve(http, None)?.map(|a| a.token))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> TvConfig {
        TvConfig {
            client_id: "cid".into(),
            client_secret: "sec".into(),
            tv_client_version: "7.0".into(),
            visitor_data: None,
            scraped: false,
        }
    }

    #[test]
    fn device_code_roundtrip() {
        let b = device_code_body(&cfg(), "dev");
        assert_eq!(b["client_id"], "cid");
        assert_eq!(b["device_model"], "ytlr::");
        let c = parse_device_code(&json!({"device_code":"D","user_code":"ABC-DEF","verification_url":"https://yt/activate","interval":5,"expires_in":1800})).unwrap();
        assert_eq!(c.user_code, "ABC-DEF");
        assert_eq!(c.interval, 5);
        assert!(!c.expired());
        assert!(parse_device_code(&json!({"error":"invalid_client"})).is_err());
    }

    #[test]
    fn poll_states() {
        let c = cfg();
        assert!(matches!(parse_poll(&json!({"error":"authorization_pending"}), &c).unwrap(), Poll::Pending));
        assert!(matches!(parse_poll(&json!({"error":"slow_down"}), &c).unwrap(), Poll::SlowDown));
        assert!(matches!(parse_poll(&json!({"error":"expired_token"}), &c).unwrap(), Poll::Expired));
        assert!(matches!(parse_poll(&json!({"error":"access_denied"}), &c).unwrap(), Poll::Denied));
        assert!(parse_poll(&json!({"error":"weird"}), &c).is_err());
        match parse_poll(&json!({"access_token":"a","refresh_token":"r","expires_in":3600}), &c).unwrap() {
            Poll::Done(t) => {
                assert_eq!(t.client_id, "cid");
                assert!(!t.needs_refresh());
            }
            p => panic!("{p:?}"),
        }
    }

    /// Only asks for a code; never completes a login.
    #[test]
    #[ignore]
    fn live_start_device_login() {
        let http = reqwest::blocking::Client::new();
        let cfg = crate::bootstrap::fetch(&http).unwrap();
        let c = start_device_login(&http, &cfg).unwrap();
        assert!(c.user_code.len() >= 6, "{c:?}");
        assert!(c.verification_url.starts_with("https://"));
        assert!(matches!(poll_device_login(&http, &cfg, &c).unwrap(), Poll::Pending | Poll::SlowDown));
    }
}

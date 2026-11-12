//! "Sign in with a code" flow used by YouTube on TVs (youtube.com/activate), the same one SmartTube
//! uses. The TV shows a short code, the user enters it on a phone, the TV polls for a token.

use crate::bootstrap::TvConfig;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn token_file() -> std::path::PathBuf {
    crate::paths::data_file("token.json")
}
const SCOPE: &str = "http://gdata.youtube.com https://www.googleapis.com/auth/youtube-paid-content";

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    pub client_id: String,
    pub client_secret: String,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

pub fn device_login(http: &reqwest::blocking::Client, cfg: &TvConfig) -> Result<Token> {
    let resp: Value = http
        .post("https://www.youtube.com/o/oauth2/device/code")
        .json(&json!({
            "client_id": cfg.client_id,
            "scope": SCOPE,
            "device_id": uuid::Uuid::new_v4().to_string(),
            "device_model": "ytlr::",
        }))
        .send()?
        .json()?;
    let device_code = resp["device_code"].as_str().context(format!("device/code failed: {resp}"))?;
    let user_code = resp["user_code"].as_str().unwrap_or("?");
    let url = resp["verification_url"].as_str().unwrap_or("https://www.youtube.com/activate");
    let mut interval = resp["interval"].as_u64().unwrap_or(5);
    let expires = now() + resp["expires_in"].as_u64().unwrap_or(1800);

    println!("\n  Open {url} on your phone and enter:\n\n      {user_code}\n");
    println!("  Waiting for approval (Ctrl+C to abort)...");

    loop {
        if now() > expires {
            bail!("device code expired");
        }
        std::thread::sleep(Duration::from_secs(interval));
        let r: Value = http
            .post("https://www.youtube.com/o/oauth2/token")
            .json(&json!({
                "client_id": cfg.client_id,
                "client_secret": cfg.client_secret,
                "code": device_code,
                "grant_type": "http://oauth.net/grant_type/device/1.0",
            }))
            .send()?
            .json()?;
        match r["error"].as_str() {
            None => {
                let t = Token {
                    access_token: r["access_token"].as_str().context("no access_token")?.into(),
                    refresh_token: r["refresh_token"].as_str().context("no refresh_token")?.into(),
                    expires_at: now() + r["expires_in"].as_u64().unwrap_or(3600),
                    client_id: cfg.client_id.clone(),
                    client_secret: cfg.client_secret.clone(),
                };
                save(&t)?;
                return Ok(t);
            }
            Some("authorization_pending") => {}
            Some("slow_down") => interval += 2,
            Some(e) => bail!("token polling failed: {e} ({r})"),
        }
    }
}

pub fn save(t: &Token) -> Result<()> {
    std::fs::write(token_file(), serde_json::to_string_pretty(t)?)?;
    Ok(())
}

/// Loads the stored token, refreshing it if it expires within a minute.
pub fn load(http: &reqwest::blocking::Client) -> Result<Option<Token>> {
    let Ok(raw) = std::fs::read_to_string(token_file()) else { return Ok(None) };
    let mut t: Token = serde_json::from_str(&raw)?;
    if t.expires_at > now() + 60 {
        return Ok(Some(t));
    }
    let r: Value = http
        .post("https://www.youtube.com/o/oauth2/token")
        .json(&json!({
            "client_id": t.client_id,
            "client_secret": t.client_secret,
            "refresh_token": t.refresh_token,
            "grant_type": "refresh_token",
        }))
        .send()?
        .json()?;
    t.access_token = r["access_token"].as_str().context(format!("refresh failed: {r}"))?.into();
    t.expires_at = now() + r["expires_in"].as_u64().unwrap_or(3600);
    save(&t)?;
    Ok(Some(t))
}

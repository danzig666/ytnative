//! Sign-in worker: TV device-code flow ("go to youtube.com/activate, enter XXX-XXX-XXX").
//! Reports each stage to the screen through `post`; honours `cancel`.

use crate::host::SignIn;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SCOPE: &str = "http://gdata.youtube.com https://www.googleapis.com/auth/youtube-paid-content";

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn run(cancel: &AtomicBool, post: &dyn Fn(SignIn)) -> anyhow::Result<()> {
    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(20)).build()?;
    let cfg = yt_core::bootstrap::fetch(&http)?;
    let r: Value = http
        .post("https://www.youtube.com/o/oauth2/device/code")
        .json(&json!({
            "client_id": cfg.client_id,
            "scope": SCOPE,
            "device_id": uuid_v4(),
            "device_model": "ytlr::",
        }))
        .send()?
        .json()?;
    let device_code = r["device_code"].as_str().ok_or_else(|| anyhow::anyhow!("no device code: {r}"))?.to_string();
    let user_code = r["user_code"].as_str().unwrap_or("?").to_string();
    let url = r["verification_url"].as_str().unwrap_or("https://www.youtube.com/activate").to_string();
    let mut interval = r["interval"].as_u64().unwrap_or(5);
    let expires = now() + r["expires_in"].as_u64().unwrap_or(1800);
    post(SignIn::Code { user_code, url });

    loop {
        for _ in 0..interval * 4 {
            if cancel.load(Ordering::SeqCst) {
                post(SignIn::Idle);
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        if now() > expires {
            anyhow::bail!("the code expired, try again");
        }
        let t: Value = http
            .post("https://www.youtube.com/o/oauth2/token")
            .json(&json!({
                "client_id": cfg.client_id,
                "client_secret": cfg.client_secret,
                "code": device_code,
                "grant_type": "http://oauth.net/grant_type/device/1.0",
            }))
            .send()?
            .json()?;
        match t["error"].as_str() {
            None => {
                let token = yt_core::auth::Token {
                    access_token: t["access_token"].as_str().unwrap_or_default().into(),
                    refresh_token: t["refresh_token"].as_str().unwrap_or_default().into(),
                    expires_at: now() + t["expires_in"].as_u64().unwrap_or(3600),
                    client_id: cfg.client_id.clone(),
                    client_secret: cfg.client_secret.clone(),
                };
                yt_core::auth::save(&token)?;
                let name = account_name(&http, &cfg, &token.access_token).unwrap_or_else(|| "Signed in".into());
                post(SignIn::Done(name));
                return Ok(());
            }
            Some("authorization_pending") => {}
            Some("slow_down") => interval += 2,
            Some("access_denied") => anyhow::bail!("sign-in was declined"),
            Some(e) => anyhow::bail!("sign-in failed: {e}"),
        }
    }
}

fn account_name(http: &reqwest::blocking::Client, cfg: &yt_core::bootstrap::TvConfig, bearer: &str) -> Option<String> {
    let api = yt_core::innertube::Api { http, cfg };
    let r = api.call(yt_core::innertube::Client::Tv, "account/accounts_list", json!({}), Some(bearer)).ok()?;
    yt_core::innertube::find(&r, "accountName").and_then(yt_core::innertube::text)
}

fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    for (i, x) in b.iter_mut().enumerate() {
        *x = ((t >> ((i % 8) * 8)) as u8) ^ (i as u8).wrapping_mul(97) ^ (std::process::id() as u8);
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

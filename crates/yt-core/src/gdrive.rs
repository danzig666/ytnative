//! Optional Google Drive backup (like SmartTube's "Backup to Google Drive"): the backup bundle
//! from [`crate::backup`] is stored in the Drive *app data folder* (hidden from the user's Drive
//! UI, private to this OAuth client), signed in with Google's device-code flow (the TV shows a
//! code, the user enters it at google.com/device).
//!
//! ## Credentials the user must provide
//!
//! The YouTube TV OAuth client that [`crate::auth`] uses cannot be used here: Google rejects it
//! for Drive scopes (`invalid_scope` for `drive.appdata`, `restricted_client` for `drive.file`;
//! checked Nov 2026). SmartTube works around this by downloading its *own* Google Cloud project's
//! client id/secret from its GitHub releases; borrowing those is not an option. So Drive backup
//! needs an OAuth client from the user's own Google Cloud project:
//!
//! 1. console.cloud.google.com -> create a project -> "APIs & Services" -> enable **Google Drive API**.
//! 2. "OAuth consent screen": User type *External*; add your Google account as a *test user*
//!    (or publish the app; while in *Testing*, Google expires refresh tokens after 7 days, so
//!    you would have to sign in again weekly).
//! 3. "Credentials" -> "Create credentials" -> "OAuth client ID" -> Application type
//!    **TVs and Limited Input devices**. Copy the client id and client secret.
//! 4. Enter them in the app (stored with [`DriveConfig::set_client`] in `gdrive.json` in the data
//!    dir; that file is never part of a backup bundle).
//!
//! The scope requested is [`SCOPE`] (`drive.appdata`), which Google allows for the device flow.

use crate::auth::DeviceCode;
use anyhow::{bail, Context, Result};
use reqwest::blocking::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "gdrive.json";
pub const SCOPE: &str = "https://www.googleapis.com/auth/drive.appdata";
pub const DEVICE_CODE_URL: &str = "https://oauth2.googleapis.com/device/code";
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub const FILES_URL: &str = "https://www.googleapis.com/drive/v3/files";
pub const UPLOAD_URL: &str = "https://www.googleapis.com/upload/drive/v3/files";
pub const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// Name of the bundle in the app data folder.
pub const BACKUP_NAME: &str = "ytnative-backup.json";
const BOUNDARY: &str = "ytnative-drive-boundary-7f3a9c";

/// Client credentials (user-provided) and the Drive sign-in, persisted in `gdrive.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DriveConfig {
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub access_token: Option<String>,
    /// Unix seconds.
    #[serde(default)]
    pub expires_at: u64,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl DriveConfig {
    pub fn load() -> Self {
        Self::load_from(&crate::paths::data_file(FILE_NAME))
    }

    pub fn load_from(path: &Path) -> Self {
        let mut c: DriveConfig = crate::fsutil::load_json_or_default(path);
        c.path = Some(path.to_path_buf());
        c
    }

    pub fn save(&self) -> Result<()> {
        let path = self.path.clone().unwrap_or_else(|| crate::paths::data_file(FILE_NAME));
        crate::fsutil::save_json(&path, self)
    }

    /// Client id/secret have been provided.
    pub fn is_configured(&self) -> bool {
        !self.client_id.trim().is_empty() && !self.client_secret.trim().is_empty()
    }

    pub fn is_signed_in(&self) -> bool {
        self.refresh_token.is_some()
    }

    /// Sets the OAuth client; a different client invalidates the current sign-in.
    pub fn set_client(&mut self, client_id: &str, client_secret: &str) {
        let (id, secret) = (client_id.trim(), client_secret.trim());
        if id != self.client_id || secret != self.client_secret {
            self.sign_out();
        }
        self.client_id = id.to_string();
        self.client_secret = secret.to_string();
    }

    pub fn sign_out(&mut self) {
        self.refresh_token = None;
        self.access_token = None;
        self.expires_at = 0;
    }

    fn require_client(&self) -> Result<()> {
        if !self.is_configured() {
            bail!("Google Drive backup needs your own OAuth client id and secret (see gdrive module docs)");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Poll {
    Pending,
    /// Add ~5 s to the interval.
    SlowDown,
    /// Signed in; tokens are stored in the config (and saved by [`poll_sign_in`]).
    Done,
    Expired,
    Denied,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DriveFile {
    pub id: String,
    pub name: String,
    #[serde(default, rename = "modifiedTime")]
    pub modified_time: Option<String>,
    #[serde(default, deserialize_with = "size_de")]
    pub size: Option<u64>,
}

/// Drive sends `size` as a string.
fn size_de<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<u64>, D::Error> {
    let v = Option::<Value>::deserialize(d)?;
    Ok(v.and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok())))
}

// ---- request builders (pure; unit-tested with `.build()`) ------------------------------------

pub fn device_code_request(http: &Client, cfg: &DriveConfig) -> RequestBuilder {
    http.post(DEVICE_CODE_URL).form(&[("client_id", cfg.client_id.as_str()), ("scope", SCOPE)])
}

pub fn poll_request(http: &Client, cfg: &DriveConfig, device_code: &str) -> RequestBuilder {
    http.post(TOKEN_URL).form(&[
        ("client_id", cfg.client_id.as_str()),
        ("client_secret", cfg.client_secret.as_str()),
        ("device_code", device_code),
        ("grant_type", DEVICE_GRANT),
    ])
}

pub fn refresh_request(http: &Client, cfg: &DriveConfig, refresh_token: &str) -> RequestBuilder {
    http.post(TOKEN_URL).form(&[
        ("client_id", cfg.client_id.as_str()),
        ("client_secret", cfg.client_secret.as_str()),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ])
}

/// Lists files in the app data folder, optionally only those named `name`.
pub fn list_request(http: &Client, access_token: &str, name: Option<&str>) -> RequestBuilder {
    let mut q = vec![
        ("spaces", "appDataFolder".to_string()),
        ("fields", "files(id,name,modifiedTime,size)".to_string()),
        ("orderBy", "modifiedTime desc".to_string()),
        ("pageSize", "100".to_string()),
    ];
    if let Some(n) = name {
        q.push(("q", format!("name = '{}' and trashed = false", n.replace('\\', "\\\\").replace('\'', "\\'"))));
    }
    http.get(FILES_URL).bearer_auth(access_token).query(&q)
}

/// multipart/related body: JSON metadata part + content part.
pub fn multipart_body(metadata: &Value, content: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(content.len() + 512);
    b.extend_from_slice(format!("--{BOUNDARY}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n").as_bytes());
    b.extend_from_slice(metadata.to_string().as_bytes());
    b.extend_from_slice(format!("\r\n--{BOUNDARY}\r\nContent-Type: application/json\r\n\r\n").as_bytes());
    b.extend_from_slice(content);
    b.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    b
}

/// Creates `name` in the app data folder.
pub fn create_request(http: &Client, access_token: &str, name: &str, content: &[u8]) -> RequestBuilder {
    let meta = json!({"name": name, "parents": ["appDataFolder"], "mimeType": "application/json"});
    http.post(format!("{UPLOAD_URL}?uploadType=multipart&fields=id,name,modifiedTime,size"))
        .bearer_auth(access_token)
        .header("Content-Type", format!("multipart/related; boundary={BOUNDARY}"))
        .body(multipart_body(&meta, content))
}

/// Replaces the content of an existing file.
pub fn update_request(http: &Client, access_token: &str, file_id: &str, content: &[u8]) -> RequestBuilder {
    http.patch(format!("{UPLOAD_URL}/{file_id}?uploadType=media&fields=id,name,modifiedTime,size"))
        .bearer_auth(access_token)
        .header("Content-Type", "application/json")
        .body(content.to_vec())
}

pub fn download_request(http: &Client, access_token: &str, file_id: &str) -> RequestBuilder {
    http.get(format!("{FILES_URL}/{file_id}?alt=media")).bearer_auth(access_token)
}

pub fn delete_request(http: &Client, access_token: &str, file_id: &str) -> RequestBuilder {
    http.delete(format!("{FILES_URL}/{file_id}")).bearer_auth(access_token)
}

// ---- responses --------------------------------------------------------------------------------

/// Applies a token-endpoint response (device polling) to `cfg`.
pub fn apply_poll_response(cfg: &mut DriveConfig, r: &Value) -> Result<Poll> {
    Ok(match r["error"].as_str() {
        None => {
            let access = r["access_token"].as_str().context("no access_token")?;
            cfg.access_token = Some(access.to_string());
            if let Some(rt) = r["refresh_token"].as_str() {
                cfg.refresh_token = Some(rt.to_string());
            }
            cfg.expires_at = crate::fsutil::now_secs() + r["expires_in"].as_u64().unwrap_or(3600);
            Poll::Done
        }
        Some("authorization_pending") => Poll::Pending,
        Some("slow_down") => Poll::SlowDown,
        Some("expired_token") => Poll::Expired,
        Some("access_denied") => Poll::Denied,
        Some(e) => bail!("Google sign-in failed: {e} {}", r["error_description"].as_str().unwrap_or("")),
    })
}

// ---- network operations -----------------------------------------------------------------------

/// Asks Google for a code to show (verification URL is google.com/device).
pub fn start_sign_in(http: &Client, cfg: &DriveConfig) -> Result<DeviceCode> {
    cfg.require_client()?;
    let r: Value = device_code_request(http, cfg).send()?.json()?;
    if let Some(e) = r["error"].as_str() {
        bail!("Google refused the Drive sign-in request: {e}");
    }
    let mut c = crate::auth::parse_device_code(&r)?;
    if let Some(u) = r["verification_url"].as_str().or(r["verification_uri"].as_str()) {
        c.verification_url = u.to_string();
    }
    Ok(c)
}

/// One poll; on success the tokens are saved to `gdrive.json`.
pub fn poll_sign_in(http: &Client, cfg: &mut DriveConfig, code: &DeviceCode) -> Result<Poll> {
    if code.expired() {
        return Ok(Poll::Expired);
    }
    let r: Value = poll_request(http, cfg, &code.device_code).send()?.json()?;
    let p = apply_poll_response(cfg, &r)?;
    if p == Poll::Done {
        cfg.save()?;
    }
    Ok(p)
}

/// A valid access token, refreshing (and saving) when it expires within a minute.
pub fn access_token(http: &Client, cfg: &mut DriveConfig) -> Result<String> {
    cfg.require_client()?;
    if let Some(t) = cfg.access_token.as_ref().filter(|_| cfg.expires_at > crate::fsutil::now_secs() + 60) {
        return Ok(t.clone());
    }
    let rt = cfg.refresh_token.clone().context("not signed in to Google Drive")?;
    let r: Value = refresh_request(http, cfg, &rt).send()?.json()?;
    if let Some(e) = r["error"].as_str() {
        if e == "invalid_grant" {
            cfg.sign_out();
            cfg.save()?;
            bail!("the Google Drive sign-in expired or was revoked; sign in again");
        }
        bail!("Google token refresh failed: {e}");
    }
    let t = r["access_token"].as_str().context("no access_token in refresh")?.to_string();
    cfg.access_token = Some(t.clone());
    cfg.expires_at = crate::fsutil::now_secs() + r["expires_in"].as_u64().unwrap_or(3600);
    cfg.save()?;
    Ok(t)
}

fn checked(resp: reqwest::blocking::Response) -> Result<reqwest::blocking::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().unwrap_or_default();
    bail!("Google Drive error {status}: {}", body.chars().take(300).collect::<String>())
}

pub fn list_files(http: &Client, cfg: &mut DriveConfig, name: Option<&str>) -> Result<Vec<DriveFile>> {
    let t = access_token(http, cfg)?;
    let r: Value = checked(list_request(http, &t, name).send()?)?.json()?;
    Ok(serde_json::from_value(r["files"].clone()).unwrap_or_default())
}

/// Creates or replaces `name` in the app data folder; returns the file.
pub fn upload(http: &Client, cfg: &mut DriveConfig, name: &str, content: &[u8]) -> Result<DriveFile> {
    let existing = list_files(http, cfg, Some(name))?;
    let t = access_token(http, cfg)?;
    let req = match existing.first() {
        Some(f) => update_request(http, &t, &f.id, content),
        None => create_request(http, &t, name, content),
    };
    Ok(checked(req.send()?)?.json()?)
}

/// Content of `name`, or None when it does not exist.
pub fn download(http: &Client, cfg: &mut DriveConfig, name: &str) -> Result<Option<Vec<u8>>> {
    let Some(f) = list_files(http, cfg, Some(name))?.into_iter().next() else { return Ok(None) };
    let t = access_token(http, cfg)?;
    Ok(Some(checked(download_request(http, &t, &f.id).send()?)?.bytes()?.to_vec()))
}

pub fn delete(http: &Client, cfg: &mut DriveConfig, name: &str) -> Result<bool> {
    let files = list_files(http, cfg, Some(name))?;
    let t = access_token(http, cfg)?;
    for f in &files {
        checked(delete_request(http, &t, &f.id).send()?)?;
    }
    Ok(!files.is_empty())
}

/// Uploads a fresh bundle of the app data as [`BACKUP_NAME`].
pub fn backup(http: &Client, cfg: &mut DriveConfig, opts: crate::backup::BackupOptions) -> Result<DriveFile> {
    let b = crate::backup::create_bundle(opts)?;
    upload(http, cfg, BACKUP_NAME, &crate::backup::to_bytes(&b)?)
}

/// Downloads [`BACKUP_NAME`] and restores it (validated first).
pub fn restore(
    http: &Client,
    cfg: &mut DriveConfig,
    opts: crate::backup::RestoreOptions,
) -> Result<crate::backup::RestoreReport> {
    let bytes = download(http, cfg, BACKUP_NAME)?.context("no backup on Google Drive yet")?;
    crate::backup::restore(&crate::backup::from_bytes(&bytes)?, opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> DriveConfig {
        let mut c = DriveConfig::default();
        c.set_client("cid.apps.googleusercontent.com", "sec");
        c
    }

    fn body(r: &reqwest::blocking::Request) -> String {
        String::from_utf8(r.body().and_then(|b| b.as_bytes()).unwrap_or_default().to_vec()).unwrap()
    }

    #[test]
    fn oauth_requests() {
        let http = Client::new();
        let c = cfg();
        let r = device_code_request(&http, &c).build().unwrap();
        assert_eq!(r.url().as_str(), DEVICE_CODE_URL);
        assert_eq!(r.method(), "POST");
        assert_eq!(r.headers()["content-type"], "application/x-www-form-urlencoded");
        assert_eq!(
            body(&r),
            "client_id=cid.apps.googleusercontent.com&scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fdrive.appdata"
        );
        let r = poll_request(&http, &c, "DEV").build().unwrap();
        let b = body(&r);
        assert!(b.contains("device_code=DEV"));
        assert!(b.contains("client_secret=sec"));
        assert!(b.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"));
        let b = body(&refresh_request(&http, &c, "RT").build().unwrap());
        assert!(b.contains("refresh_token=RT") && b.contains("grant_type=refresh_token"));
    }

    #[test]
    fn drive_requests() {
        let http = Client::new();
        let r = list_request(&http, "AT", Some("it's.json")).build().unwrap();
        assert_eq!(r.headers()["authorization"], "Bearer AT");
        let pairs: Vec<(String, String)> = r.url().query_pairs().into_owned().collect();
        assert!(pairs.contains(&("spaces".into(), "appDataFolder".into())));
        assert!(pairs.contains(&("q".into(), "name = 'it\\'s.json' and trashed = false".into())));

        let r = create_request(&http, "AT", "b.json", b"{\"x\":1}").build().unwrap();
        assert_eq!(r.method(), "POST");
        assert!(r.url().as_str().starts_with("https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart"));
        assert_eq!(r.headers()["content-type"], format!("multipart/related; boundary={BOUNDARY}"));
        let b = body(&r);
        assert!(b.starts_with(&format!("--{BOUNDARY}\r\n")));
        assert!(b.contains("\"parents\":[\"appDataFolder\"]"));
        assert!(b.contains("\r\n\r\n{\"x\":1}\r\n"));
        assert!(b.ends_with(&format!("--{BOUNDARY}--\r\n")));

        let r = update_request(&http, "AT", "FID", b"{}").build().unwrap();
        assert_eq!(r.method(), "PATCH");
        assert!(r.url().path().ends_with("/files/FID"));
        assert_eq!(body(&r), "{}");
        let r = download_request(&http, "AT", "FID").build().unwrap();
        assert_eq!(r.url().query(), Some("alt=media"));
        assert_eq!(delete_request(&http, "AT", "FID").build().unwrap().method(), "DELETE");
    }

    #[test]
    fn poll_responses() {
        let mut c = cfg();
        assert_eq!(apply_poll_response(&mut c, &json!({"error":"authorization_pending"})).unwrap(), Poll::Pending);
        assert_eq!(apply_poll_response(&mut c, &json!({"error":"slow_down"})).unwrap(), Poll::SlowDown);
        assert_eq!(apply_poll_response(&mut c, &json!({"error":"access_denied"})).unwrap(), Poll::Denied);
        assert_eq!(apply_poll_response(&mut c, &json!({"error":"expired_token"})).unwrap(), Poll::Expired);
        assert!(apply_poll_response(&mut c, &json!({"error":"invalid_client"})).is_err());
        assert!(!c.is_signed_in());
        let p = apply_poll_response(&mut c, &json!({"access_token":"a","refresh_token":"r","expires_in":3599}));
        assert_eq!(p.unwrap(), Poll::Done);
        assert!(c.is_signed_in());
        assert!(c.expires_at > crate::fsutil::now_secs() + 3000);
        // Changing the client signs out; setting the same one does not.
        c.set_client(" cid.apps.googleusercontent.com ", "sec");
        assert!(c.is_signed_in());
        c.set_client("other", "sec");
        assert!(!c.is_signed_in());
    }

    #[test]
    fn config_persist_and_guard() {
        let d = crate::fsutil::temp_dir("gdrive");
        let p = d.join(FILE_NAME);
        let mut c = DriveConfig::load_from(&p);
        assert!(!c.is_configured());
        assert!(start_sign_in(&Client::new(), &c).is_err()); // no network call without a client
        c.set_client("id", "secret");
        c.save().unwrap();
        assert!(DriveConfig::load_from(&p).is_configured());
        let files: Vec<DriveFile> =
            serde_json::from_value(json!([{"id":"1","name":"a","size":"42"},{"id":"2","name":"b"}])).unwrap();
        assert_eq!(files[0].size, Some(42));
        assert_eq!(files[1].size, None);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Documents why Drive needs the user's own client: Google rejects the YouTube TV client for
    /// Drive scopes. Only asks for a device code; never signs in.
    #[test]
    #[ignore]
    fn live_tv_client_rejected_for_drive() {
        let http = Client::new();
        let tv = crate::bootstrap::fetch(&http).unwrap();
        let mut c = DriveConfig::default();
        c.set_client(&tv.client_id, &tv.client_secret);
        let err = start_sign_in(&http, &c).unwrap_err().to_string();
        assert!(err.contains("invalid_scope") || err.contains("restricted_client"), "{err}");
    }
}

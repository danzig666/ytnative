//! Several signed-in accounts, SmartTube style.
//!
//! `accounts.json` in the data dir holds every account (profile + OAuth token + optional local
//! password) and which one is current. A pre-existing single `token.json` is migrated into it the
//! first time it is read (the old file is left in place, untouched).
//!
//! One Google sign-in can own several identities (the personal channel plus brand accounts). They
//! share one OAuth token; a brand identity is addressed by sending its page id in the
//! `X-Goog-PageId` header. Each stored [`Account`] is one identity.

use crate::auth::{self, Token};
use crate::bootstrap::TvConfig;
use crate::innertube::{text, Api, Client};
use crate::session::Session;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Account {
    /// Stable id: the identity's obfuscated Gaia id (or a local id until the profile is known).
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub handle: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    /// Brand account: page id sent as `X-Goog-PageId`. None for the Google account's own channel.
    #[serde(default)]
    pub page_id: Option<String>,
    pub token: Token,
    /// Optional local lock ("account password"): `pbkdf2-sha256$<iters>$<salt hex>$<hash hex>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

impl Account {
    pub fn has_password(&self) -> bool {
        self.password.is_some()
    }

    pub fn check_password(&self, pw: &str) -> bool {
        self.password.as_deref().map_or(true, |h| verify_password(h, pw))
    }
}

/// The on-disk file.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Store {
    pub current: Option<String>,
    pub accounts: Vec<Account>,
}

/// One entry of `account/accounts_list`.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Identity {
    pub id: String,
    pub name: String,
    pub handle: Option<String>,
    pub email: Option<String>,
    pub avatar: Option<String>,
    pub page_id: Option<String>,
    pub selected: bool,
    pub has_channel: bool,
    pub disabled: bool,
}

fn store_file() -> PathBuf {
    crate::paths::data_file("accounts.json")
}

fn legacy_token_file() -> PathBuf {
    crate::paths::data_file("token.json")
}

fn local_id(t: &Token) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, t.refresh_token.as_bytes());
    format!("local-{}", hex(&d.as_ref()[..6]))
}

impl Store {
    /// Reads `accounts.json`, migrating `token.json` if there is no accounts file yet.
    pub fn load() -> Result<Store> {
        let path = store_file();
        if let Ok(raw) = std::fs::read_to_string(&path) {
            return serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()));
        }
        let mut s = Store::default();
        if let Ok(raw) = std::fs::read_to_string(legacy_token_file()) {
            let token: Token = serde_json::from_str(&raw).context("parsing token.json")?;
            s.upsert(Account {
                id: local_id(&token),
                name: String::new(),
                handle: None,
                email: None,
                avatar: None,
                page_id: None,
                token,
                password: None,
            });
            s.save()?;
        }
        Ok(s)
    }

    pub fn save(&self) -> Result<()> {
        let path = store_file();
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }

    fn get_mut(&mut self, id: &str) -> Option<&mut Account> {
        self.accounts.iter_mut().find(|a| a.id == id)
    }

    pub fn current(&self) -> Option<&Account> {
        self.current.as_deref().and_then(|id| self.get(id)).or(self.accounts.first())
    }

    /// Adds or replaces (by id) an account and makes it current. Keeps an existing password.
    pub fn upsert(&mut self, mut a: Account) {
        if let Some(old) = self.get_mut(&a.id) {
            if a.password.is_none() {
                a.password = old.password.take();
            }
            *old = a.clone();
        } else {
            self.accounts.push(a.clone());
        }
        self.current = Some(a.id);
    }

    /// Writes a refreshed token to every identity that shares its refresh token.
    pub fn update_token(&mut self, t: &Token) -> bool {
        let mut hit = false;
        for a in self.accounts.iter_mut().filter(|a| a.token.refresh_token == t.refresh_token) {
            a.token = t.clone();
            hit = true;
        }
        hit
    }

    pub fn switch(&mut self, id: &str) -> Result<()> {
        if self.get(id).is_none() {
            bail!("no account {id}");
        }
        self.current = Some(id.into());
        Ok(())
    }

    /// Removes an account (locally: the token is not revoked). The first remaining one becomes
    /// current if it was the current one.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.accounts.len();
        self.accounts.retain(|a| a.id != id);
        if self.current.as_deref() == Some(id) || self.current.as_deref().is_some_and(|c| self.get(c).is_none()) {
            self.current = self.accounts.first().map(|a| a.id.clone());
        }
        self.accounts.len() != before
    }
}

// ---------- public, file-backed API ----------

pub fn list() -> Result<Vec<Account>> {
    Ok(Store::load()?.accounts)
}

pub fn current() -> Result<Option<Account>> {
    Ok(Store::load()?.current().cloned())
}

pub fn switch(id: &str) -> Result<()> {
    let mut s = Store::load()?;
    s.switch(id)?;
    s.save()
}

pub fn remove(id: &str) -> Result<bool> {
    let mut s = Store::load()?;
    let r = s.remove(id);
    s.save()?;
    Ok(r)
}

/// Sets (Some) or clears (None) the local password of an account.
pub fn set_password(id: &str, password: Option<&str>) -> Result<()> {
    let mut s = Store::load()?;
    let a = s.get_mut(id).context("no such account")?;
    a.password = password.map(hash_password);
    s.save()
}

/// Account `id` (or the current one) with a token refreshed if needed (and persisted).
pub fn resolve(http: &reqwest::blocking::Client, id: Option<&str>) -> Result<Option<Account>> {
    let mut s = Store::load()?;
    let Some(mut a) = (match id {
        Some(id) => s.get(id).cloned(),
        None => s.current().cloned(),
    }) else {
        return Ok(None);
    };
    if auth::refresh(http, &mut a.token)? {
        s.update_token(&a.token);
        s.save()?;
    }
    Ok(Some(a))
}

/// Legacy `auth::save`: updates the identities using this token, or adds it as a new account.
pub fn save_token(t: &Token) -> Result<()> {
    let mut s = Store::load()?;
    if !s.update_token(t) {
        s.upsert(Account {
            id: local_id(t),
            name: String::new(),
            handle: None,
            email: None,
            avatar: None,
            page_id: None,
            token: t.clone(),
            password: None,
        });
    }
    s.save()
}

pub fn accounts_list_body() -> Value {
    json!({ "accountReadMask": { "returnOwner": true, "returnBrandAccounts": true, "returnPersonaAccounts": false } })
}

/// Parses an `account/accounts_list` response.
pub fn parse_accounts_list(r: &Value) -> Vec<Identity> {
    let mut items = Vec::new();
    collect(r, "accountItem", &mut items);
    items
        .into_iter()
        .map(|a| {
            let tokens = a["serviceEndpoint"]["selectActiveIdentityEndpoint"]["supportedTokens"].as_array();
            let tok = |k: &str, f: &str| {
                tokens.and_then(|ts| ts.iter().find_map(|t| t[k][f].as_str())).map(str::to_string)
            };
            let page_id = tok("pageIdToken", "pageId");
            let gaia = tok("accountStateToken", "obfuscatedGaiaId");
            let datasync = tok("datasyncIdToken", "datasyncIdToken")
                .map(|d| d.split("||").find(|p| !p.is_empty()).unwrap_or("").to_string())
                .filter(|d| !d.is_empty());
            let name = text(&a["accountName"]).unwrap_or_default();
            let handle = text(&a["channelHandle"]);
            let id = page_id
                .clone()
                .or(gaia)
                .or(datasync)
                .or(handle.clone())
                .unwrap_or_else(|| name.clone());
            Identity {
                id,
                name,
                handle,
                email: text(&a["accountByline"]).filter(|s| !s.is_empty()),
                avatar: a["accountPhoto"]["thumbnails"]
                    .as_array()
                    .and_then(|t| t.last())
                    .and_then(|t| t["url"].as_str())
                    .map(str::to_string),
                page_id,
                selected: a["isSelected"].as_bool().unwrap_or(false),
                has_channel: a["hasChannel"].as_bool().unwrap_or(false),
                disabled: a["isDisabled"].as_bool().unwrap_or(false),
            }
        })
        .collect()
}

fn collect<'a>(v: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(m) => {
            if let Some(x) = m.get(key) {
                out.push(x);
            }
            m.values().for_each(|c| collect(c, key, out));
        }
        Value::Array(a) => a.iter().for_each(|c| collect(c, key, out)),
        _ => {}
    }
}

/// Identities reachable with a token (the Google account and its brand accounts).
pub fn identities(http: &reqwest::blocking::Client, cfg: &TvConfig, token: &Token) -> Result<Vec<Identity>> {
    let api = Api { http, cfg };
    let r = api.call(Client::Tv, "account/accounts_list", accounts_list_body(), Some(&token.access_token))?;
    Ok(parse_accounts_list(&r))
}

fn account_from(i: &Identity, token: Token) -> Account {
    Account {
        id: i.id.clone(),
        name: i.name.clone(),
        handle: i.handle.clone(),
        email: i.email.clone(),
        avatar: i.avatar.clone(),
        page_id: i.page_id.clone(),
        token,
        password: None,
    }
}

/// Stores a freshly signed-in token as an account (the identity YouTube reports as selected) and
/// makes it current. Without a profile (network error) it is stored under a local id.
pub fn add(http: &reqwest::blocking::Client, cfg: &TvConfig, token: Token) -> Result<Account> {
    let ids = identities(http, cfg, &token).unwrap_or_default();
    let mut s = Store::load()?;
    // Drop a placeholder entry for the same token (e.g. a migrated token.json).
    let local = local_id(&token);
    let a = match ids.iter().find(|i| i.selected).or(ids.first()) {
        Some(i) => {
            s.remove(&local);
            account_from(i, token)
        }
        None => Account {
            id: local,
            name: String::new(),
            handle: None,
            email: None,
            avatar: None,
            page_id: None,
            token,
            password: None,
        },
    };
    s.upsert(a.clone());
    s.save()?;
    Ok(a)
}

/// Adds another identity (e.g. a brand account) reachable with `base`'s token and makes it current.
pub fn add_identity(base_id: &str, identity: &Identity) -> Result<Account> {
    let mut s = Store::load()?;
    let token = s.get(base_id).context("no such account")?.token.clone();
    let a = account_from(identity, token);
    s.upsert(a.clone());
    s.save()?;
    Ok(a)
}

/// Fills in a placeholder account's profile (name/handle/avatar/id) from `accounts_list`.
pub fn fill_profile(http: &reqwest::blocking::Client, cfg: &TvConfig, id: &str) -> Result<Account> {
    let s = Store::load()?;
    let a = s.get(id).context("no such account")?.clone();
    let ids = identities(http, cfg, &a.token)?;
    let i = match &a.page_id {
        Some(p) => ids.iter().find(|i| i.page_id.as_deref() == Some(p)),
        None => ids.iter().find(|i| i.page_id.is_none() && i.selected).or(ids.iter().find(|i| i.page_id.is_none())),
    }
    .context("account not in accounts_list")?;
    let mut s = Store::load()?;
    let was_current = s.current().map(|c| c.id.clone()) == Some(a.id.clone());
    let mut new = account_from(i, a.token.clone());
    new.password = a.password.clone();
    if new.id != a.id {
        s.remove(&a.id);
    }
    let prev_current = s.current.clone();
    s.upsert(new.clone());
    if !was_current {
        s.current = prev_current;
    }
    s.save()?;
    Ok(new)
}

// ---------- password hashing ----------

const PBKDF2_ITERS: u32 = 20_000;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// Random bytes from the OS CSPRNG.
pub(crate) fn random_bytes<const N: usize>() -> [u8; N] {
    use ring::rand::SecureRandom;
    let mut b = [0u8; N];
    ring::rand::SystemRandom::new().fill(&mut b).expect("system RNG");
    b
}

pub fn hash_password(pw: &str) -> String {
    let salt: [u8; 16] = random_bytes();
    let mut out = [0u8; 32];
    let iters = std::num::NonZeroU32::new(PBKDF2_ITERS).unwrap();
    ring::pbkdf2::derive(ring::pbkdf2::PBKDF2_HMAC_SHA256, iters, &salt, pw.as_bytes(), &mut out);
    format!("pbkdf2-sha256${PBKDF2_ITERS}${}${}", hex(&salt), hex(&out))
}

pub fn verify_password(stored: &str, pw: &str) -> bool {
    let p: Vec<&str> = stored.split('$').collect();
    let [alg, iters, salt, hash] = p[..] else { return false };
    let (Some(iters), Some(salt), Some(hash)) =
        (iters.parse().ok().and_then(std::num::NonZeroU32::new), unhex(salt), unhex(hash))
    else {
        return false;
    };
    alg == "pbkdf2-sha256"
        && ring::pbkdf2::verify(ring::pbkdf2::PBKDF2_HMAC_SHA256, iters, &salt, pw.as_bytes(), &hash).is_ok()
}

// ---------- Session integration ----------

impl Session {
    /// A session pinned to account `id` (it keeps using it even if another becomes current).
    pub fn with_account(id: &str) -> Result<Session> {
        let mut s = Session::new()?;
        let a = resolve(&s.http, Some(id))?.with_context(|| format!("no account {id}"))?;
        s.account = Some(a.id);
        s.page_id = a.page_id;
        s.token = Some(a.token);
        Ok(s)
    }

    /// The account this session acts as.
    pub fn account(&self) -> Result<Option<Account>> {
        let s = Store::load()?;
        Ok(match &self.account {
            Some(id) => s.get(id).cloned(),
            None => s.current().cloned(),
        })
    }

    /// Makes `id` the current account and points this session at it.
    pub fn switch_account(&mut self, id: &str) -> Result<()> {
        switch(id)?;
        let a = resolve(&self.http, Some(id))?.context("account vanished")?;
        if self.account.is_some() {
            self.account = Some(a.id);
        }
        self.page_id = a.page_id;
        self.token = Some(a.token);
        Ok(())
    }

    /// Identities of this session's sign-in (`account/accounts_list`).
    pub fn identities(&mut self) -> Result<Vec<Identity>> {
        // Ask without a page id so the whole list comes back.
        let bearer = self.bearer()?.context("not signed in")?;
        let api = Api { http: &self.http, cfg: &self.cfg };
        let r = api.call(Client::Tv, "account/accounts_list", accounts_list_body(), Some(&bearer))?;
        Ok(parse_accounts_list(&r))
    }

    /// Stores a token from `auth::poll_device_login` as a new current account and switches to it.
    pub fn add_account(&mut self, token: Token) -> Result<Account> {
        let a = add(&self.http, &self.cfg, token)?;
        self.switch_account(&a.id)?;
        Ok(a)
    }

    /// Fills in the profile of the session's account if it is still a placeholder (migrated).
    pub fn ensure_profile(&mut self) -> Result<Option<Account>> {
        let Some(a) = self.account()? else { return Ok(None) };
        if !a.name.is_empty() {
            return Ok(Some(a));
        }
        let new = fill_profile(&self.http, &self.cfg, &a.id)?;
        if self.account.is_some() {
            self.account = Some(new.id.clone());
        }
        Ok(Some(new))
    }
}

/// Data dir for `#[ignore]` live tests: `YTNATIVE_TEST_DATA_DIR`, else the repo root (token.json).
#[cfg(test)]
pub(crate) fn live_data_dir() {
    let dir = std::env::var("YTNATIVE_TEST_DATA_DIR").unwrap_or_else(|_| "C:/Download/prog/ytnative".into());
    crate::paths::set_data_dir(dir.into());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(r: &str) -> Token {
        Token {
            access_token: "a".into(),
            refresh_token: r.into(),
            expires_at: u64::MAX / 2,
            client_id: "c".into(),
            client_secret: "s".into(),
        }
    }

    fn acct(id: &str, r: &str) -> Account {
        Account {
            id: id.into(),
            name: id.into(),
            handle: None,
            email: None,
            avatar: None,
            page_id: None,
            token: tok(r),
            password: None,
        }
    }

    #[test]
    fn store_ops() {
        let mut s = Store::default();
        assert!(s.current().is_none());
        s.upsert(acct("a", "r1"));
        s.upsert(acct("b", "r1"));
        s.upsert(acct("c", "r2"));
        assert_eq!(s.current().unwrap().id, "c");
        s.switch("a").unwrap();
        assert!(s.switch("zz").is_err());
        assert_eq!(s.current().unwrap().id, "a");
        let mut t = tok("r1");
        t.access_token = "new".into();
        assert!(s.update_token(&t));
        assert_eq!(s.get("b").unwrap().token.access_token, "new");
        assert_eq!(s.get("c").unwrap().token.access_token, "a");
        // Re-adding keeps the password.
        s.get_mut("a").unwrap().password = Some(hash_password("1234"));
        s.upsert(acct("a", "r1"));
        assert!(s.get("a").unwrap().has_password());
        assert!(s.remove("a"));
        assert_eq!(s.current().unwrap().id, "b");
        assert!(!s.remove("a"));
        let js = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Store>(&js).unwrap(), s);
    }

    #[test]
    fn passwords() {
        let h = hash_password("hunter2");
        assert!(h.starts_with("pbkdf2-sha256$"));
        assert_ne!(h, hash_password("hunter2"), "salted");
        assert!(verify_password(&h, "hunter2"));
        assert!(!verify_password(&h, "hunter3"));
        assert!(!verify_password("garbage", "x"));
        let mut a = acct("a", "r");
        assert!(a.check_password("anything"));
        a.password = Some(h);
        assert!(!a.check_password("x"));
    }

    #[test]
    fn accounts_list_parse() {
        let r = json!({"contents":[{"accountSectionListRenderer":{"contents":[{"accountItemSectionRenderer":{"contents":[
            {"accountItem":{"accountName":{"simpleText":"Brand"},"channelHandle":{"simpleText":"@brand"},
              "accountPhoto":{"thumbnails":[{"url":"https://x/a.jpg"}]},"hasChannel":true,"isSelected":false,
              "serviceEndpoint":{"selectActiveIdentityEndpoint":{"supportedTokens":[
                {"pageIdToken":{"pageId":"111"}},{"accountStateToken":{"obfuscatedGaiaId":"111"}}]}}}},
            {"accountItem":{"accountName":{"simpleText":"Me"},"accountByline":{"simpleText":"me@example.com"},
              "channelHandle":{"simpleText":"@me"},"hasChannel":true,"isSelected":true,
              "serviceEndpoint":{"selectActiveIdentityEndpoint":{"supportedTokens":[
                {"accountStateToken":{"obfuscatedGaiaId":"222"}},{"datasyncIdToken":{"datasyncIdToken":"222||"}}]}}}}
        ]}}]}}]});
        let ids = parse_accounts_list(&r);
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0].id, "111");
        assert_eq!(ids[0].page_id.as_deref(), Some("111"));
        assert_eq!(ids[0].avatar.as_deref(), Some("https://x/a.jpg"));
        assert!(!ids[0].selected);
        assert_eq!(ids[1].id, "222");
        assert_eq!(ids[1].page_id, None);
        assert_eq!(ids[1].email.as_deref(), Some("me@example.com"));
        assert_eq!(ids[1].handle.as_deref(), Some("@me"));
        assert!(ids[1].selected);
    }

    #[test]
    fn private_fixture_accounts_list() {
        let Ok(raw) = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/private/accounts_list.json")) else { return };
        let ids = parse_accounts_list(&serde_json::from_str(&raw).unwrap());
        assert!(!ids.is_empty());
        assert_eq!(ids.iter().filter(|i| i.selected).count(), 1);
        assert!(ids.iter().all(|i| !i.id.is_empty() && !i.name.is_empty()));
    }

    /// Read-only: resolves the real account file (migrating token.json in the repo root if needed)
    /// and lists the identities. Run with --ignored.
    #[test]
    #[ignore]
    fn live_accounts() {
        crate::accounts::live_data_dir();
        let mut s = Session::new().unwrap();
        assert!(s.signed_in());
        let a = s.ensure_profile().unwrap().unwrap();
        assert!(!a.name.is_empty());
        let ids = s.identities().unwrap();
        assert!(ids.iter().any(|i| i.selected));
        eprintln!("{} accounts stored, {} identities", list().unwrap().len(), ids.len());
    }
}

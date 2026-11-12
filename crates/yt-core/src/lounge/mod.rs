//! "Link with TV code": the SCREEN side of YouTube's Lounge protocol (what SmartTube and the official TV
//! app do), so the YouTube app on a phone can pair with the TV and control playback.
//!
//! Flow:
//! 1. [`LoungeScreen::new`] loads or creates the screen identity (`screen_id` from
//!    `/api/lounge/pairing/generate_screen_id`, a random `device_id`), persisted in `lounge.json`.
//! 2. [`LoungeScreen::pairing_code`] returns the 12-digit code to show ("123 456 789 012"). The phone
//!    enters it under Settings -> "Watch on TV" -> "Link with TV code".
//! 3. [`LoungeScreen::run`] (on its own thread) binds the browserchannel session (`/api/lounge/bc/bind`,
//!    `device=LOUNGE_SCREEN`), long-polls it, turns incoming messages into [`Command`]s and posts the
//!    app's [`Event`]s back (`nowPlaying`, `onStateChange`, ...).
//!
//! The YouTube TV app does not show a QR code for this flow in SmartTube (the `qr=1` field on
//! `get_pairing_code` only returns the bare digits), so there is no deep link helper here.
//!
//! Optional: [`dial`] makes the TV appear in the phone's cast menu (UNTESTED on webOS).

mod chunk;
pub mod dial;
mod protocol;

pub use chunk::{ChunkParser, Message};
pub use protocol::{Command, Device, DpadKey, Event, NowPlaying, PlayerState, Playlist, Status};

use anyhow::{bail, Context, Result};
use protocol::{encode_batch, PlaylistContext};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BASE: &str = "https://www.youtube.com/api/lounge";
/// App id SmartTube registers with (the leanback/TV client).
const APP: &str = "lb-v4";
const ORIGIN: &str = "https://www.youtube.com";
/// The server may keep the long-poll open for minutes; we recycle it after this long (AID makes the
/// next request replay anything we missed) so a silently dead connection is noticed.
const LONG_POLL_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a fetched pairing code is reused before a new one is requested.
const PAIRING_CODE_TTL: Duration = Duration::from_secs(4 * 60);
/// Refresh the lounge token when it expires within this window (token lifespan is ~14 days).
const TOKEN_REFRESH_MARGIN_MS: u64 = 60 * 60 * 1000;
const MAX_BACKOFF: Duration = Duration::from_secs(60);

fn state_file() -> std::path::PathBuf {
    crate::paths::data_file("lounge.json")
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Persisted screen identity (`lounge.json`).
#[derive(Serialize, Deserialize, Default, Clone)]
struct Persisted {
    screen_id: String,
    device_id: String,
    #[serde(default)]
    lounge_token: String,
    #[serde(default)]
    token_expiration_ms: u64,
}

struct State {
    p: Persisted,
    code: Option<(String, Instant)>,
}

struct Inner {
    http: reqwest::blocking::Client,
    name: String,
    stop: Arc<AtomicBool>,
    /// Set to force a fresh bind (after a new pairing code, like SmartTube does).
    rebind: AtomicBool,
    state: Mutex<State>,
}

/// The TV as a lounge screen. Cheap to clone; clones share identity, cached code and stop flag, so the
/// UI thread can keep one for [`pairing_code`](Self::pairing_code) / [`stop`](Self::stop) while another
/// is moved into [`run`](Self::run).
#[derive(Clone)]
pub struct LoungeScreen {
    inner: Arc<Inner>,
}

/// Non-200 answer from a lounge endpoint.
#[derive(Debug)]
pub struct HttpStatus(pub u16);
impl std::fmt::Display for HttpStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "lounge HTTP {}", self.0)
    }
}
impl std::error::Error for HttpStatus {}

/// Channel session ids and counters.
#[derive(Debug, Clone)]
struct Session {
    sid: String,
    gsessionid: String,
    /// Index of the last message received (acknowledged with `AID`).
    aid: i64,
    /// Request id of the next POST.
    rid: u64,
    /// Number of messages we have sent in this session (`ofs`).
    ofs: u64,
}

impl LoungeScreen {
    /// Loads the screen identity from `lounge.json`, creating it (one network call) when missing.
    /// `device_name` is what phones show ("Living room TV").
    pub fn new(http: reqwest::blocking::Client, device_name: &str) -> Result<Self> {
        let mut p: Persisted = std::fs::read_to_string(state_file())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let mut dirty = false;
        if p.device_id.is_empty() {
            p.device_id = uuid::Uuid::new_v4().to_string();
            dirty = true;
        }
        if p.screen_id.is_empty() {
            p.screen_id = generate_screen_id(&http)?;
            p.lounge_token.clear();
            dirty = true;
        }
        if dirty {
            save(&p);
        }
        Ok(LoungeScreen {
            inner: Arc::new(Inner {
                http,
                name: device_name.to_string(),
                stop: Arc::new(AtomicBool::new(false)),
                rebind: AtomicBool::new(false),
                state: Mutex::new(State { p, code: None }),
            }),
        })
    }

    pub fn screen_id(&self) -> String {
        self.inner.state.lock().unwrap().p.screen_id.clone()
    }

    pub fn device_id(&self) -> String {
        self.inner.state.lock().unwrap().p.device_id.clone()
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// The flag [`run`](Self::run) watches; setting it makes `run` return within ~250 ms.
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        self.inner.stop.clone()
    }

    pub fn stop(&self) {
        self.inner.stop.store(true, Ordering::SeqCst);
    }

    /// The code to show on screen, formatted `"xxx xxx xxx xxx"`. Reuses the last code for a few minutes;
    /// call it again whenever the code is (re)displayed, e.g. every minute while the screen is open.
    pub fn pairing_code(&self) -> Result<String> {
        if let Some((code, at)) = &self.inner.state.lock().unwrap().code {
            if at.elapsed() < PAIRING_CODE_TTL {
                return Ok(code.clone());
            }
        }
        self.refresh_pairing_code()
    }

    /// Always requests a new pairing code.
    pub fn refresh_pairing_code(&self) -> Result<String> {
        let token = self.lounge_token()?;
        let (screen_id, device_id) = {
            let st = self.inner.state.lock().unwrap();
            (st.p.screen_id.clone(), st.p.device_id.clone())
        };
        let body = form(&[
            ("lounge_token", &token),
            ("screen_id", &screen_id),
            ("screen_name", &self.inner.name),
            ("access_type", "permanent"),
            ("app", APP),
            ("device_id", &device_id),
            ("qr", "1"),
        ]);
        let text = self.post_form(&format!("{BASE}/pairing/get_pairing_code?ctx=pair"), body)?;
        let code = parse_pairing_code(&text).with_context(|| format!("unexpected pairing code answer ({} bytes)", text.len()))?;
        let formatted = format_code(&code);
        self.inner.state.lock().unwrap().code = Some((formatted.clone(), Instant::now()));
        // SmartTube restarts its session after fetching a code so the new pairing is picked up.
        self.inner.rebind.store(true, Ordering::SeqCst);
        Ok(formatted)
    }

    /// Pairs a sender that generated its own code, e.g. the `pairingCode` of a DIAL launch request.
    pub fn register_pairing_code(&self, pairing_code: &str) -> Result<()> {
        let (screen_id, device_id) = {
            let st = self.inner.state.lock().unwrap();
            (st.p.screen_id.clone(), st.p.device_id.clone())
        };
        let body = form(&[
            ("access_type", "permanent"),
            ("app", APP),
            ("pairing_code", pairing_code),
            ("screen_id", &screen_id),
            ("screen_name", &self.inner.name),
            ("device_id", &device_id),
        ]);
        self.post_form(&format!("{BASE}/pairing/register_pairing_code"), body)?;
        self.inner.rebind.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Forgets the identity (new screen id on next start): unpairs every phone.
    pub fn reset_identity() {
        let _ = std::fs::remove_file(state_file());
    }

    /// Runs the lounge session until the stop flag is set or `events` is disconnected. Incoming commands go
    /// to `commands`; everything received on `events` is posted to the phones (batched).
    ///
    /// Reconnects with backoff on network errors, rebinds on `400/404` (session gone) and re-mints the lounge
    /// token on `401/403/410`. The long-poll runs on a helper thread that exits within one long-poll period
    /// after stop (it is blocked in a read until then).
    pub fn run(self, commands: Sender<Command>, events: Receiver<Event>) {
        self.inner.stop.store(false, Ordering::SeqCst);
        let shared: Arc<Mutex<Option<Session>>> = Arc::new(Mutex::new(None));
        let ctx: Arc<Mutex<PlaylistContext>> = Arc::default();
        {
            let me = self.clone();
            let shared = shared.clone();
            let ctx = ctx.clone();
            std::thread::Builder::new()
                .name("lounge-poll".into())
                .spawn(move || me.poll_loop(&shared, &ctx, &commands))
                .expect("spawn lounge-poll");
        }

        let mut pending: Vec<Event> = Vec::new();
        while !self.stopped() {
            match events.recv_timeout(Duration::from_millis(250)) {
                Ok(e) => {
                    pending.push(e);
                    while let Ok(e) = events.try_recv() {
                        pending.push(e);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if pending.is_empty() {
                continue;
            }
            let Some(sess) = shared.lock().unwrap().clone() else {
                // Not bound yet: keep the most recent updates (state is idempotent).
                let n = pending.len();
                if n > 32 {
                    pending.drain(..n - 32);
                }
                continue;
            };
            let batch: Vec<Event> = pending.drain(..pending.len().min(20)).collect();
            let fields = encode_batch(&batch, sess.ofs, &ctx.lock().unwrap());
            match self.post_events(&sess, fields) {
                Ok(()) => {
                    if let Some(s) = shared.lock().unwrap().as_mut() {
                        if s.sid == sess.sid {
                            s.rid += 1;
                            s.ofs += batch.len() as u64;
                        }
                    }
                }
                Err(e) => {
                    eprintln!("lounge: posting events failed: {e:#}");
                    if let Some(HttpStatus(400 | 404 | 401 | 403 | 410)) = e.downcast_ref::<HttpStatus>() {
                        self.inner.rebind.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
        self.stop();
    }

    fn stopped(&self) -> bool {
        self.inner.stop.load(Ordering::SeqCst)
    }

    /// Sleeps in small steps so stop is noticed; returns false when stopped.
    fn sleep(&self, d: Duration) -> bool {
        let end = Instant::now() + d;
        while Instant::now() < end {
            if self.stopped() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100).min(end - Instant::now()));
        }
        !self.stopped()
    }

    fn poll_loop(&self, shared: &Mutex<Option<Session>>, ctx: &Mutex<PlaylistContext>, tx: &Sender<Command>) {
        let mut failures: u32 = 0;
        let forward = |msgs: &[Message], sess: &mut Session| -> bool {
            for m in msgs {
                if m.index <= sess.aid {
                    continue; // replayed duplicate
                }
                sess.aid = m.index;
                if let Some(c) = Command::from_message(m) {
                    ctx.lock().unwrap().update(&c);
                    if tx.send(c).is_err() {
                        return false;
                    }
                }
            }
            true
        };

        while !self.stopped() {
            let need_bind = shared.lock().unwrap().is_none() || self.inner.rebind.swap(false, Ordering::SeqCst);
            if need_bind {
                *shared.lock().unwrap() = None;
                match self.bind() {
                    Ok((mut sess, msgs)) => {
                        failures = 0;
                        if !forward(&msgs, &mut sess) {
                            break;
                        }
                        *shared.lock().unwrap() = Some(sess);
                    }
                    Err(e) => {
                        eprintln!("lounge: bind failed: {e:#}");
                        self.on_http_error(&e);
                        failures += 1;
                        if !self.sleep(backoff(failures)) {
                            break;
                        }
                        continue;
                    }
                }
            }

            let Some(sess) = shared.lock().unwrap().clone() else { continue };
            let mut alive = true;
            let res = self.long_poll(&sess, LONG_POLL_TIMEOUT, |msgs| {
                let mut guard = shared.lock().unwrap();
                let Some(s) = guard.as_mut() else { return false };
                alive = forward(msgs, s);
                alive && !self.inner.rebind.load(Ordering::SeqCst) && !self.stopped()
            });
            if !alive {
                break;
            }
            match res {
                Ok(()) => failures = 0,
                Err(e) => {
                    eprintln!("lounge: long-poll failed: {e:#}");
                    if e.downcast_ref::<HttpStatus>().is_some() {
                        *shared.lock().unwrap() = None;
                    }
                    self.on_http_error(&e);
                    failures += 1;
                    if !self.sleep(backoff(failures)) {
                        break;
                    }
                }
            }
        }
        self.stop();
    }

    /// Drops the lounge token on auth-type failures so the next bind mints a new one.
    fn on_http_error(&self, e: &anyhow::Error) {
        if let Some(HttpStatus(401 | 403 | 410)) = e.downcast_ref::<HttpStatus>() {
            self.inner.state.lock().unwrap().p.lounge_token.clear();
        }
    }

    /// Returns a valid lounge token, minting/refreshing it via `get_lounge_token_batch` when needed.
    fn lounge_token(&self) -> Result<String> {
        let screen_id = {
            let st = self.inner.state.lock().unwrap();
            if !st.p.lounge_token.is_empty() && st.p.token_expiration_ms > now_ms() + TOKEN_REFRESH_MARGIN_MS {
                return Ok(st.p.lounge_token.clone());
            }
            st.p.screen_id.clone()
        };
        let text = self.post_form(&format!("{BASE}/pairing/get_lounge_token_batch"), form(&[("screen_ids", &screen_id)]))?;
        let v: Value = serde_json::from_str(&text).context("lounge token answer is not JSON")?;
        let s = &v["screens"][0];
        let token = s["loungeToken"].as_str().filter(|t| !t.is_empty()).context("no loungeToken in answer")?.to_string();
        let exp = match &s["expiration"] {
            Value::Number(n) => n.as_u64().unwrap_or(0),
            Value::String(x) => x.parse().unwrap_or(0),
            _ => 0,
        };
        let exp = if exp == 0 { now_ms() + 24 * 3600 * 1000 } else { exp };
        let mut st = self.inner.state.lock().unwrap();
        st.p.lounge_token = token.clone();
        st.p.token_expiration_ms = exp;
        save(&st.p);
        Ok(token)
    }

    /// Query parameters common to every bind request.
    fn common_params(&self, token: &str) -> Vec<(&'static str, String)> {
        let device_id = self.device_id();
        vec![
            ("device", "LOUNGE_SCREEN".into()),
            ("id", device_id),
            ("name", self.inner.name.clone()),
            ("app", APP.into()),
            ("theme", "cl".into()),
            ("capabilities", "dsp,mic,dpa".into()),
            ("mdx-version", "3".into()),
            ("loungeIdToken", token.into()),
            ("VER", "8".into()),
            ("v", "2".into()),
            ("CVER", "1".into()),
            ("t", "1".into()),
        ]
    }

    /// Initial bind: `POST` with `count=0`, answers the `c` (SID) and `S` (gsessionid) messages plus the
    /// current lounge state (`loungeStatus`, `getNowPlaying`, ...).
    fn bind(&self) -> Result<(Session, Vec<Message>)> {
        let token = self.lounge_token()?;
        let rid = 10_000 + (rand_u32() % 80_000) as u64;
        let mut q = self.common_params(&token);
        q.push(("RID", rid.to_string()));
        q.push(("zx", zx()));
        let url = format!("{BASE}/bc/bind?{}", query(&q));
        let text = self.post_form(&url, "count=0".into())?;
        let msgs = chunk::parse_all(text.as_bytes());
        let find = |name: &str| msgs.iter().find(|m| m.name == name).and_then(|m| m.args.first()?.as_str().map(String::from));
        let (Some(sid), Some(gsessionid)) = (find("c"), find("S")) else {
            bail!("bind answer has no SID/gsessionid ({} messages)", msgs.len());
        };
        let rest = msgs.into_iter().filter(|m| m.name != "c" && m.name != "S").collect();
        Ok((Session { sid, gsessionid, aid: -1, rid: rid + 1, ofs: 0 }, rest))
    }

    /// One long-poll `GET` (`RID=rpc`, `CI=0`, `TYPE=xmlhttp`). Calls `on_msgs` for every batch of messages as
    /// they stream in; stops early when it returns false. A read timeout or EOF ends the poll normally.
    fn long_poll(&self, sess: &Session, timeout: Duration, mut on_msgs: impl FnMut(&[Message]) -> bool) -> Result<()> {
        let token = self.lounge_token()?;
        let mut q = self.common_params(&token);
        q.extend([
            ("RID", "rpc".to_string()),
            ("SID", sess.sid.clone()),
            ("gsessionid", sess.gsessionid.clone()),
            ("CI", "0".into()),
            ("AID", sess.aid.to_string()),
            ("TYPE", "xmlhttp".into()),
            ("zx", zx()),
        ]);
        let url = format!("{BASE}/bc/bind?{}", query(&q));
        let mut resp = self.inner.http.get(url).header("Origin", ORIGIN).timeout(timeout).send()?;
        let code = resp.status().as_u16();
        if code != 200 {
            return Err(HttpStatus(code).into());
        }
        let mut parser = ChunkParser::new();
        let mut buf = [0u8; 8192];
        loop {
            match resp.read(&mut buf) {
                Ok(0) => return Ok(()),
                Ok(n) => {
                    let msgs = parser.feed(&buf[..n]);
                    if !msgs.is_empty() && !on_msgs(&msgs) {
                        return Ok(());
                    }
                }
                // Timeouts end the poll; the next one resumes from AID.
                Err(e) if is_timeout(&e) => return Ok(()),
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Outgoing `POST` with an encoded event batch.
    fn post_events(&self, sess: &Session, fields: Vec<(String, String)>) -> Result<()> {
        let token = self.lounge_token()?;
        let mut q = self.common_params(&token);
        q.extend([
            ("RID", sess.rid.to_string()),
            ("SID", sess.sid.clone()),
            ("gsessionid", sess.gsessionid.clone()),
            ("AID", sess.aid.to_string()),
            ("zx", zx()),
        ]);
        let url = format!("{BASE}/bc/bind?{}", query(&q));
        let pairs: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        self.post_form(&url, form(&pairs))?;
        Ok(())
    }

    fn post_form(&self, url: &str, body: String) -> Result<String> {
        let resp = self
            .inner
            .http
            .post(url)
            .header("Origin", ORIGIN)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()?;
        let code = resp.status().as_u16();
        if code != 200 {
            return Err(HttpStatus(code).into());
        }
        Ok(resp.text()?)
    }
}

fn generate_screen_id(http: &reqwest::blocking::Client) -> Result<String> {
    let resp = http.get(format!("{BASE}/pairing/generate_screen_id")).header("Origin", ORIGIN).send()?;
    let code = resp.status().as_u16();
    if code != 200 {
        return Err(HttpStatus(code).into());
    }
    let id = resp.text()?.trim().to_string();
    if id.is_empty() || id.len() > 128 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        bail!("unexpected generate_screen_id answer ({} bytes)", id.len());
    }
    Ok(id)
}

fn save(p: &Persisted) {
    if let Ok(s) = serde_json::to_string_pretty(p) {
        let _ = std::fs::write(state_file(), s);
    }
}

/// `get_pairing_code` answers the bare digits; older variants answered `{"code": "..."}`.
fn parse_pairing_code(text: &str) -> Option<String> {
    let t = text.trim();
    let raw = match serde_json::from_str::<Value>(t) {
        Ok(Value::Object(o)) => o.get("code")?.as_str()?.to_string(),
        _ => t.to_string(),
    };
    let digits: String = raw.chars().filter(|c| !c.is_whitespace() && *c != '-').collect();
    (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())).then_some(digits)
}

/// `"123456789012"` -> `"123 456 789 012"`.
fn format_code(code: &str) -> String {
    let chars: Vec<char> = code.chars().collect();
    chars.chunks(3).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join(" ")
}

fn backoff(failures: u32) -> Duration {
    // 1s, 2s, 4s, ... capped.
    Duration::from_secs(1u64 << failures.saturating_sub(1).min(6)).min(MAX_BACKOFF)
}

fn is_timeout(e: &std::io::Error) -> bool {
    if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) {
        return true;
    }
    // reqwest wraps its timeout error inside the io::Error.
    let mut src: Option<&(dyn std::error::Error + 'static)> = e.get_ref().map(|r| r as _);
    while let Some(s) = src {
        if let Some(re) = s.downcast_ref::<reqwest::Error>() {
            if re.is_timeout() {
                return true;
            }
        }
        if s.to_string().contains("timed out") {
            return true;
        }
        src = s.source();
    }
    false
}

fn rand_u32() -> u32 {
    let b = uuid::Uuid::new_v4().into_bytes();
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// 12 random lowercase letters (cache buster).
fn zx() -> String {
    uuid::Uuid::new_v4().into_bytes().iter().take(12).map(|b| (b'a' + b % 26) as char).collect()
}

/// Percent-encodes per `application/x-www-form-urlencoded` (space as `%20`, which servers accept too).
fn enc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")
}

fn query(pairs: &[(&'static str, String)]) -> String {
    pairs.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_code_formats() {
        assert_eq!(parse_pairing_code("123456789012").as_deref(), Some("123456789012"));
        assert_eq!(parse_pairing_code("{\"code\":\"123456789012\"}").as_deref(), Some("123456789012"));
        assert_eq!(parse_pairing_code("<html>").as_deref(), None);
        assert_eq!(format_code("123456789012"), "123 456 789 012");
    }

    #[test]
    fn encoding_and_backoff() {
        assert_eq!(form(&[("name", "Living room TV (ytnative)"), ("c", "dsp,mic")]), "name=Living%20room%20TV%20%28ytnative%29&c=dsp%2Cmic");
        assert_eq!(zx().len(), 12);
        assert!(zx().chars().all(|c| c.is_ascii_lowercase()));
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(3), Duration::from_secs(4));
        assert_eq!(backoff(50), MAX_BACKOFF);
    }

    /// Hits the real API (no phone needed): screen id, lounge token, pairing code, one bind, one event post,
    /// one short long-poll. `cargo test -p yt-core lounge::tests::live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_pairing_and_bind() {
        let dir = std::env::temp_dir().join("ytnative-lounge-live-test");
        let _ = std::fs::remove_dir_all(&dir);
        crate::paths::set_data_dir(dir);
        let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build().unwrap();
        let screen = LoungeScreen::new(http, "ytnative test").unwrap();
        assert!(!screen.screen_id().is_empty());
        assert!(state_file().exists());

        let code = screen.pairing_code().unwrap();
        assert_eq!(code.len(), 15, "code {code:?}");
        assert!(code.chars().all(|c| c.is_ascii_digit() || c == ' '));
        assert_eq!(screen.pairing_code().unwrap(), code, "cached");
        println!("pairing code fetched ({} digits)", code.replace(' ', "").len());

        let (mut sess, msgs) = screen.bind().unwrap();
        assert!(!sess.sid.is_empty() && !sess.gsessionid.is_empty());
        let names: Vec<_> = msgs.iter().map(|m| m.name.as_str()).collect();
        println!("bind ok, initial messages: {names:?}");
        sess.aid = msgs.iter().map(|m| m.index).max().unwrap_or(-1);

        let fields = encode_batch(&[Event::NowPlaying(None)], sess.ofs, &PlaylistContext::default());
        screen.post_events(&sess, fields).unwrap();
        println!("event post ok");

        let mut got = Vec::new();
        screen
            .long_poll(&sess, Duration::from_secs(5), |m| {
                got.extend(m.iter().map(|m| m.name.clone()));
                true
            })
            .unwrap();
        println!("long-poll ok, messages: {got:?}");

        // The full loop: binding forwards the server's initial `getNowPlaying`; answer it, then stop.
        let (ctx, crx) = std::sync::mpsc::channel();
        let (etx, erx) = std::sync::mpsc::channel();
        let runner = screen.clone();
        let t = std::thread::spawn(move || runner.run(ctx, erx));
        let first = crx.recv_timeout(Duration::from_secs(20)).unwrap();
        println!("run(): first command {first:?}");
        etx.send(Event::NowPlaying(None)).unwrap();
        std::thread::sleep(Duration::from_secs(2));
        screen.stop();
        t.join().unwrap();
        println!("run(): stopped");
    }
}

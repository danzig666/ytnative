//! The problem report: the app version, the TV and the tails of the app's logs, shown on the
//! Problem report screen and offered to a phone on the same network as a text file.
//!
//! The file is served by the app's HTTP server (the DIAL server on port 8090, or a listener of
//! its own while the cast menu entry is off) at `/problem-report/<token>`, with a token made
//! fresh each time the screen opens, and only while the screen is open: [`open`] publishes the
//! report, [`close`] takes it down. Someone else on the network cannot guess the address, and
//! nothing is served once the viewer has left the screen.
//!
//! The text is scrubbed before it is shown or served: stream addresses (they carry the viewer's
//! IP address and signatures), authorization values, OAuth tokens, cookies and e-mail addresses
//! are cut out here, and then plx's own pass ([`plx_base::eventlog::scrub::scrub_local`]) takes
//! the network addresses and the remaining credentials.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use yt_core::lounge::dial::{DialServer, TextPages};

/// Where the report is served (the token follows).
const PATH: &str = "/problem-report/";
/// The port the app's own listener tries first (the DIAL server's): a phone that scanned an
/// earlier code finds the same address.
const PORT: u16 = 8090;
/// At most this much of each log's end is read.
const TAIL_BYTES: u64 = 48 * 1024;
/// And at most this many of its last lines kept.
const TAIL_LINES: usize = 400;

/// The DIAL server's port once it runs with [`pages`] (0: not running).
static DIAL_PORT: AtomicU16 = AtomicU16::new(0);

/// What is being served right now.
struct Serving {
    token: String,
    text: Arc<String>,
    /// The listener started for the screen (the DIAL server was off); dropping it stops it.
    _own: Option<DialServer>,
}

static SERVING: Mutex<Option<Serving>> = Mutex::new(None);

/// The DIAL server is listening on `port` and answers with [`pages`].
pub fn dial_listening(port: u16) {
    DIAL_PORT.store(port, Ordering::SeqCst);
}

/// The report page for the app's HTTP server: answered only while the screen is open, only on
/// the exact path with the current token.
pub fn pages() -> TextPages {
    TextPages(Arc::new(|path: &str| {
        let serving = SERVING.lock().ok()?;
        let s = serving.as_ref()?;
        (path.strip_prefix(PATH) == Some(s.token.as_str())).then(|| s.text.as_str().to_owned())
    }))
}

/// What the screen shows.
pub struct Report {
    pub version: String,
    /// The TV's model, empty when unknown (the simulator, or a TV that does not say).
    pub model: String,
    /// The webOS version, empty when unknown.
    pub webos: String,
    /// The scrubbed log tails, one entry per line, each log after a line naming its file.
    pub lines: Vec<String>,
    /// Where a phone downloads the report, or why it cannot.
    pub link: Result<String, Unavailable>,
}

/// Why the report cannot be downloaded.
#[derive(Debug, Clone, PartialEq)]
pub enum Unavailable {
    /// The TV has no address on a network.
    NoAddress,
    /// No listener could be started (the error).
    NoServer(String),
}

/// The screen opened: read the logs, make a new token and start serving.
pub fn open() -> Report {
    let version = crate::update::current_version().to_string();
    let (model, webos) = device();
    let logs = read_logs();
    let lines: Vec<String> = logs.iter().flat_map(|(name, text)| std::iter::once(format!("--- {name} ---")).chain(text.lines().map(str::to_owned))).collect();
    let text = document(&version, &model, &webos, &logs);
    let token = token();
    let mut serving = SERVING.lock().unwrap_or_else(|e| e.into_inner());
    *serving = None;
    let port = match DIAL_PORT.load(Ordering::SeqCst) {
        0 => match yt_core::lounge::dial::serve_pages(PORT, pages()).or_else(|_| yt_core::lounge::dial::serve_pages(0, pages())) {
            Ok(server) => Ok((server.http_port(), Some(server))),
            Err(e) => Err(Unavailable::NoServer(format!("{e:#}"))),
        },
        port => Ok((port, None)),
    };
    let link = port.and_then(|(port, own)| {
        let ip = local_ip().ok_or(Unavailable::NoAddress)?;
        *serving = Some(Serving { token: token.clone(), text: Arc::new(text), _own: own });
        Ok(link(&ip.to_string(), port, &token))
    });
    crate::log(&format!("problem report: opened ({})", if link.is_ok() { "served" } else { "not served" }));
    Report { version, model, webos, lines, link }
}

/// The screen closed: nothing is served any more (and the screen's own listener stops).
pub fn close() {
    if let Ok(mut s) = SERVING.lock() {
        *s = None;
    }
}

/// The address a phone opens.
fn link(ip: &str, port: u16, token: &str) -> String {
    format!("http://{ip}:{port}{PATH}{token}")
}

/// The TV's model and webOS version, as the boot probe read them (empty when unknown).
fn device() -> (String, String) {
    let info = plx_platform::tv::device::info();
    let webos = if info.build.is_empty() { info.release.clone() } else { info.build.clone() };
    (plx_platform::tv::device::device().model.clone(), webos)
}

/// The logs worth reading, by the name the report gives them: the crash tracer's log, the app's
/// log, and plx's event, crash and standard error logs in the runtime folder.
fn log_files() -> Vec<(String, PathBuf)> {
    let mut paths: Vec<PathBuf> = Vec::new();
    #[cfg(not(windows))]
    paths.extend(["/tmp/ytnative/crash.log", "/tmp/ytnative/crash.log.1", "/tmp/ytnative/ytnative.log"].map(PathBuf::from));
    #[cfg(windows)]
    paths.push(crate::sim::base_dir().join("crash.log"));
    use plx_base::paths::{in_runtime_dir, runtime_file};
    paths.push(plx_base::eventlog::events_log());
    paths.push(in_runtime_dir(runtime_file::CRASH));
    paths.push(in_runtime_dir(runtime_file::STDERR));
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for p in paths {
        if out.iter().any(|(_, q)| *q == p) {
            continue;
        }
        // The file's name only: a full path on the simulator names the Windows user.
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        out.push((name, p));
    }
    out
}

/// Each log that exists and is not empty: its name and its scrubbed tail.
fn read_logs() -> Vec<(String, String)> {
    log_files().into_iter().filter_map(|(name, path)| Some((name, scrub(&tail(&read_end(&path)?, TAIL_LINES))))).filter(|(_, t)| !t.trim().is_empty()).collect()
}

/// The last [`TAIL_BYTES`] of a file (lossy UTF-8, the first partial line dropped).
fn read_end(path: &std::path::Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let from = len.saturating_sub(TAIL_BYTES);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = Vec::new();
    f.take(TAIL_BYTES).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    Some(if from > 0 { text.split_once('\n').map(|(_, rest)| rest.to_string()).unwrap_or_default() } else { text })
}

/// The last `n` lines of `s`.
fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// The text a phone downloads (for whoever reads the report, so in English).
fn document(version: &str, model: &str, webos: &str, logs: &[(String, String)]) -> String {
    let or_unknown = |s: &str| if s.is_empty() { "unknown".to_string() } else { s.to_string() };
    let mut out = format!("ytnative problem report\nApp version: {version}\nTV model: {}\nwebOS version: {}\n", or_unknown(model), or_unknown(webos));
    if logs.is_empty() {
        out.push_str("\nNo log files were found.\n");
    }
    for (name, text) in logs {
        out.push_str(&format!("\n--- {name} ---\n{text}\n"));
    }
    out
}

/// A fresh random token: 128 bits as hex.
fn token() -> String {
    let mut bytes = [0u8; 16];
    #[cfg(not(windows))]
    let from_os = {
        use std::io::Read;
        std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).is_ok()
    };
    #[cfg(windows)]
    let from_os = false;
    if !from_os {
        // The standard library's hasher keys come from the system's random source.
        use std::hash::{BuildHasher, Hasher};
        for (i, chunk) in bytes.chunks_mut(8).enumerate() {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_usize(i);
            h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
            chunk.copy_from_slice(&h.finish().to_le_bytes());
        }
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The TV's address on the local network: the one the system would send from towards the
/// internet (nothing is sent).
fn local_ip() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    s.connect(("8.8.8.8", 53)).ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

/// What is replaced by what.
const REMOVED_STREAM: &str = "<stream address removed>";
const REMOVED: &str = "<removed>";

/// Keys whose value is a secret wherever they appear (`key=value`, `"key": "value"`,
/// `Key: value`). Matched without regard to case.
const SECRET_KEYS: &[&str] = &["authorization", "access_token", "refresh_token", "id_token", "client_secret", "device_code", "set-cookie", "cookie", "sapisid", "bearer"];

/// The log text made safe to show and to hand to a phone.
pub fn scrub(text: &str) -> String {
    text.lines().map(scrub_line).collect::<Vec<_>>().join("\n")
}

fn scrub_line(line: &str) -> String {
    let s = cut_words(line, |w| w.contains("googlevideo.com") || w.contains("/videoplayback"), REMOVED_STREAM);
    let s = cut_values(&s);
    // Google's OAuth access ("ya29.") and refresh ("1//") tokens, wherever they turn up.
    let s = cut_words(&s, |w| w.contains("ya29.") || w.contains("1//0"), REMOVED);
    let s = cut_words(&s, is_email, REMOVED);
    plx_base::eventlog::scrub::scrub_local(&s)
}

/// Where a word (a run without spaces, quotes or brackets) begins and ends.
fn is_word_char(c: char) -> bool {
    !(c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | '`'))
}

/// Every word for which `hit` holds, replaced by `with`.
fn cut_words(line: &str, hit: impl Fn(&str) -> bool, with: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        out.push_str(if !word.is_empty() && hit(word) { with } else { word });
        word.clear();
    };
    for c in line.chars() {
        if is_word_char(c) {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

fn is_email(w: &str) -> bool {
    let w = w.trim_matches(|c: char| !c.is_alphanumeric());
    let Some((user, host)) = w.split_once('@') else { return false };
    !user.is_empty() && host.contains('.') && !host.starts_with('.') && host.chars().all(|c| c.is_alphanumeric() || matches!(c, '.' | '-'))
}

/// The value after every secret key ([`SECRET_KEYS`]) replaced. A key counts as one when it
/// stands as its own word and a separator follows (`:` or `=`, quotes and spaces around it; for
/// "bearer" a space). A quoted value ends at its quote; an unquoted header's ("Authorization:
/// Bearer ...", "Cookie: a=b; c=d") at the line's end, since half a credential is still one; an
/// unquoted parameter's at the next `&`, `;`, `,`, `}` or space.
fn cut_values(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    let mut out = String::with_capacity(line.len());
    let mut at = 0;
    let mut from = 0;
    while from < line.len() {
        let next = SECRET_KEYS.iter().filter_map(|k| lower[from..].find(k).map(|i| (from + i, *k))).min_by_key(|(i, k)| (*i, usize::MAX - k.len()));
        let Some((start, key)) = next else { break };
        let key_end = start + key.len();
        from = key_end;
        let own_word = !line[..start].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        let sep_len = line[key_end..].find(|c: char| !matches!(c, '"' | '\'' | ':' | '=' | ' ' | '\t')).unwrap_or(line.len() - key_end);
        let sep = &line[key_end..key_end + sep_len];
        let separated = sep.contains([':', '=']) || (key == "bearer" && sep.starts_with(' '));
        if !own_word || !separated || sep_len == 0 || start < at {
            continue;
        }
        let value_start = key_end + sep_len;
        let rest = &line[value_start..];
        let quote = sep.chars().rev().find(|c| matches!(c, '"' | '\''));
        let value_len = match quote {
            Some(q) => rest.find(q),
            None if matches!(key, "authorization" | "cookie" | "set-cookie" | "bearer") => None,
            None => rest.find(|c: char| matches!(c, '&' | ';' | ',' | '}') || c.is_whitespace()),
        }
        .unwrap_or(rest.len());
        if value_len == 0 {
            continue;
        }
        out.push_str(&line[at..value_start]);
        out.push_str(REMOVED);
        at = value_start + value_len;
        from = at;
    }
    out.push_str(&line[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_addresses_are_cut() {
        let s = scrub("player: open https://rr3---sn-abc.googlevideo.com/videoplayback?expire=1&ip=1.2.3.4&sig=XYZ (itag 22)");
        assert_eq!(s, format!("player: open {REMOVED_STREAM} (itag 22)"));
        assert!(!scrub("\"url\":\"https://r1.googlevideo.com/videoplayback?x=1\"").contains("googlevideo"));
    }

    #[test]
    fn authorization_values_are_cut() {
        let s = scrub("request Authorization: Bearer abc.def-123");
        assert!(!s.contains("abc.def"), "{s}");
        let s = scrub(r#"{"authorization":"Bearer secret","x":1}"#);
        assert!(!s.contains("secret"), "{s}");
        assert!(s.contains("\"x\":1"), "{s}");
    }

    #[test]
    fn oauth_tokens_and_cookies_are_cut() {
        let s = scrub(r#"token.json {"access_token":"ya29.a0AfH6","refresh_token":"1//0gXyz","expires_in":3599}"#);
        assert!(!s.contains("ya29") && !s.contains("1//0g"), "{s}");
        assert!(s.contains("3599"), "{s}");
        let s = scrub("request Cookie: SID=abc; HSID=def");
        assert!(!s.contains("SID=abc") && !s.contains("HSID=def"), "{s}");
        assert!(!scrub("seen ya29.loose_token here").contains("ya29"));
    }

    #[test]
    fn email_addresses_are_cut() {
        let s = scrub("signed in as someone@example.com now");
        assert!(!s.contains("someone@"), "{s}");
        assert!(scrub("bt: 0x91f0d0 libfoo.so+0x10").contains("libfoo.so+0x10"));
    }

    #[test]
    fn ordinary_lines_survive() {
        let line = "lounge: SetVideo { video_id: \"dQw4w9WgXcQ\" }";
        assert_eq!(cut_values(line), line);
        assert_eq!(cut_words(line, |w| w.contains("googlevideo.com"), REMOVED_STREAM), line);
        // A key name inside another word is not a key.
        assert_eq!(cut_values("bearerless cookies: none"), "bearerless cookies: none");
        assert_eq!(cut_values("GET /x?access_token=abc&v=1"), "GET /x?access_token=<removed>&v=1");
    }

    #[test]
    fn tails_and_document() {
        assert_eq!(tail("a\nb\nc\n", 2), "b\nc");
        assert_eq!(tail("a", 5), "a");
        let d = document("1.2.3", "", "6.5.3-47", &[("crash.log".into(), "*** SIGNAL 11".into())]);
        assert!(d.contains("App version: 1.2.3\nTV model: unknown\nwebOS version: 6.5.3-47\n"));
        assert!(d.contains("\n--- crash.log ---\n*** SIGNAL 11\n"));
        assert!(document("1", "", "", &[]).contains("No log files were found."));
    }

    #[test]
    fn tokens_are_random_and_served_only_with_the_token() {
        let (a, b) = (token(), token());
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert_eq!(link("192.168.1.5", 8090, &a), format!("http://192.168.1.5:8090/problem-report/{a}"));

        let page = pages();
        *SERVING.lock().unwrap() = Some(Serving { token: a.clone(), text: Arc::new("report".into()), _own: None });
        assert_eq!((page.0)(&format!("{PATH}{a}")).as_deref(), Some("report"));
        assert_eq!((page.0)(&format!("{PATH}{b}")), None);
        assert_eq!((page.0)(PATH), None);
        assert_eq!((page.0)("/problem-report"), None);
        close();
        assert_eq!((page.0)(&format!("{PATH}{a}")), None);
    }
}

//! Network trouble, told plainly: which kind of failure an error is (no connection, a slow or
//! broken one, a secure connection the TV's clock spoils, YouTube's servers failing), what to
//! say about it, and when a page that failed for it loads again on its own.
//!
//! The TV says when its internet connection comes back (`connectionmanager/getStatus`,
//! `isInternetConnectionAvailable`, open to public apps): every page that failed for the network
//! is loaded again at once. Without that word (the simulator, a missed event) it is tried again
//! every [`RETRY_MS`] while it is on screen.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// How often a page that failed for the network is tried again while it is shown.
pub const RETRY_MS: u32 = 20_000;

/// What kind of network failure an error is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// No connection at all: no address, no route, no name lookup.
    Offline,
    /// A connection that timed out or broke off.
    Unstable,
    /// The secure connection was refused: on a TV, nearly always a wrong date and time.
    Secure,
    /// YouTube's servers answered with an error of their own (HTTP 5xx).
    Server,
}

impl Problem {
    /// The headline.
    pub fn title(self) -> &'static str {
        match self {
            Problem::Offline => "No internet connection",
            Problem::Unstable => "The connection timed out",
            Problem::Secure => "Secure connection failed",
            Problem::Server => "YouTube is not responding",
        }
    }

    /// What to do about it.
    pub fn advice(self) -> &'static str {
        match self {
            Problem::Offline => "Check the TV's network connection.",
            Problem::Unstable => "The network is slow or unstable.",
            Problem::Secure => "Check that the TV's date and time are correct.",
            Problem::Server => "Try again in a few minutes.",
        }
    }

    /// A page that failed for it loads again on its own (a wrong clock does not mend itself).
    pub fn retries(self) -> bool {
        self != Problem::Secure
    }
}

/// The kind of network failure in an error's text (`{e:#}`: the whole chain), if it is one.
pub fn classify(err: &str) -> Option<Problem> {
    let e = err.to_ascii_lowercase();
    let has = |ks: &[&str]| ks.iter().any(|k| e.contains(k));
    if has(&["dns error", "failed to lookup address", "name resolution", "no such host", "name or service not known", "network is unreachable", "network unreachable", "no route to host", "host is unreachable", "connection refused", "actively refused", "no address associated"]) {
        return Some(Problem::Offline);
    }
    if has(&["certificate", "tls handshake", "invalid peer", "ssl"]) {
        return Some(Problem::Secure);
    }
    if has(&["timed out", "timeout", "connection reset", "connection closed", "broken pipe", "unexpected eof", "connection aborted", "error sending request", "error decoding response body"]) {
        return Some(Problem::Unstable);
    }
    // YouTube's own error pages: "browse -> HTTP 503 Service Unavailable: ..."
    let server = e.match_indices("http 5").any(|(i, _)| e[i + 6..].chars().take(2).all(|c| c.is_ascii_digit()) && e[i + 6..].len() >= 2);
    server.then_some(Problem::Server)
}

static ONLINE_GEN: AtomicU32 = AtomicU32::new(0);
static ONLINE: AtomicBool = AtomicBool::new(true);

/// Bumped each time the TV's internet connection comes back: a page that failed for the network
/// at an older number loads again.
pub fn online_gen() -> u32 {
    ONLINE_GEN.load(Ordering::Relaxed)
}

/// The TV's word on its connection (true where nobody says: the simulator).
#[allow(dead_code)]
pub fn online() -> bool {
    ONLINE.load(Ordering::Relaxed)
}

/// A change of the TV's connection: a return to online bumps [`online_gen`] and wakes the app.
#[cfg_attr(windows, allow(dead_code))]
fn set_online(now: bool) {
    let was = ONLINE.swap(now, Ordering::Relaxed);
    if now != was {
        crate::log(&format!("network: {}", if now { "back online" } else { "offline" }));
        if now {
            ONLINE_GEN.fetch_add(1, Ordering::Relaxed);
        }
        plx_machine::present::wake_from_worker();
    }
}

/// `isInternetConnectionAvailable` of a `getStatus` answer.
#[cfg_attr(windows, allow(dead_code))]
fn available(msg: &str) -> Option<bool> {
    serde_json::from_str::<serde_json::Value>(msg).ok()?["isInternetConnectionAvailable"].as_bool()
}

/// Watches the TV's connection (once, in the background).
#[cfg(not(windows))]
pub fn start() {
    plx_base::task::spawn("net", || {
        let luna = match crate::ls2::Luna::wide(plx_base::paths::app_id()) {
            Ok(l) => l,
            Err(e) => return crate::log(&format!("network: no bus: {e}")),
        };
        let ok = luna.subscribe("luna://com.webos.service.connectionmanager/getStatus", r#"{"subscribe":true}"#, |m| {
            if let Some(now) = available(m) {
                set_online(now);
            }
        });
        if !ok {
            crate::log("network: getStatus refused");
        }
    });
}

/// A feed page that failed for the network is due to load again: the connection came back since
/// (`failed_gen` is the [`online_gen`] it failed at), or [`RETRY_MS`] went by.
pub fn retry_due(error: Option<&str>, loading: bool, failed_at: u32, failed_gen: u32, now_ms: u32) -> bool {
    !loading
        && error.and_then(classify).is_some_and(Problem::retries)
        && (online_gen() != failed_gen || now_ms.wrapping_sub(failed_at) >= RETRY_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_failures_are_told_apart() {
        let c = |s: &str| classify(s);
        assert_eq!(c("browse -> error sending request for url (https://www.youtube.com/youtubei/v1/browse): client error (Connect): dns error: failed to lookup address information: Temporary failure in name resolution"), Some(Problem::Offline));
        assert_eq!(c("error sending request: client error (Connect): tcp connect error: Network is unreachable (os error 101)"), Some(Problem::Offline));
        assert_eq!(c("error sending request: client error (Connect): tcp connect error: Connection refused (os error 111)"), Some(Problem::Offline));
        assert_eq!(c("error sending request for url (https://x): operation timed out"), Some(Problem::Unstable));
        assert_eq!(c("error sending request: connection reset by peer"), Some(Problem::Unstable));
        assert_eq!(c("error sending request: client error (Connect): invalid peer certificate: Expired"), Some(Problem::Secure));
        assert_eq!(c("browse -> HTTP 503 Service Unavailable: backend"), Some(Problem::Server));
        assert_eq!(c("browse -> HTTP 400 Bad Request: \"Request contains an invalid argument.\""), None);
        assert_eq!(c("Sign in to confirm you're not a bot"), None);
        assert_eq!(c("This video is private"), None);
    }

    #[test]
    fn a_network_failure_loads_again_when_due() {
        let off = Some("dns error: failed to lookup address");
        let g = online_gen();
        assert!(!retry_due(off, false, 1_000, g, 1_000 + RETRY_MS - 1), "not yet");
        assert!(retry_due(off, false, 1_000, g, 1_000 + RETRY_MS), "after the wait");
        assert!(retry_due(off, false, 1_000, g.wrapping_sub(1), 1_001), "the connection came back since");
        assert!(!retry_due(off, true, 1_000, g, 1_000 + RETRY_MS), "already loading");
        assert!(!retry_due(Some("HTTP 400 Bad Request"), false, 1_000, g, 1_000 + RETRY_MS), "not a network failure");
        assert!(!retry_due(Some("invalid peer certificate"), false, 1_000, g, 1_000 + RETRY_MS), "a wrong clock does not mend itself");
    }

    #[test]
    fn the_tv_says_whether_it_is_online() {
        assert_eq!(available(r#"{"returnValue":true,"isInternetConnectionAvailable":false,"wired":{}}"#), Some(false));
        assert_eq!(available(r#"{"returnValue":true,"isInternetConnectionAvailable":true}"#), Some(true));
        assert_eq!(available(r#"{"returnValue":true}"#), None);
    }
}

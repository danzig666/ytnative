//! Live chat beside a live stream: the messages kept for the panel, the polling that brings new
//! ones, and the panel's scroll position.
//!
//! The chat is polled only while its panel is open. Each request is one worker job: it waits the
//! time YouTube asked for (`timeoutMs`), then makes the request; its answer, applied on the main
//! thread, starts the next job. Closing the panel moves the generation on, so a job still waiting
//! gives up without a request and an answer still on its way is dropped.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use yt_core::live_chat::{ChatMessage, LiveChatPage};

/// Messages kept: older ones are dropped from the top as new ones arrive.
pub const KEEP: usize = 200;
/// Wait between requests when YouTube names none, and the bounds on what it names.
const DEFAULT_WAIT_MS: u64 = 5000;
const MIN_WAIT_MS: u64 = 1000;
const MAX_WAIT_MS: u64 = 15000;
/// After a failed request: this much longer per failure in a row, up to the cap.
const RETRY_STEP_MS: u64 = 5000;
const RETRY_MAX_MS: u64 = 30000;

/// The live chat of the video playing, as the panel shows it.
#[derive(Default)]
pub struct Chat {
    pub video_id: String,
    /// Oldest first.
    pub messages: Vec<ChatMessage>,
    /// No answer yet since the panel opened on this video.
    pub loading: bool,
    /// The chat could not be loaded (shown only while there are no messages).
    pub error: Option<String>,
    /// YouTube sent no further continuation: the stream (and its chat) is over.
    pub ended: bool,
    /// The video's watch page offers no live chat (it is off for this stream).
    pub no_chat: bool,
    open: bool,
    /// The continuation to ask with next (kept to retry after a failure); empty: look up the
    /// first one on the watch page.
    token: String,
    failures: u32,
    /// The current generation, shared with the waiting job so it can see it was given up.
    gen: Arc<AtomicU64>,
}

/// One request to make on a worker: wait `delay_ms`, then ask with `token`.
pub struct Poll {
    pub gen: u64,
    current: Arc<AtomicU64>,
    video_id: String,
    token: String,
    delay_ms: u64,
}

impl Chat {
    /// The panel opened on `video_id`, with the first continuation when the player's watch page
    /// had one (else it is looked up first). The messages are kept when it is the same video (the
    /// panel was closed for a moment); polling starts again at once.
    pub fn open(&mut self, video_id: &str, token: Option<&str>) -> Poll {
        if self.video_id != video_id || self.ended || self.no_chat || self.messages.is_empty() {
            self.video_id = video_id.to_string();
            self.messages.clear();
            self.token = token.unwrap_or_default().to_string();
            self.ended = false;
            self.no_chat = false;
        }
        self.open = true;
        self.loading = self.messages.is_empty();
        self.error = None;
        self.failures = 0;
        self.next(0)
    }

    /// The panel closed: no more requests.
    pub fn close(&mut self) {
        self.open = false;
        self.gen.fetch_add(1, Ordering::Relaxed);
    }

    fn next(&mut self, delay_ms: u64) -> Poll {
        let gen = self.gen.fetch_add(1, Ordering::Relaxed) + 1;
        Poll { gen, current: self.gen.clone(), video_id: self.video_id.clone(), token: self.token.clone(), delay_ms }
    }

    /// A worker's answer for generation `gen` (`Ok(None)`: the video has no live chat): the
    /// messages go in, and the next request comes back to be started (None: the panel closed,
    /// the answer is old, or there is no more chat).
    pub fn apply(&mut self, gen: u64, page: Result<Option<LiveChatPage>, String>) -> Option<Poll> {
        if !self.open || gen != self.gen.load(Ordering::Relaxed) {
            return None;
        }
        self.loading = false;
        match page {
            Ok(None) => {
                self.no_chat = true;
                None
            }
            Ok(Some(p)) => {
                self.failures = 0;
                self.error = None;
                self.messages.retain(|m| !p.removed.contains(&m.id));
                // A reconnect can send messages again: keep each once.
                let recent: std::collections::HashSet<String> = self.messages.iter().rev().take(KEEP).map(|m| m.id.clone()).collect();
                self.messages.extend(p.messages.into_iter().filter(|m| m.id.is_empty() || !recent.contains(&m.id)));
                if self.messages.len() > KEEP {
                    self.messages.drain(..self.messages.len() - KEEP);
                }
                match p.continuation {
                    Some(t) => {
                        self.token = t;
                        Some(self.next(p.timeout_ms.unwrap_or(DEFAULT_WAIT_MS).clamp(MIN_WAIT_MS, MAX_WAIT_MS)))
                    }
                    None => {
                        self.ended = true;
                        None
                    }
                }
            }
            Err(e) => {
                self.failures += 1;
                if self.failures == 1 {
                    crate::log(&format!("live chat: {e}"));
                }
                self.error = Some("Couldn't load the live chat".into());
                Some(self.next((RETRY_STEP_MS * self.failures as u64).min(RETRY_MAX_MS)))
            }
        }
    }
}

/// A worker's part of a [`Poll`]: wait, then ask, unless the panel closed meanwhile (the first
/// time, the chat's first continuation is looked up on the web watch page). `Ok(None)`: the
/// video has no live chat. The session is held only to borrow its connection and configuration,
/// not across the requests.
pub fn run(session: &Mutex<Option<yt_core::session::Session>>, poll: Poll) -> Result<Option<LiveChatPage>, String> {
    let given_up = || poll.current.load(Ordering::Relaxed) != poll.gen;
    let until = std::time::Instant::now() + std::time::Duration::from_millis(poll.delay_ms);
    while std::time::Instant::now() < until {
        if given_up() {
            return Err("closed".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if given_up() {
        return Err("closed".into());
    }
    let (http, cfg) = crate::host::with_session(session, |s| Ok((s.http.clone(), s.cfg.clone()))).map_err(|e| format!("{e:#}"))?;
    let token = match poll.token {
        t if !t.is_empty() => t,
        _ => match yt_core::live_chat::first_continuation(&http, &poll.video_id).map_err(|e| format!("{e:#}"))? {
            Some(t) => t,
            None => return Ok(None),
        },
    };
    yt_core::live_chat::fetch(&http, &cfg, &token).map(Some).map_err(|e| format!("{e:#}"))
}

/// Where the panel's list is scrolled. It follows the newest message until the viewer scrolls
/// back; then it stays put, measured from the bottom of the message that was newest then, so new
/// messages (and old ones dropped from the top) do not move what the viewer is reading.
#[derive(Debug, Clone, PartialEq)]
pub struct Scroll {
    anchor: Option<String>,
    /// Distance from the anchor's bottom up to the view's bottom, in pixels.
    above: f32,
}

impl Default for Scroll {
    fn default() -> Self {
        Scroll { anchor: None, above: 0.0 }
    }
}

impl Scroll {
    /// At the newest message, following new ones.
    #[cfg(test)]
    pub fn following(&self) -> bool {
        self.anchor.is_none()
    }

    /// How far the view's bottom is above the bottom of the list (0: at the newest message).
    /// `bottom_of` gives a message's bottom from the list's top; `total` is the list's height.
    pub fn offset(&mut self, bottom_of: impl Fn(&str) -> Option<f32>, total: f32, view: f32) -> f32 {
        let Some(a) = &self.anchor else { return 0.0 };
        match bottom_of(a) {
            Some(b) => ((total - b) + self.above).clamp(0.0, (total - view).max(0.0)),
            // The anchor was removed or dropped: back to the newest.
            None => {
                self.anchor = None;
                0.0
            }
        }
    }

    /// Moves the view by `by` pixels (positive: back to older messages) from `offset`; reaching
    /// the bottom follows new messages again.
    pub fn scroll_by(&mut self, by: f32, offset: f32, max: f32, newest: Option<&str>) {
        let to = (offset + by).clamp(0.0, max.max(0.0));
        match newest {
            Some(id) if to > 0.5 => {
                self.anchor = Some(id.to_string());
                self.above = to;
            }
            _ => *self = Scroll::default(),
        }
    }

    /// Scrolled back while newer messages arrived below the view.
    pub fn newer_below(&self, newest: Option<&str>) -> bool {
        self.anchor.as_deref().is_some_and(|a| Some(a) != newest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yt_core::live_chat::ChatMessage;

    fn msg(id: &str) -> ChatMessage {
        ChatMessage { id: id.into(), author: "a".into(), text: id.into(), ..Default::default() }
    }

    fn page(ids: &[&str], next: Option<&str>, timeout: Option<u64>) -> LiveChatPage {
        LiveChatPage { messages: ids.iter().map(|i| msg(i)).collect(), removed: vec![], continuation: next.map(str::to_string), timeout_ms: timeout }
    }

    #[test]
    fn polls_while_open_and_waits_as_asked() {
        let mut c = Chat::default();
        let p = c.open("v", Some("T0"));
        assert!(c.loading);
        assert_eq!((p.token.as_str(), p.delay_ms), ("T0", 0));
        let p = c.apply(p.gen, Ok(Some(page(&["1", "2"], Some("T1"), Some(8000))))).unwrap();
        assert!(!c.loading);
        assert_eq!((p.token.as_str(), p.delay_ms), ("T1", 8000));
        // Waits out of bounds are brought in.
        let p = c.apply(p.gen, Ok(Some(page(&["3"], Some("T2"), Some(10))))).unwrap();
        assert_eq!(p.delay_ms, MIN_WAIT_MS);
        let p = c.apply(p.gen, Ok(Some(page(&[], Some("T3"), None)))).unwrap();
        assert_eq!(p.delay_ms, DEFAULT_WAIT_MS);
        assert_eq!(c.messages.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["1", "2", "3"]);
        // Closed: the waiting job sees it gave up, and its answer is dropped.
        c.close();
        assert_ne!(p.current.load(Ordering::Relaxed), p.gen);
        assert!(c.apply(p.gen, Ok(Some(page(&["4"], Some("T4"), None)))).is_none());
        assert_eq!(c.messages.len(), 3);
    }

    #[test]
    fn old_answers_are_dropped_after_reopening() {
        let mut c = Chat::default();
        let first = c.open("v", Some("T0"));
        c.close();
        let second = c.open("v", Some("T0"));
        assert!(c.apply(first.gen, Ok(Some(page(&["x"], Some("T1"), None)))).is_none());
        assert!(c.messages.is_empty());
        assert!(c.apply(second.gen, Ok(Some(page(&["y"], Some("T1"), None)))).is_some());
        // Reopened on the same video: the messages stay and polling goes on from the last token.
        c.close();
        let again = c.open("v", Some("IGNORED"));
        assert_eq!(again.token, "T1");
        assert_eq!(c.messages.len(), 1);
        // Another video starts over.
        c.close();
        let other = c.open("w", Some("W0"));
        assert_eq!(other.token, "W0");
        assert!(c.messages.is_empty());
    }

    #[test]
    fn removed_repeated_and_old_messages() {
        let mut c = Chat::default();
        let p = c.open("v", Some("T0"));
        let ids: Vec<String> = (0..KEEP + 5).map(|i| i.to_string()).collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let p = c.apply(p.gen, Ok(Some(page(&refs, Some("T1"), None)))).unwrap();
        assert_eq!(c.messages.len(), KEEP);
        assert_eq!(c.messages[0].id, "5");
        let mut next = page(&["204", "new"], Some("T2"), None);
        next.removed = vec!["10".into()];
        c.apply(p.gen, Ok(Some(next))).unwrap();
        assert_eq!(c.messages.last().unwrap().id, "new");
        assert!(!c.messages.iter().any(|m| m.id == "10"));
        assert_eq!(c.messages.iter().filter(|m| m.id == "204").count(), 1);
    }

    #[test]
    fn failures_retry_slower_and_the_end_stops() {
        let mut c = Chat::default();
        let p = c.open("v", Some("T0"));
        let p = c.apply(p.gen, Err("offline".into())).unwrap();
        assert_eq!((p.token.as_str(), p.delay_ms), ("T0", RETRY_STEP_MS));
        assert!(c.error.is_some());
        let p = c.apply(p.gen, Err("offline".into())).unwrap();
        assert_eq!(p.delay_ms, 2 * RETRY_STEP_MS);
        let mut p = p;
        for _ in 0..10 {
            p = c.apply(p.gen, Err("offline".into())).unwrap();
        }
        assert_eq!(p.delay_ms, RETRY_MAX_MS);
        let p = c.apply(p.gen, Ok(Some(page(&["1"], Some("T1"), None)))).unwrap();
        assert!(c.error.is_none());
        assert!(c.apply(p.gen, Ok(Some(page(&["2"], None, None)))).is_none());
        assert!(c.ended);
        assert_eq!(c.messages.len(), 2);
    }

    #[test]
    fn first_continuation_is_looked_up_and_a_missing_chat_stops() {
        let mut c = Chat::default();
        let p = c.open("v", None);
        assert_eq!((p.video_id.as_str(), p.token.as_str()), ("v", ""));
        assert!(c.apply(p.gen, Ok(None)).is_none());
        assert!(c.no_chat && !c.loading);
        // Opened again: it looks again (the chat may have been turned on meanwhile).
        c.close();
        let p = c.open("v", None);
        assert!(!c.no_chat && c.loading);
        assert_eq!(p.token, "");
    }

    #[test]
    fn scroll_follows_until_scrolled_back() {
        let mut s = Scroll::default();
        let bottoms = |ids: &'static [(&'static str, f32)]| move |id: &str| ids.iter().find(|(i, _)| *i == id).map(|(_, b)| *b);
        // Following: always at the bottom.
        assert_eq!(s.offset(bottoms(&[("a", 100.0), ("b", 200.0)]), 200.0, 50.0), 0.0);
        // Back 30 px from the newest ("b").
        s.scroll_by(30.0, 0.0, 150.0, Some("b"));
        assert!(!s.following());
        assert_eq!(s.offset(bottoms(&[("a", 100.0), ("b", 200.0)]), 200.0, 50.0), 30.0);
        // A message arrives below: the view stays on what it showed (60 px more below it).
        assert_eq!(s.offset(bottoms(&[("a", 100.0), ("b", 200.0), ("c", 260.0)]), 260.0, 50.0), 90.0);
        assert!(s.newer_below(Some("c")));
        // "a" dropped from the top: every bottom moves up, the offset from the bottom does not.
        assert_eq!(s.offset(bottoms(&[("b", 100.0), ("c", 160.0)]), 160.0, 50.0), 90.0);
        // Scrolling back down to the bottom follows again.
        s.scroll_by(-200.0, 90.0, 110.0, Some("c"));
        assert!(s.following() && !s.newer_below(Some("c")));
        // Never past the top.
        s.scroll_by(1000.0, 0.0, 110.0, Some("c"));
        assert_eq!(s.offset(bottoms(&[("b", 100.0), ("c", 160.0)]), 160.0, 50.0), 110.0);
        // The anchor removed: back to the newest.
        assert_eq!(s.offset(bottoms(&[("b", 100.0)]), 100.0, 50.0), 0.0);
        assert!(s.following());
    }
}

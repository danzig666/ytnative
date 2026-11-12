//! Watch history and resume position: the playback/watchtime pings a YouTube player sends
//! (SmartTube's `track` module), with the user's auth so the video lands in their history and the
//! position is saved for "resume".
//!
//! Flow: `Session::tracking_for(id)` (signed-in TV `player` call, only for its tracking URLs) ->
//! `report_start(pos)` when playback begins -> `report_progress(pos, playing)` every
//! `Tracking::next_flush_secs()` seconds and on pause/seek -> `report_end(pos)` when leaving.

use crate::innertube::find;
use crate::session::Session;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::time::Instant;

const CPN_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// A client playback nonce: 16 random chars of the URL-safe base64 alphabet.
pub fn cpn() -> String {
    let b: [u8; 16] = crate::accounts::random_bytes();
    b.iter().map(|x| CPN_ALPHABET[(x & 63) as usize] as char).collect()
}

/// Seconds as YouTube prints them in stats URLs ("12.345").
fn secs(s: f64) -> String {
    let s = format!("{:.3}", s.max(0.0));
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() { "0".into() } else { s.into() }
}

#[derive(Debug, Clone)]
pub struct Tracking {
    pub video_id: String,
    /// `playbackTracking.videostatsPlaybackUrl.baseUrl` (playback start).
    pub playback_url: Option<String>,
    /// `playbackTracking.videostatsWatchtimeUrl.baseUrl` (periodic watch time).
    pub watchtime_url: Option<String>,
    pub ptracking_url: Option<String>,
    pub length_secs: f64,
    pub cpn: String,
    pub client_version: String,
    /// Where to resume, when the player response says so.
    pub resume_ms: Option<u64>,
    /// `videostatsScheduledFlushWalltimeSeconds` (e.g. [10, 20, 30]): first pings, from start.
    pub flush_schedule: Vec<u64>,
    /// `videostatsDefaultFlushIntervalSeconds` (e.g. 40): ping period after the schedule.
    pub flush_interval: u64,
    started: Option<Instant>,
    /// Start of the segment the next watchtime ping reports.
    segment_start: f64,
    pings: usize,
    sent_playback: bool,
}

impl Tracking {
    pub fn from_player(video_id: &str, pr: &Value, client_version: &str) -> Option<Tracking> {
        let pt = &pr["playbackTracking"];
        let url = |k: &str| pt[k]["baseUrl"].as_str().map(str::to_string);
        let t = Tracking {
            video_id: video_id.into(),
            playback_url: url("videostatsPlaybackUrl"),
            watchtime_url: url("videostatsWatchtimeUrl"),
            ptracking_url: url("ptrackingUrl"),
            length_secs: pr["videoDetails"]["lengthSeconds"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .or(pr["videoDetails"]["lengthSeconds"].as_f64())
                .unwrap_or(0.0),
            cpn: cpn(),
            client_version: client_version.into(),
            resume_ms: resume_ms_from_player(pr),
            flush_schedule: pt["videostatsScheduledFlushWalltimeSeconds"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_u64).collect())
                .unwrap_or_else(|| vec![10, 20, 30]),
            flush_interval: pt["videostatsDefaultFlushIntervalSeconds"].as_u64().unwrap_or(40),
            started: None,
            segment_start: 0.0,
            pings: 0,
            sent_playback: false,
        };
        (t.playback_url.is_some() || t.watchtime_url.is_some()).then_some(t)
    }

    /// Seconds since `report_start`.
    fn rt(&self) -> f64 {
        self.started.map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0)
    }

    /// Seconds to wait before the next `report_progress` (scheduled flushes, then the interval).
    pub fn next_flush_secs(&self) -> u64 {
        let prev = if self.pings == 0 { 0 } else { self.flush_schedule.get(self.pings - 1).copied().unwrap_or(0) };
        match self.flush_schedule.get(self.pings) {
            Some(&at) => at.saturating_sub(prev).max(1),
            None => self.flush_interval.max(1),
        }
    }

    fn with_params(&self, base: &str, params: &[(&str, String)]) -> Option<String> {
        let mut url = reqwest::Url::parse(base).ok()?;
        let keys: Vec<&str> = params.iter().map(|(k, _)| *k).collect();
        let keep: Vec<(String, String)> =
            url.query_pairs().filter(|(k, _)| !keys.contains(&k.as_ref())).map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        {
            let mut q = url.query_pairs_mut();
            q.clear();
            for (k, v) in &keep {
                q.append_pair(k, v);
            }
            for (k, v) in params {
                q.append_pair(k, v);
            }
        }
        Some(url.into())
    }

    fn common(&self) -> Vec<(&'static str, String)> {
        let rt = self.rt();
        vec![
            ("ver", "2".into()),
            ("cpn", self.cpn.clone()),
            ("c", "TVHTML5".into()),
            ("cver", self.client_version.clone()),
            ("hl", "en_US".into()),
            ("len", secs(self.length_secs)),
            ("rt", secs(rt)),
            ("rtn", secs(rt)),
            ("lact", ((rt * 1000.0) as u64).to_string()),
        ]
    }

    /// The playback-start ping (`api/stats/playback`) at position `cmt` seconds.
    pub fn playback_ping_url(&self, cmt: f64) -> Option<String> {
        let mut p = self.common();
        p.push(("cmt", secs(cmt)));
        self.with_params(self.playback_url.as_deref()?, &p)
    }

    /// A watch-time ping (`api/stats/watchtime`) for the segment `st..et` seconds.
    pub fn watchtime_ping_url(&self, st: f64, et: f64, playing: bool, final_ping: bool) -> Option<String> {
        let mut p = self.common();
        p.extend([
            ("cmt", secs(et)),
            ("st", secs(st)),
            ("et", secs(et)),
            ("state", if playing { "playing" } else { "paused" }.into()),
            ("volume", "100".into()),
            ("muted", "0".into()),
        ]);
        if final_ping {
            p.push(("final", "1".into()));
        }
        self.with_params(self.watchtime_url.as_deref()?, &p)
    }

    pub fn ptracking_ping_url(&self) -> Option<String> {
        self.with_params(self.ptracking_url.as_deref()?, &[("cpn", self.cpn.clone())])
    }

    /// True when less than 5% (SmartTube's threshold) of the video remains.
    pub fn almost_watched(&self, pos: f64) -> bool {
        self.length_secs > 0.0 && self.length_secs - pos < self.length_secs * 0.05
    }
}

/// Resume position from a player response, when it carries one.
pub fn resume_ms_from_player(pr: &Value) -> Option<u64> {
    let pc = &pr["playerConfig"];
    for v in [
        &pc["playbackStartConfig"]["startSeconds"],
        &pc["resumePlaybackConfig"]["resumeOffsetSeconds"],
        &pr["playbackStartConfig"]["startSeconds"],
    ] {
        if let Some(s) = v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())) {
            if s > 0.0 {
                return Some((s * 1000.0) as u64);
            }
        }
    }
    None
}

/// Resume position from a raw tile (or any item JSON): `watchEndpoint.startTimeSeconds` first,
/// else `percentDurationWatched` of the resume overlay times `length_secs` (if known).
pub fn resume_ms_from_tile(tile: &Value, length_secs: Option<f64>) -> Option<u64> {
    let t = tile.get("tileRenderer").unwrap_or(tile);
    let start = find(&t["onSelectCommand"], "watchEndpoint")
        .or_else(|| find(&t["navigationEndpoint"], "watchEndpoint"))
        .and_then(|w| w["startTimeSeconds"].as_u64());
    if let Some(s) = start.filter(|s| *s > 0) {
        return Some(s * 1000);
    }
    let pct = find(t, "thumbnailOverlayResumePlaybackRenderer")?["percentDurationWatched"].as_f64()?;
    let len = length_secs?;
    (pct > 0.0 && pct < 100.0).then(|| (len * pct / 100.0 * 1000.0) as u64)
}

impl Session {
    /// Signed-in TV `player` call, used only for its tracking URLs (its formats are SABR-only).
    pub fn tracking_for(&mut self, video_id: &str) -> Result<Tracking> {
        if !self.signed_in() {
            bail!("tracking needs a signed-in session");
        }
        let pr = self.raw(
            "player",
            json!({
                "videoId": video_id,
                "playbackContext": { "contentPlaybackContext": { "html5Preference": "HTML5_PREF_WANTS" } },
                "contentCheckOk": true,
                "racyCheckOk": true,
            }),
        )?;
        let version = self.cfg.tv_client_version.clone();
        Tracking::from_player(video_id, &pr, &version)
            .with_context(|| format!("no playbackTracking for {video_id}: {}", pr["playabilityStatus"]))
    }

    /// GETs a stats URL with the session's auth.
    pub fn stats_ping(&mut self, url: &str) -> Result<()> {
        let bearer = self.bearer()?;
        let mut req = self.http.get(url).header("User-Agent", crate::bootstrap::TV_UA);
        if let Some(b) = bearer {
            req = req.bearer_auth(b);
        }
        if let Some(p) = &self.page_id {
            req = req.header("X-Goog-PageId", p);
        }
        let status = req.send()?.status();
        if !status.is_success() {
            bail!("stats ping -> HTTP {status}");
        }
        Ok(())
    }

    /// Playback started at `pos` seconds: creates the watch-history record.
    pub fn report_start(&mut self, t: &mut Tracking, pos: f64) -> Result<()> {
        t.started.get_or_insert_with(Instant::now);
        t.segment_start = pos;
        if let Some(u) = t.playback_ping_url(pos) {
            self.stats_ping(&u)?;
        }
        t.sent_playback = true;
        if let Some(u) = t.ptracking_ping_url() {
            let _ = self.stats_ping(&u);
        }
        Ok(())
    }

    /// Periodic / pause / seek report: the segment since the last report ends at `pos`.
    /// After a seek, call it with the pre-seek position first, then `report_seek(new_pos)`.
    pub fn report_progress(&mut self, t: &mut Tracking, pos: f64, playing: bool) -> Result<()> {
        if !t.sent_playback {
            self.report_start(t, pos)?;
        }
        let st = t.segment_start.min(pos);
        if let Some(u) = t.watchtime_ping_url(st, pos, playing, false) {
            self.stats_ping(&u)?;
        }
        t.segment_start = pos;
        t.pings += 1;
        Ok(())
    }

    /// The user jumped to `pos`: the next segment starts there.
    pub fn report_seek(&mut self, t: &mut Tracking, pos: f64) {
        t.segment_start = pos;
    }

    /// Playback ends (or the player closes) at `pos`. Near the end it marks the video watched.
    pub fn report_end(&mut self, t: &mut Tracking, pos: f64) -> Result<()> {
        if !t.sent_playback {
            self.report_start(t, pos)?;
        }
        let et = if t.almost_watched(pos) { t.length_secs } else { pos };
        let st = t.segment_start.min(et);
        if let Some(u) = t.watchtime_ping_url(st, et, false, true) {
            self.stats_ping(&u)?;
        }
        t.segment_start = et;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player() -> Value {
        json!({
            "videoDetails": {"lengthSeconds": "571"},
            "playbackTracking": {
                "videostatsPlaybackUrl": {"baseUrl": "https://s.youtube.com/api/stats/playback?cl=1&docid=VID&ei=EI&ns=yt&el=leanback&len=571&of=OF&vm=VM"},
                "videostatsWatchtimeUrl": {"baseUrl": "https://s.youtube.com/api/stats/watchtime?cl=1&docid=VID&ei=EI&ns=yt&el=leanback&len=571&of=OF&vm=VM"},
                "ptrackingUrl": {"baseUrl": "https://www.youtube.com/ptracking?ei=EI&video_id=VID"},
                "videostatsScheduledFlushWalltimeSeconds": [10, 20, 30],
                "videostatsDefaultFlushIntervalSeconds": 40
            }
        })
    }

    fn q(url: &str) -> Vec<(String, String)> {
        reqwest::Url::parse(url).unwrap().query_pairs().map(|(k, v)| (k.into(), v.into())).collect()
    }

    fn get(p: &[(String, String)], k: &str) -> Option<String> {
        let all: Vec<_> = p.iter().filter(|(kk, _)| kk == k).collect();
        assert!(all.len() <= 1, "duplicate {k}");
        all.first().map(|(_, v)| v.clone())
    }

    #[test]
    fn cpn_shape() {
        let a = cpn();
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|b| CPN_ALPHABET.contains(&b)));
        assert_ne!(a, cpn());
    }

    #[test]
    fn secs_format() {
        assert_eq!(secs(0.0), "0");
        assert_eq!(secs(12.5), "12.5");
        assert_eq!(secs(119.405), "119.405");
        assert_eq!(secs(-1.0), "0");
        assert_eq!(secs(571.0), "571");
    }

    #[test]
    fn ping_urls() {
        let t = Tracking::from_player("VID", &player(), "7.2026").unwrap();
        assert_eq!(t.length_secs, 571.0);
        let p = q(&t.playback_ping_url(12.5).unwrap());
        assert_eq!(get(&p, "docid").as_deref(), Some("VID"));
        assert_eq!(get(&p, "ei").as_deref(), Some("EI"));
        assert_eq!(get(&p, "of").as_deref(), Some("OF"));
        assert_eq!(get(&p, "vm").as_deref(), Some("VM"));
        assert_eq!(get(&p, "ver").as_deref(), Some("2"));
        assert_eq!(get(&p, "cmt").as_deref(), Some("12.5"));
        assert_eq!(get(&p, "len").as_deref(), Some("571"));
        assert_eq!(get(&p, "cpn"), Some(t.cpn.clone()));
        assert_eq!(get(&p, "c").as_deref(), Some("TVHTML5"));

        let w = q(&t.watchtime_ping_url(10.0, 50.25, true, false).unwrap());
        assert_eq!(get(&w, "st").as_deref(), Some("10"));
        assert_eq!(get(&w, "et").as_deref(), Some("50.25"));
        assert_eq!(get(&w, "cmt").as_deref(), Some("50.25"));
        assert_eq!(get(&w, "state").as_deref(), Some("playing"));
        assert_eq!(get(&w, "volume").as_deref(), Some("100"));
        assert_eq!(get(&w, "final"), None);
        let f = q(&t.watchtime_ping_url(50.0, 60.0, false, true).unwrap());
        assert_eq!(get(&f, "final").as_deref(), Some("1"));
        assert_eq!(get(&f, "state").as_deref(), Some("paused"));
        assert!(t.watchtime_ping_url(0.0, 1.0, true, false).unwrap().starts_with("https://s.youtube.com/api/stats/watchtime?"));
        let pt = q(&t.ptracking_ping_url().unwrap());
        assert_eq!(get(&pt, "cpn"), Some(t.cpn.clone()));
    }

    #[test]
    fn flush_schedule() {
        let mut t = Tracking::from_player("VID", &player(), "7").unwrap();
        let mut seq = Vec::new();
        for _ in 0..5 {
            seq.push(t.next_flush_secs());
            t.pings += 1;
        }
        assert_eq!(seq, [10, 10, 10, 40, 40]);
        assert!(t.almost_watched(560.0));
        assert!(!t.almost_watched(300.0));
        assert!(Tracking::from_player("VID", &json!({}), "7").is_none());
    }

    #[test]
    fn resume_hints() {
        let tile = json!({"tileRenderer":{"onSelectCommand":{"watchEndpoint":{"videoId":"v","startTimeSeconds":22}},
            "header":{"tileHeaderRenderer":{"thumbnailOverlays":[{"thumbnailOverlayResumePlaybackRenderer":{"percentDurationWatched":10}}]}}}});
        assert_eq!(resume_ms_from_tile(&tile, None), Some(22_000));
        let tile = json!({"header":{"tileHeaderRenderer":{"thumbnailOverlays":[{"thumbnailOverlayResumePlaybackRenderer":{"percentDurationWatched":10}}]}}});
        assert_eq!(resume_ms_from_tile(&tile, Some(600.0)), Some(60_000));
        assert_eq!(resume_ms_from_tile(&tile, None), None);
        assert_eq!(resume_ms_from_player(&json!({"playerConfig":{"playbackStartConfig":{"startSeconds":"41.5"}}})), Some(41_500));
        assert_eq!(resume_ms_from_player(&player()), None);
    }

    #[test]
    fn private_fixtures() {
        let base = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/private/");
        if let Ok(raw) = std::fs::read_to_string(format!("{base}player_tv.json")) {
            let t = Tracking::from_player("jNQXAC9IVRw", &serde_json::from_str(&raw).unwrap(), "7").unwrap();
            assert!(t.playback_url.is_some() && t.watchtime_url.is_some());
            assert!(t.length_secs > 0.0);
        }
    }

    /// Watches the first seconds of a public test video on the real account, checks it shows up in
    /// watch history, then removes exactly that entry again ("Remove from watch history").
    #[test]
    #[ignore]
    fn live_history_cycle() {
        use crate::actions::{menu_map, MenuKind};
        const VID: &str = "jNQXAC9IVRw";
        crate::accounts::live_data_dir();
        let mut s = Session::new().unwrap();
        let hist = |s: &mut Session| s.raw("browse", json!({ "browseId": "FEhistory" })).unwrap();
        if menu_map(&hist(&mut s)).contains_key(VID) {
            eprintln!("skipping: test video already in watch history");
            return;
        }
        let mut t = s.tracking_for(VID).unwrap();
        s.report_start(&mut t, 0.0).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(3));
        s.report_progress(&mut t, 3.0, true).unwrap();
        s.report_end(&mut t, 5.0).unwrap();

        let mut token = None;
        let mut resume = None;
        for _ in 0..10 {
            let h = hist(&mut s);
            if let Some(acts) = menu_map(&h).get(VID) {
                token = acts.iter().find(|a| a.kind == MenuKind::RemoveFromHistory).and_then(|a| a.token().map(str::to_string));
                let mut tiles = Vec::new();
                find_tiles(&h, &mut tiles);
                resume = tiles.iter().find(|t| t["contentId"] == VID).and_then(|t| resume_ms_from_tile(t, Some(19.0)));
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
        let token = token.expect("video did not appear in watch history");
        s.feedback(&[&token]).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(3));
        assert!(!menu_map(&hist(&mut s)).contains_key(VID), "history entry was not removed");
        eprintln!("resume hint after watching to 5 s: {resume:?} ms");
    }

    fn find_tiles<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
        match v {
            Value::Object(m) => {
                if let Some(t) = m.get("tileRenderer") {
                    out.push(t);
                }
                m.values().for_each(|c| find_tiles(c, out));
            }
            Value::Array(a) => a.iter().for_each(|c| find_tiles(c, out)),
            _ => {}
        }
    }
}

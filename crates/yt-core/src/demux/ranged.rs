//! Direct-URL formats (`adaptiveFormats[].url`): one fMP4/WebM file per format, fetched with
//! bounded HTTP Range requests. The init part and the index (`sidx` / `Cues`) come first; the
//! index maps time to byte ranges, so a seek is "fetch the segment containing t".
//!
//! YouTube throttles single huge ranges, so every request is capped at [`MAX_RANGE`].

use super::{AccessUnit, Container, Demuxer, SegmentRef, TrackInfo};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::io::Read;
use std::time::Duration;

/// Largest single Range request.
pub const MAX_RANGE: u64 = 4 << 20;

fn num(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// Parallel requests per range: YouTube limits each connection (about 9 to 18 Mbit/s measured
/// in November 2026, less than a 1440p60 AV1 stream's 13.8 Mbit/s peak), not the line (four
/// connections at once got 26 Mbit/s), so one connection left high-bitrate videos stalling.
pub const PARALLEL: u64 = 3;
/// Ranges smaller than this go in one request.
const MIN_PART: u64 = 512 << 10;

/// Fetches bytes `start..=end` (inclusive), in requests of at most [`MAX_RANGE`], up to
/// [`PARALLEL`] at a time, retrying each a few times. `sink` receives the data in order, chunk by
/// chunk.
pub fn fetch_range_with(
    http: &reqwest::blocking::Client,
    url: &str,
    user_agent: &str,
    start: u64,
    end: u64,
    sink: &mut dyn FnMut(&[u8]) -> Result<()>,
) -> Result<()> {
    let mut at = start;
    while at <= end {
        let parts = plan_parts(at, end);
        let results: Vec<Result<Vec<u8>>> = if parts.len() == 1 {
            vec![fetch_retry(http, url, user_agent, parts[0].0, parts[0].1)]
        } else {
            std::thread::scope(|sc| {
                let hs: Vec<_> = parts.iter().map(|&(a, b)| sc.spawn(move || fetch_retry(http, url, user_agent, a, b))).collect();
                hs.into_iter().map(|h| h.join().unwrap_or_else(|_| Err(anyhow::anyhow!("range thread panicked")))).collect()
            })
        };
        for ((a, b), r) in parts.iter().zip(results) {
            let data = r?;
            if data.is_empty() {
                bail!("empty range response at {a}");
            }
            sink(&data)?;
            if (data.len() as u64) < b + 1 - a {
                // A short answer (the end of the file): carry on from where it stopped.
                at = a + data.len() as u64;
                break;
            }
            at = b + 1;
        }
    }
    Ok(())
}

/// The next window of up to [`PARALLEL`] requests from `at` towards `end` (inclusive bounds).
fn plan_parts(at: u64, end: u64) -> Vec<(u64, u64)> {
    let left = end + 1 - at;
    let window = left.min(PARALLEL * MAX_RANGE);
    let n = if window < 2 * MIN_PART { 1 } else { PARALLEL.min(window / MIN_PART).max(1) };
    let part = window.div_ceil(n);
    let mut out = Vec::new();
    let mut a = at;
    while a < at + window {
        let b = (a + part).min(at + window) - 1;
        out.push((a, b));
        a = b + 1;
    }
    out
}

fn fetch_retry(http: &reqwest::blocking::Client, url: &str, user_agent: &str, a: u64, b: u64) -> Result<Vec<u8>> {
    let mut tries = 0;
    loop {
        match fetch_once(http, url, user_agent, a, b) {
            Ok(d) => return Ok(d),
            Err(e) => {
                tries += 1;
                if tries >= 3 {
                    return Err(e);
                }
                std::thread::sleep(Duration::from_millis(300 * tries));
            }
        }
    }
}

pub fn fetch_range(http: &reqwest::blocking::Client, url: &str, user_agent: &str, start: u64, end: u64) -> Result<Vec<u8>> {
    let len = (end + 1).checked_sub(start).filter(|l| *l > 0).with_context(|| format!("bad byte range {start}-{end}"))?;
    let mut out = Vec::with_capacity(len.min(PARALLEL * MAX_RANGE) as usize);
    fetch_range_with(http, url, user_agent, start, end, &mut |d| {
        out.extend_from_slice(d);
        Ok(())
    })?;
    Ok(out)
}

fn fetch_once(http: &reqwest::blocking::Client, url: &str, user_agent: &str, start: u64, end: u64) -> Result<Vec<u8>> {
    let mut resp = http
        .get(url)
        .header("User-Agent", user_agent)
        .header("Range", format!("bytes={start}-{end}"))
        .header("Accept-Encoding", "identity")
        .send()
        .context("range request")?;
    let st = resp.status();
    if st.as_u16() != 206 && !st.is_success() {
        bail!("range {start}-{end}: HTTP {st}");
    }
    if st.as_u16() == 200 && start > 0 {
        bail!("server ignored the Range header");
    }
    // Read at most the asked-for bytes: a server answering 200 with the whole file must not
    // pull hundreds of MB into memory.
    let want = end + 1 - start;
    let mut out = Vec::with_capacity(want.min(MAX_RANGE) as usize);
    (&mut resp).take(want).read_to_end(&mut out).context("range body")?;
    Ok(out)
}

/// The error of a segment fetch given up by its `abort` check (see
/// [`RangedTrack::fetch_segment_abortable`]); not a network failure.
#[derive(Debug)]
pub struct Aborted;

impl std::fmt::Display for Aborted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("segment fetch aborted")
    }
}

impl std::error::Error for Aborted {}

/// One direct-URL format, opened: tracks + segment index known.
pub struct RangedTrack {
    pub url: String,
    pub itag: i32,
    pub container: Container,
    pub content_length: u64,
    pub segments: Vec<SegmentRef>,
    pub demux: Box<dyn Demuxer>,
    pub user_agent: String,
    http: reqwest::blocking::Client,
}

impl RangedTrack {
    /// `fmt` is one `adaptiveFormats` entry with `url`, `initRange`, `indexRange`.
    pub fn open(http: reqwest::blocking::Client, fmt: &Value, user_agent: &str) -> Result<RangedTrack> {
        let url = fmt["url"].as_str().context("format has no direct url")?.to_string();
        let itag = fmt["itag"].as_i64().unwrap_or(0) as i32;
        let container = Container::from_mime(fmt["mimeType"].as_str().unwrap_or(""));
        let init_end = num(&fmt["initRange"]["end"]).context("format has no initRange")?;
        let (ix_start, ix_end) = (
            num(&fmt["indexRange"]["start"]).context("format has no indexRange")?,
            num(&fmt["indexRange"]["end"]).context("format has no indexRange")?,
        );
        let content_length = num(&fmt["contentLength"]).unwrap_or(0);
        let mut demux = container.demuxer();
        if ix_start > init_end + 1 && ix_start - init_end > 1 << 20 {
            demux.push(&fetch_range(&http, &url, user_agent, 0, init_end)?)?;
            demux.reset_to(ix_start);
            demux.push(&fetch_range(&http, &url, user_agent, ix_start, ix_end)?)?;
        } else {
            demux.push(&fetch_range(&http, &url, user_agent, 0, init_end.max(ix_end))?)?;
        }
        if demux.tracks().is_empty() {
            bail!("itag {itag}: no tracks in the init segment");
        }
        let mut segments = demux.index().map(<[SegmentRef]>::to_vec).unwrap_or_default();
        if segments.is_empty() {
            bail!("itag {itag}: no segment index");
        }
        for s in &mut segments {
            if s.size == 0 && content_length > s.offset {
                s.size = content_length - s.offset;
            }
        }
        Ok(RangedTrack { url, itag, container, content_length, segments, demux, user_agent: user_agent.to_string(), http })
    }

    pub fn track(&self) -> &TrackInfo {
        &self.demux.tracks()[0]
    }

    /// Index of the segment containing `t_ns` (the first one before the start, the last one
    /// past the end).
    pub fn segment_at(&self, t_ns: i64) -> usize {
        match self.segments.iter().rposition(|s| s.start_ns <= t_ns) {
            Some(i) => i,
            None => 0,
        }
    }

    pub fn duration_ns(&self) -> i64 {
        self.segments.last().map(|s| s.start_ns + s.duration_ns).or_else(|| self.demux.duration_ns()).unwrap_or(0)
    }

    /// Downloads segment `i` and returns its access units (decode order).
    pub fn fetch_segment(&mut self, i: usize) -> Result<Vec<AccessUnit>> {
        let mut out = Vec::new();
        self.fetch_segment_with(i, &mut |au| {
            out.push(au);
            Ok(())
        })?;
        Ok(out)
    }

    /// Like [`fetch_segment`](Self::fetch_segment), but gives up with [`Aborted`] as soon as
    /// `abort` returns true between two ranges (a stop or a seek), instead of downloading the
    /// rest of a segment nobody wants any more.
    pub fn fetch_segment_abortable(&mut self, i: usize, abort: &dyn Fn() -> bool) -> Result<Vec<AccessUnit>> {
        let mut out = Vec::new();
        self.fetch_segment_inner(i, abort, &mut |au| {
            out.push(au);
            Ok(())
        })?;
        Ok(out)
    }

    /// Like [`fetch_segment_abortable`](Self::fetch_segment_abortable), handing units over as the
    /// data arrives instead of after the whole segment (a 4K segment can be 50 MB).
    pub fn fetch_segment_streaming(&mut self, i: usize, abort: &dyn Fn() -> bool, sink: &mut dyn FnMut(AccessUnit) -> Result<()>) -> Result<()> {
        self.fetch_segment_inner(i, abort, sink)
    }

    /// Like [`fetch_segment`](Self::fetch_segment), handing units over as each range arrives.
    pub fn fetch_segment_with(&mut self, i: usize, sink: &mut dyn FnMut(AccessUnit) -> Result<()>) -> Result<()> {
        self.fetch_segment_inner(i, &|| false, sink)
    }

    fn fetch_segment_inner(&mut self, i: usize, abort: &dyn Fn() -> bool, sink: &mut dyn FnMut(AccessUnit) -> Result<()>) -> Result<()> {
        let s = *self.segments.get(i).context("segment index out of range")?;
        if s.size == 0 {
            bail!("segment {i} has unknown size");
        }
        self.demux.reset_to(s.offset);
        let (http, url, ua) = (self.http.clone(), self.url.clone(), self.user_agent.clone());
        let demux = &mut self.demux;
        fetch_range_with(&http, &url, &ua, s.offset, s.offset + s.size - 1, &mut |d| {
            if abort() {
                return Err(Aborted.into());
            }
            demux.push(d)?;
            while let Some(au) = demux.pop() {
                sink(au)?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod plan_tests {
    use super::*;

    fn covers(at: u64, end: u64) {
        let mut a = at;
        while a <= end {
            let parts = plan_parts(a, end);
            assert!(!parts.is_empty() && parts.len() as u64 <= PARALLEL);
            for w in parts.windows(2) {
                assert_eq!(w[0].1 + 1, w[1].0, "contiguous");
            }
            for (x, y) in &parts {
                assert!(x <= y && y - x < MAX_RANGE, "{x}-{y}");
            }
            assert_eq!(parts[0].0, a);
            a = parts.last().unwrap().1 + 1;
        }
        assert_eq!(a, end + 1, "ends exactly at the end");
    }

    #[test]
    fn parts_cover_the_range_without_gaps_or_overlap() {
        for (at, end) in [(0, 0), (0, 100), (10, 600_000), (0, (1 << 20) - 1), (5, 9_000_000), (0, 40 << 20), (123, 123 + 3 * MAX_RANGE)] {
            covers(at, end);
        }
    }

    #[test]
    fn small_ranges_take_one_request_and_large_ones_several() {
        assert_eq!(plan_parts(0, 1000).len(), 1);
        assert_eq!(plan_parts(0, (8 << 20) - 1).len(), PARALLEL as usize);
    }
}

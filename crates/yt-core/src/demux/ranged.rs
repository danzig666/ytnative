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

/// Fetches bytes `start..=end` (inclusive), in requests of at most [`MAX_RANGE`], retrying each
/// a few times. `sink` receives the data chunk by chunk.
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
        let to = end.min(at + MAX_RANGE - 1);
        let mut tries = 0;
        let data = loop {
            match fetch_once(http, url, user_agent, at, to) {
                Ok(d) => break d,
                Err(e) => {
                    tries += 1;
                    if tries >= 3 {
                        return Err(e);
                    }
                    std::thread::sleep(Duration::from_millis(300 * tries));
                }
            }
        };
        if data.is_empty() {
            bail!("empty range response at {at}");
        }
        at += data.len() as u64;
        sink(&data)?;
    }
    Ok(())
}

pub fn fetch_range(http: &reqwest::blocking::Client, url: &str, user_agent: &str, start: u64, end: u64) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity((end + 1 - start) as usize);
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
    let mut out = Vec::with_capacity((end + 1 - start) as usize);
    resp.read_to_end(&mut out).context("range body")?;
    if st.as_u16() == 200 && start > 0 {
        bail!("server ignored the Range header");
    }
    out.truncate((end + 1 - start) as usize);
    Ok(out)
}

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

    /// Like [`fetch_segment`](Self::fetch_segment), handing units over as each range arrives.
    pub fn fetch_segment_with(&mut self, i: usize, sink: &mut dyn FnMut(AccessUnit) -> Result<()>) -> Result<()> {
        let s = *self.segments.get(i).context("segment index out of range")?;
        if s.size == 0 {
            bail!("segment {i} has unknown size");
        }
        self.demux.reset_to(s.offset);
        let (http, url, ua) = (self.http.clone(), self.url.clone(), self.user_agent.clone());
        let demux = &mut self.demux;
        fetch_range_with(&http, &url, &ua, s.offset, s.offset + s.size - 1, &mut |d| {
            demux.push(d)?;
            while let Some(au) = demux.pop() {
                sink(au)?;
            }
            Ok(())
        })
    }
}

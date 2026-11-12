//! Subtitles: download a caption track (srv3 by default, WebVTT or TTML too), parse it into plain
//! timed cues and make auto-generated (ASR) tracks readable.
//!
//! ASR tracks are "rolling": YouTube shows two rows, the upper one being the previous line, and
//! the VTT form repeats that previous line in every cue plus 10 ms transition cues. We turn them
//! into a sequence of distinct lines and then into non-overlapping two-row cues, so every word is
//! shown once per line and nothing flickers.

use crate::streams::CaptionTrack;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cue {
    pub start_ms: u64,
    pub end_ms: u64,
    /// Plain text; rows separated by '\n'.
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Srv3,
    Vtt,
    Ttml,
}

impl Format {
    pub fn param(self) -> &'static str {
        match self {
            Format::Srv3 => "srv3",
            Format::Vtt => "vtt",
            Format::Ttml => "ttml",
        }
    }
}

/// Download URL of a track, optionally auto-translated (`tlang`).
pub fn track_url(track: &CaptionTrack, fmt: Format, translate_to: Option<&str>) -> String {
    let mut u = track.base_url.clone();
    // Drop a fmt/tlang the base URL may already carry.
    for key in ["fmt", "tlang"] {
        if let Some(i) = u.find(&format!("&{key}=")) {
            let end = u[i + 1..].find('&').map(|e| i + 1 + e).unwrap_or(u.len());
            u.replace_range(i..end, "");
        }
    }
    u.push_str(&format!("&fmt={}", fmt.param()));
    if let Some(l) = translate_to {
        u.push_str(&format!("&tlang={l}"));
    }
    u
}

/// Downloads and parses a track. ASR tracks are de-rolled (see module docs).
pub fn fetch(http: &reqwest::blocking::Client, track: &CaptionTrack, translate_to: Option<&str>) -> Result<Vec<Cue>> {
    let mut last_err = None;
    for fmt in [Format::Srv3, Format::Vtt] {
        let resp = http.get(track_url(track, fmt, translate_to)).send()?;
        let status = resp.status();
        let body = resp.text()?;
        if !status.is_success() || body.trim_start().starts_with("<html") {
            last_err = Some(format!("{} -> HTTP {status}{}", fmt.param(), if body.contains("Sorry...") { " (rate limited)" } else { "" }));
            continue;
        }
        let cues = parse(&body, track.auto);
        if !cues.is_empty() || body.len() < 200 {
            return Ok(cues);
        }
        last_err = Some(format!("{}: no cues parsed", fmt.param()));
    }
    bail!("captions {}: {}", track.language, last_err.unwrap_or_default())
}

impl crate::session::Session {
    /// Downloads a caption track as cues (see [`fetch`]).
    pub fn captions(&self, track: &CaptionTrack, translate_to: Option<&str>) -> Result<Vec<Cue>> {
        fetch(&self.http, track, translate_to)
    }
}

/// Parses any supported format (detected from the content).
pub fn parse(body: &str, auto: bool) -> Vec<Cue> {
    let t = body.trim_start_matches('\u{feff}').trim_start();
    if t.starts_with("WEBVTT") {
        parse_vtt(t, auto)
    } else if t.contains("<timedtext") {
        parse_srv3(t, auto)
    } else if t.contains("<tt") {
        parse_ttml(t)
    } else {
        Vec::new()
    }
}

/// The cue showing at `pos_ms` (cues sorted by start). With overlaps, the latest-started wins.
pub fn cue_at(cues: &[Cue], pos_ms: u64) -> Option<&Cue> {
    let n = cues.partition_point(|c| c.start_ms <= pos_ms);
    let c = cues.get(n.checked_sub(1)?)?;
    (pos_ms < c.end_ms).then_some(c)
}

// ---------- WebVTT ----------

pub fn parse_vtt(body: &str, auto: bool) -> Vec<Cue> {
    let body = body.replace("\r\n", "\n");
    let mut raw = Vec::new();
    for block in body.split("\n\n") {
        let mut lines = block.lines();
        let Some(timing) = lines.by_ref().find(|l| l.contains("-->")) else { continue };
        let mut parts = timing.split("-->");
        let (Some(a), Some(b)) = (parts.next(), parts.next()) else { continue };
        let (Some(start), Some(end)) = (vtt_time(a.trim()), vtt_time(b.split_whitespace().next().unwrap_or(""))) else { continue };
        let text: Vec<String> = lines.map(clean_line).collect();
        raw.push((start, end, text));
    }
    // Rolling ASR cues already hold the two on-screen rows (previous line + the new one, word
    // timing tags stripped); the few-ms transition cues between them only repeat the settled
    // text, so they go.
    let min_len = if auto { 50 } else { 1 };
    finish(
        raw.into_iter()
            .filter(|(s, e, _)| e.saturating_sub(*s) >= min_len)
            .map(|(s, e, rows)| Cue { start_ms: s, end_ms: e, text: join_rows(&rows) })
            .collect(),
    )
}

fn vtt_time(s: &str) -> Option<u64> {
    let (hms, ms) = s.split_once('.').unwrap_or((s, "0"));
    let mut total = 0u64;
    for p in hms.split(':') {
        total = total * 60 + p.trim().parse::<u64>().ok()?;
    }
    let ms: u64 = format!("{:0<3}", &ms[..ms.len().min(3)]).parse().ok()?;
    Some(total * 1000 + ms)
}

// ---------- srv3 (YouTube timed text format 3) ----------

pub fn parse_srv3(body: &str, auto: bool) -> Vec<Cue> {
    let mut cues = Vec::new();
    for (attrs, inner) in elements(body, "p") {
        let Some(start) = attr(attrs, "t").and_then(|v| v.parse::<u64>().ok()) else { continue };
        let dur = attr(attrs, "d").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        // `a="1"` paragraphs are just the "start a new row" markers of rolling captions.
        if attr(attrs, "a") == Some("1") {
            continue;
        }
        let inner = inner.replace("<br />", "\n").replace("<br/>", "\n");
        let text = join_rows(&inner.split('\n').map(clean_line).collect::<Vec<_>>());
        if text.is_empty() || dur == 0 {
            continue;
        }
        cues.push(Cue { start_ms: start, end_ms: start + dur, text });
    }
    if auto {
        derolled(cues)
    } else {
        finish(cues)
    }
}

// ---------- TTML ----------

pub fn parse_ttml(body: &str) -> Vec<Cue> {
    let mut cues = Vec::new();
    for (attrs, inner) in elements(body, "p") {
        let (Some(b), Some(e)) = (attr(attrs, "begin").and_then(ttml_time), attr(attrs, "end").and_then(ttml_time)) else { continue };
        let inner = inner.replace("<br />", "\n").replace("<br/>", "\n");
        let text = join_rows(&inner.split('\n').map(clean_line).collect::<Vec<_>>());
        cues.push(Cue { start_ms: b, end_ms: e, text });
    }
    finish(cues)
}

fn ttml_time(s: &str) -> Option<u64> {
    if let Some(ms) = s.strip_suffix("ms") {
        return ms.parse::<f64>().ok().map(|v| v as u64);
    }
    if let Some(sec) = s.strip_suffix('s') {
        return sec.parse::<f64>().ok().map(|v| (v * 1000.0).round() as u64);
    }
    vtt_time(s)
}

// ---------- shared ----------

/// Rolling lines -> non-overlapping two-row cues: each line shows until the next one starts,
/// under the previous line while that one is still on screen.
fn derolled(lines: Vec<Cue>) -> Vec<Cue> {
    // Merge consecutive repeats of the same line.
    let mut ls: Vec<Cue> = Vec::new();
    for l in lines {
        match ls.last_mut() {
            Some(p) if p.text == l.text && l.start_ms <= p.end_ms + 50 => p.end_ms = p.end_ms.max(l.end_ms),
            _ => ls.push(l),
        }
    }
    ls.sort_by_key(|c| c.start_ms);
    let mut out = Vec::with_capacity(ls.len());
    for i in 0..ls.len() {
        let cur = &ls[i];
        let end = match ls.get(i + 1) {
            Some(n) if n.start_ms < cur.end_ms => n.start_ms,
            _ => cur.end_ms,
        };
        if end <= cur.start_ms {
            continue;
        }
        let text = match i.checked_sub(1).map(|p| &ls[p]) {
            // The previous line still on screen (its own timing overlaps this one).
            Some(p) if p.end_ms > cur.start_ms && !is_sound_tag(&p.text) && !is_sound_tag(&cur.text) => format!("{}\n{}", p.text, cur.text),
            _ => cur.text.clone(),
        };
        out.push(Cue { start_ms: cur.start_ms, end_ms: end, text });
    }
    out
}

fn is_sound_tag(s: &str) -> bool {
    s.starts_with('[') && s.ends_with(']')
}

fn finish(mut cues: Vec<Cue>) -> Vec<Cue> {
    cues.retain(|c| !c.text.is_empty() && c.end_ms > c.start_ms);
    cues.sort_by_key(|c| c.start_ms);
    cues
}

fn join_rows(rows: &[String]) -> String {
    rows.iter().map(|r| r.trim()).filter(|r| !r.is_empty()).collect::<Vec<_>>().join("\n")
}

/// Strips markup (`<c>`, `<i>`, `<00:00:01.000>`, `<s ...>`, `<font>`) and decodes entities.
fn clean_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    let out = decode_entities(&out);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(semi) = tail[..tail.len().min(12)].find(';') else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let ent = &tail[1..semi];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ if ent.starts_with("#x") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &tail[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `<tag attrs>inner</tag>` occurrences (no nesting of the same tag; self-closing skipped).
fn elements<'a>(body: &'a str, tag: &str) -> Vec<(&'a str, &'a str)> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        // Must be the whole tag name ("<p " / "<p>"), not "<pen".
        if !after.starts_with([' ', '>', '\n', '\t']) {
            rest = after;
            continue;
        }
        let Some(gt) = after.find('>') else { break };
        let attrs = &after[..gt];
        if attrs.ends_with('/') {
            rest = &after[gt + 1..];
            continue;
        }
        let content = &after[gt + 1..];
        let Some(end) = content.find(&close) else { break };
        out.push((attrs, &content[..end]));
        rest = &content[end + close.len()..];
    }
    out
}

fn attr<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = attrs;
    while let Some(i) = rest.find(name) {
        let before_ok = i == 0 || rest[..i].ends_with([' ', '\n', '\t']);
        let after = &rest[i + name.len()..];
        if before_ok {
            if let Some(v) = after.strip_prefix("=\"") {
                return v.find('"').map(|e| &v[..e]);
            }
        }
        rest = after;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASR_VTT: &str = "WEBVTT\nKind: captions\nLanguage: en\n\n\
00:00:00.320 --> 00:00:18.790 align:start position:0%\n \n[Music]\n\n\
00:00:18.790 --> 00:00:18.800 align:start position:0%\n \n \n\n\
00:00:18.800 --> 00:00:21.790 align:start position:0%\n \nWe're<00:00:19.039><c> no</c><00:00:19.359><c> strangers</c><00:00:19.840><c> to</c>\n\n\
00:00:21.790 --> 00:00:21.800 align:start position:0%\nWe're no strangers to\n \n\n\
00:00:21.800 --> 00:00:25.950 align:start position:0%\nWe're no strangers to\nlove.<00:00:22.800><c> You</c><00:00:23.039><c> know</c>\n\n\
00:00:25.950 --> 00:00:25.960 align:start position:0%\nlove. You know\n \n\n\
00:00:25.960 --> 00:00:29.109 align:start position:0%\nlove. You know\nI.<00:00:27.039><c> I</c><00:00:27.279><c> feel</c>\n";

    const ASR_SRV3: &str = r#"<?xml version="1.0" encoding="utf-8" ?><timedtext format="3">
<head><ws id="0"/><pen id="1"/></head><body>
<w t="0" id="1" wp="1" ws="1"/>
<p t="320" d="14260" w="1">[Music]</p>
<p t="18790" w="1" a="1">
</p>
<p t="18800" d="7160" w="1"><s ac="0">We&#39;re</s><s t="239" ac="0"> no</s><s t="559" ac="0"> strangers</s><s t="1040" ac="0"> to</s></p>
<p t="21790" d="4170" w="1" a="1">
</p>
<p t="21800" d="7319" w="1"><s ac="0">love.</s><s t="1000" ac="0"> You</s><s t="1239" ac="0"> know</s></p>
<p t="25950" d="3169" w="1" a="1">
</p>
<p t="25960" d="3149" w="1"><s ac="0">I.</s><s t="1079" ac="0"> I</s><s t="1319" ac="0"> feel</s></p>
</body></timedtext>"#;

    fn check_asr(c: &[Cue]) {
        let texts: Vec<&str> = c.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["[Music]", "We're no strangers to", "We're no strangers to\nlove. You know", "love. You know\nI. I feel"], "{c:#?}");
        // Non-overlapping and contiguous rows.
        for w in c.windows(2) {
            assert!(w[0].end_ms <= w[1].start_ms);
        }
        assert_eq!(cue_at(c, 22_000).unwrap().text, "We're no strangers to\nlove. You know");
        assert!(cue_at(c, 18_795).is_none());
        assert!(cue_at(c, 100).is_none());
    }

    #[test]
    fn asr_vtt_derolled() {
        check_asr(&parse(ASR_VTT, true));
    }

    #[test]
    fn asr_srv3_derolled() {
        check_asr(&parse(ASR_SRV3, true));
    }

    #[test]
    fn manual_vtt_and_ttml() {
        let vtt = "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.500\n<i>Hello</i> &amp; <c.colorE5E5E5>welcome</c>\nsecond row\n\n00:01:00.000 --> 00:01:01.000 line:90%\nbye\n";
        let c = parse(vtt, false);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0], Cue { start_ms: 1000, end_ms: 2500, text: "Hello & welcome\nsecond row".into() });
        assert_eq!(c[1].start_ms, 60_000);
        let ttml = r#"<?xml version="1.0"?><tt xmlns="http://www.w3.org/ns/ttml"><body><div>
<p begin="00:00:01.000" end="00:00:02.000" style="s2">One<br />Two</p>
<p begin="00:00:03.000" end="00:00:04.000">&quot;Three&quot;</p></div></body></tt>"#;
        let c = parse(ttml, false);
        assert_eq!(c[0].text, "One\nTwo");
        assert_eq!(c[1].text, "\"Three\"");
        assert_eq!(cue_at(&c, 3500).unwrap().start_ms, 3000);
    }

    #[test]
    fn manual_srv3() {
        let s = r#"<timedtext format="3"><body><p t="1000" d="1500">Line one
line &amp; two</p><p t="3000" d="1000"><s>a</s><s> b</s></p></body></timedtext>"#;
        let c = parse(s, false);
        assert_eq!(c[0].text, "Line one\nline & two");
        assert_eq!(c[1], Cue { start_ms: 3000, end_ms: 4000, text: "a b".into() });
    }

    #[test]
    fn url_building() {
        let t = CaptionTrack { base_url: "https://x/api/timedtext?v=1&fmt=srv1&lang=en".into(), name: "English".into(), language: "en".into(), auto: false, translatable: true };
        assert_eq!(track_url(&t, Format::Vtt, Some("hu")), "https://x/api/timedtext?v=1&lang=en&fmt=vtt&tlang=hu");
    }

    #[test]
    #[ignore]
    fn live_captions() {
        let http = reqwest::blocking::Client::builder().gzip(true).build().unwrap();
        let cfg = crate::bootstrap::fetch(&http).unwrap();
        let api = crate::innertube::Api { http: &http, cfg: &cfg };
        let r = crate::streams::resolve(&api, "dQw4w9WgXcQ", None).unwrap();
        let manual = r.captions.iter().find(|t| !t.auto && t.language.starts_with("en")).expect("manual en");
        let asr = r.captions.iter().find(|t| t.auto).expect("asr");
        let m = fetch(&http, manual, None).unwrap();
        eprintln!("manual: {} cues; first {:?}", m.len(), m.first());
        assert!(m.len() > 10);
        let a = fetch(&http, asr, None).unwrap();
        eprintln!("asr: {} cues; at 30s {:?}", a.len(), cue_at(&a, 30_000));
        assert!(a.len() > 10);
        for w in a.windows(2) {
            assert!(w[0].end_ms <= w[1].start_ms);
        }
        match fetch(&http, manual, Some("hu")) {
            Ok(t) => {
                eprintln!("hu: {} cues; at 30s {:?}", t.len(), cue_at(&t, 30_000));
                assert!(!t.is_empty());
            }
            // 2026-11: timedtext answers `tlang` requests without a PO token with 429; report it
            // rather than fail, the untranslated paths above are what this test guards.
            Err(e) if e.to_string().contains("429") => eprintln!("SKIP translation: {e}"),
            Err(e) => panic!("translation failed: {e}"),
        }
    }
}

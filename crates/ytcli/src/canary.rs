//! `ytcli canary [--update] [--file <path>]`: resolves a fixed set of public videos (plus live and
//! upcoming streams discovered at run time) through the client ring, proves each one is really
//! fetchable and compares the outcome with the expectations in `config/canary.json`.
//!
//! VOD: the first 64 KiB of the chosen video format must come back as HTTP 206.
//! Live: the HLS master playlist must come back as HTTP 200 starting with `#EXTM3U`.
//! Exit status is non-zero when a video expected to play no longer does. `--update` rewrites the
//! expectations of the fixed entries to what was observed.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Instant;
use yt_core::model::ItemKind;
use yt_core::search_filters::{Filters, Kind};
use yt_core::session::Session;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Playable,
    /// Not playable, and that is expected (upcoming, some age-restricted videos).
    Unplayable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    id: String,
    category: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    title: String,
    expect: Outcome,
}

/// Videos found at run time by a search (live streams and premieres rotate too fast to pin).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Discover {
    category: String,
    #[serde(default)]
    queries: Vec<String>,
    /// Browse ids scanned after the searches (TV search never lists upcoming streams; the
    /// YouTube "Live" destination channel has an "Upcoming Live Streams" shelf).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    browse: Vec<String>,
    /// Restrict the search to live results (`search_filters` Live feature).
    #[serde(default)]
    live_filter: bool,
    /// Item kind to keep: "live" or "upcoming".
    kind: String,
    count: usize,
    expect: Outcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Canary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    comment: Option<String>,
    videos: Vec<Entry>,
    #[serde(default)]
    discover: Vec<Discover>,
}

struct Row {
    id: String,
    category: String,
    client: String,
    outcome: Outcome,
    detail: String,
    expect: Outcome,
    ms: u128,
}

impl Row {
    fn verdict(&self) -> &'static str {
        match (self.expect, self.outcome) {
            (Outcome::Playable, Outcome::Unplayable) => "REGRESSION",
            (Outcome::Unplayable, Outcome::Playable) => "improved",
            _ => "ok",
        }
    }
}

fn default_file() -> PathBuf {
    let local = PathBuf::from("config/canary.json");
    if local.exists() {
        return local;
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/canary.json")
}

pub fn run(args: &[String]) -> Result<()> {
    let update = args.iter().any(|a| a == "--update");
    let file = match args.iter().position(|a| a == "--file") {
        Some(i) => PathBuf::from(args.get(i + 1).context("--file needs a path")?),
        None => default_file(),
    };
    let text = std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    let mut canary: Canary = serde_json::from_str(&text).with_context(|| format!("parsing {}", file.display()))?;

    let mut s = Session::new()?;
    eprintln!(
        "[canary] {} fixed videos, {} discovery rules, signed in: {}",
        canary.videos.len(),
        canary.discover.len(),
        s.signed_in()
    );

    let mut rows = Vec::new();
    for e in &canary.videos {
        rows.push(check(&mut s, &e.id, &e.category, e.expect));
    }
    let fixed: std::collections::HashSet<String> = canary.videos.iter().map(|e| e.id.clone()).collect();
    for d in &canary.discover {
        let found = discover(&mut s, d, &fixed);
        if found.len() < d.count {
            eprintln!("[canary] {}: found {} of {} wanted", d.category, found.len(), d.count);
        }
        for id in found {
            rows.push(check(&mut s, &id, &d.category, d.expect));
        }
    }
    if let Some(src) = yt_core::clients::active_source() {
        eprintln!("[canary] client profiles: {src:?}");
    }

    print_table(&rows);
    let regressions = rows.iter().filter(|r| r.verdict() == "REGRESSION").count();
    let improved = rows.iter().filter(|r| r.verdict() == "improved").count();
    println!(
        "\n{} checked: {} playable, {} unplayable, {regressions} regression(s), {improved} improvement(s)",
        rows.len(),
        rows.iter().filter(|r| r.outcome == Outcome::Playable).count(),
        rows.iter().filter(|r| r.outcome == Outcome::Unplayable).count(),
    );

    if update {
        let mut changed = 0;
        for e in &mut canary.videos {
            if let Some(r) = rows.iter().find(|r| r.id == e.id) {
                if r.outcome != e.expect {
                    e.expect = r.outcome;
                    changed += 1;
                }
            }
        }
        std::fs::write(&file, serde_json::to_string_pretty(&canary)? + "\n")?;
        println!("updated {} ({changed} expectation(s) changed)", file.display());
        return Ok(());
    }
    if regressions > 0 {
        bail!("{regressions} canary regression(s)");
    }
    Ok(())
}

/// Search-driven discovery of live / upcoming videos.
fn discover(s: &mut Session, d: &Discover, skip: &std::collections::HashSet<String>) -> Vec<String> {
    let want = match d.kind.as_str() {
        "live" => ItemKind::Live,
        "upcoming" => ItemKind::Upcoming,
        other => {
            eprintln!("[canary] unknown discover kind {other:?}");
            return Vec::new();
        }
    };
    let params = d.live_filter.then(|| Filters { live: true, kind: Kind::Video, ..Default::default() }.params()).flatten();
    let mut out: Vec<String> = Vec::new();
    let sources = d.queries.iter().map(|q| (true, q)).chain(d.browse.iter().map(|b| (false, b)));
    for (is_search, src) in sources {
        if out.len() >= d.count {
            break;
        }
        let page = if is_search { s.search(src, params.as_deref()) } else { s.browse(src, None) };
        match page {
            Ok(page) => {
                for it in page.shelves.iter().flat_map(|sh| sh.items.iter()) {
                    if it.kind == want && !skip.contains(&it.id) && !out.contains(&it.id) && out.len() < d.count {
                        out.push(it.id.clone());
                    }
                }
            }
            Err(e) => eprintln!("[canary] {}: {src:?}: {e:#}", if is_search { "search" } else { "browse" }),
        }
    }
    out
}

fn check(s: &mut Session, id: &str, category: &str, expect: Outcome) -> Row {
    let t = Instant::now();
    let (client, res) = match s.resolve(id) {
        Ok(r) => {
            let client = match r.client {
                yt_core::innertube::Client::Profile(p) => p.key.clone(),
                c => format!("{c:?}"),
            };
            (client, verify(&s.http, &r))
        }
        Err(e) => ("-".into(), Err(e)),
    };
    let (outcome, detail) = match res {
        Ok(d) => (Outcome::Playable, d),
        Err(e) => (Outcome::Unplayable, short_reason(&format!("{e:#}"))),
    };
    let row = Row { id: id.into(), category: category.into(), client, outcome, detail, expect, ms: t.elapsed().as_millis() };
    eprintln!("[canary] {} {:?} {} ({} ms)", row.id, row.outcome, row.detail, row.ms);
    row
}

/// Fetches 64 KiB of the chosen video format, or the HLS master playlist for live streams.
fn verify(http: &reqwest::blocking::Client, r: &yt_core::streams::Resolved) -> Result<String> {
    let ua = r.client.user_agent();
    if r.is_upcoming {
        bail!("upcoming");
    }
    if r.is_live {
        let url = r.hls.as_deref().context("live without HLS manifest")?;
        let resp = http.get(url).header("User-Agent", ua).send()?;
        let status = resp.status().as_u16();
        let body = resp.text()?;
        if status != 200 || !body.starts_with("#EXTM3U") {
            bail!("HLS HTTP {status}");
        }
        let variants = body.lines().filter(|l| l.starts_with("#EXT-X-STREAM-INF")).count();
        return Ok(format!("live HLS 200, {variants} variants"));
    }
    let f = pick_video(&r.adaptive).context("no direct video format")?;
    let url = f["url"].as_str().unwrap_or_default();
    let resp = http.get(url).header("User-Agent", ua).header("Range", "bytes=0-65535").send()?;
    let status = resp.status().as_u16();
    let len = resp.bytes()?.len();
    if status != 206 {
        bail!("range HTTP {status} (itag {})", f["itag"]);
    }
    let max_h = r.adaptive.iter().filter_map(|f| f["height"].as_u64()).max().unwrap_or(0);
    Ok(format!("206 {len}B itag {} {}p (max {max_h}p)", f["itag"], f["height"]))
}

/// Same preference as the player: H.264 up to 1080p, then any direct video format.
fn pick_video(formats: &[Value]) -> Option<&Value> {
    let direct = |f: &&Value| f["url"].is_string() && f["mimeType"].as_str().is_some_and(|m| m.starts_with("video/"));
    formats
        .iter()
        .filter(direct)
        .filter(|f| f["mimeType"].as_str().is_some_and(|m| m.contains("avc1")) && f["height"].as_u64().unwrap_or(0) <= 1080)
        .max_by_key(|f| f["height"].as_u64().unwrap_or(0))
        .or_else(|| formats.iter().find(direct))
}

/// First client's reason, trimmed for the table.
fn short_reason(e: &str) -> String {
    let e = e.split_once(": ").map(|(_, r)| r).unwrap_or(e);
    let first = e.split("; ").next().unwrap_or(e);
    first.chars().take(60).collect()
}

fn print_table(rows: &[Row]) {
    println!(
        "\n| {:<11} | {:<14} | {:<10} | {:<10} | {:<10} | {:<10} | {:>6} | detail",
        "id", "category", "client", "result", "expected", "verdict", "ms"
    );
    println!("|{}|", ["-".repeat(13), "-".repeat(16), "-".repeat(12), "-".repeat(12), "-".repeat(12), "-".repeat(12), "-".repeat(8), "-".repeat(8)].join("|"));
    let name = |o: Outcome| match o {
        Outcome::Playable => "playable",
        Outcome::Unplayable => "unplayable",
    };
    for r in rows {
        println!(
            "| {:<11} | {:<14} | {:<10} | {:<10} | {:<10} | {:<10} | {:>6} | {}",
            r.id,
            r.category,
            r.client,
            name(r.outcome),
            name(r.expect),
            r.verdict(),
            r.ms,
            r.detail
        );
    }
}

use yt_core::{auth, bootstrap, innertube, mpd};

use anyhow::{bail, Context, Result};
use innertube::{Api, Client};
use serde_json::{json, Value};

const USAGE: &str = "\
ytcli - YouTube stream extraction proof of concept

USAGE:
  ytcli login               Sign in with a TV code (youtube.com/activate)
  ytcli whoami              Show the signed-in account
  ytcli subs                List the latest videos from your subscriptions
  ytcli streams <id|url>    Resolve + verify 1080p video/audio URLs and the HLS manifest
        [--sample <secs>]          ...and mux the first <secs> seconds into sample.mp4 (needs ffmpeg)
        [--tv]                     ...also report what the signed-in TV client returns
";

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let http = reqwest::blocking::Client::builder()
        .gzip(true)
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let cfg = bootstrap::fetch(&http).context("bootstrapping from youtube.com/tv")?;
    eprintln!(
        "[bootstrap] TVHTML5 {} | oauth client {} ({}) | visitorData: {}",
        cfg.tv_client_version,
        &cfg.client_id[..12],
        if cfg.scraped { "scraped" } else { "fallback" },
        if cfg.visitor_data.is_some() { "yes" } else { "no" },
    );
    let api = Api { http: &http, cfg: &cfg };

    match args.first().map(String::as_str) {
        Some("login") => {
            auth::device_login(&http, &cfg)?;
            println!("  Signed in. Token saved to token.json.");
            whoami(&api)
        }
        Some("whoami") => whoami(&api),
        Some("subs") => subs(&api),
        Some("streams") => {
            let id = video_id(args.get(1).context("missing video id")?)?;
            let sample = args
                .iter()
                .position(|a| a == "--sample")
                .map(|i| args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(20u32));
            streams(&api, &id, sample, args.iter().any(|a| a == "--tv"))
        }
        _ => {
            print!("{USAGE}");
            Ok(())
        }
    }
}

fn video_id(s: &str) -> Result<String> {
    let re = regex::Regex::new(r"(?:v=|youtu\.be/|shorts/|live/|^)([A-Za-z0-9_-]{11})(?:$|[&?#/])")?;
    Ok(re.captures(s).context("can't parse video id")?[1].to_string())
}

fn token(api: &Api) -> Result<auth::Token> {
    auth::load(api.http)?.context("not signed in - run `ytcli login` first")
}

fn whoami(api: &Api) -> Result<()> {
    let t = token(api)?;
    let r = api.call(Client::Tv, "account/accounts_list", json!({}), Some(&t.access_token))?;
    let name = innertube::find(&r, "accountName").and_then(innertube::text);
    let handle = innertube::find(&r, "channelHandle").and_then(innertube::text);
    match name {
        Some(n) => println!("Signed in as: {n} {}", handle.unwrap_or_default()),
        None => bail!("authenticated call returned no account: {}", truncate(&r.to_string(), 400)),
    }
    Ok(())
}

fn subs(api: &Api) -> Result<()> {
    let t = token(api)?;
    let r = api.call(Client::Tv, "browse", json!({ "browseId": "FEsubscriptions" }), Some(&t.access_token))?;
    let mut tiles = Vec::new();
    innertube::video_tiles(&r, &mut tiles);
    tiles.dedup_by(|a, b| a.0 == b.0);
    if tiles.is_empty() {
        bail!("no videos parsed from subscriptions feed ({} bytes of JSON)", r.to_string().len());
    }
    println!("Subscriptions feed - {} videos on first page:", tiles.len());
    for (id, title) in tiles.iter().take(15) {
        println!("  {id}  {title}");
    }
    Ok(())
}

struct Picked {
    itag: u64,
    desc: String,
    url: String,
}

fn pick_video(formats: &[Value]) -> Option<Picked> {
    // H.264 first: every webOS generation can hardware-decode it. Highest height <= 1080, then fps.
    let score = |f: &Value| -> Option<(u64, u64, u64)> {
        let mime = f["mimeType"].as_str()?;
        if !mime.starts_with("video/") || f["url"].as_str().is_none() {
            return None;
        }
        let h = f["height"].as_u64()?;
        if h > 1080 {
            return None;
        }
        let codec_pref = if mime.contains("avc1") { 2 } else if mime.contains("vp9") { 1 } else { 0 };
        Some((h, codec_pref, f["fps"].as_u64().unwrap_or(0)))
    };
    let f = formats.iter().filter_map(|f| score(f).map(|s| (s, f))).max_by_key(|(s, _)| *s)?.1;
    Some(Picked {
        itag: f["itag"].as_u64()?,
        desc: format!(
            "{}x{} {}fps {} {}kbps",
            f["width"], f["height"], f["fps"], f["mimeType"].as_str()?, f["bitrate"].as_u64()? / 1000
        ),
        url: f["url"].as_str()?.into(),
    })
}

fn pick_audio(formats: &[Value]) -> Option<Picked> {
    let f = formats
        .iter()
        .filter(|f| {
            f["mimeType"].as_str().is_some_and(|m| m.starts_with("audio/mp4"))
                && f["url"].is_string()
                && f["isDrc"].as_bool() != Some(true)
                && f["audioTrack"]["audioIsDefault"].as_bool() != Some(false)
        })
        .max_by_key(|f| f["bitrate"].as_u64().unwrap_or(0))?;
    Some(Picked {
        itag: f["itag"].as_u64()?,
        desc: format!("{} {}kbps", f["mimeType"].as_str()?, f["bitrate"].as_u64()? / 1000),
        url: f["url"].as_str()?.into(),
    })
}

/// Fetches the first 64 KiB of a stream URL and checks it really is media.
fn verify(http: &reqwest::blocking::Client, ua: &str, url: &str) -> Result<String> {
    let r = http.get(url).header("User-Agent", ua).header("Range", "bytes=0-65535").send()?;
    let status = r.status();
    let total = r
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.rsplit('/').next())
        .and_then(|s| s.parse::<u64>().ok());
    let body = r.bytes()?;
    if !status.is_success() {
        bail!("HTTP {status}");
    }
    let kind = if body.len() > 8 && &body[4..8] == b"ftyp" { "MP4 (ftyp box)" } else { "unknown container" };
    Ok(format!(
        "HTTP {} - {} - {:.1} MiB total",
        status.as_u16(),
        kind,
        total.unwrap_or(0) as f64 / 1048576.0
    ))
}

fn streams(api: &Api, id: &str, sample: Option<u32>, tv: bool) -> Result<()> {
    let pr = api.player(Client::VisionOs, id, None)?;
    let status = pr["playabilityStatus"]["status"].as_str().unwrap_or("?");
    let details = &pr["videoDetails"];
    println!(
        "Video:  {} - {} ({}s)\nStatus: {status} {}",
        details["title"].as_str().unwrap_or("?"),
        details["author"].as_str().unwrap_or("?"),
        details["lengthSeconds"].as_str().unwrap_or("?"),
        pr["playabilityStatus"]["reason"].as_str().unwrap_or("")
    );
    if status != "OK" {
        bail!("not playable via VISIONOS client");
    }
    let sd = &pr["streamingData"];
    let formats: Vec<Value> = sd["adaptiveFormats"].as_array().cloned().unwrap_or_default();
    let ciphered = formats.iter().filter(|f| f["signatureCipher"].is_string()).count();
    let n_param = formats.iter().filter(|f| f["url"].as_str().is_some_and(|u| u.contains("&n="))).count();
    println!(
        "Formats: {} adaptive ({} need signature deciphering, {} carry an 'n' challenge param)",
        formats.len(),
        ciphered,
        n_param
    );

    let v = pick_video(&formats).context("no usable video format")?;
    let a = pick_audio(&formats).context("no usable audio format")?;
    let ua = Client::VisionOs.user_agent();
    println!("\nVideo itag {}: {}\n  verify: {}", v.itag, v.desc, verify(api.http, ua, &v.url)?);
    println!("Audio itag {}: {}\n  verify: {}", a.itag, a.desc, verify(api.http, ua, &a.url)?);
    if let Some(h) = sd["hlsManifestUrl"].as_str() {
        let m = api.http.get(h).header("User-Agent", ua).send()?.text()?;
        let variants = m.lines().filter(|l| l.starts_with("#EXT-X-STREAM-INF")).count();
        let max_res = regex::Regex::new(r"RESOLUTION=\d+x(\d+)")?
            .captures_iter(&m)
            .filter_map(|c| c[1].parse::<u32>().ok())
            .max();
        println!("HLS manifest: {variants} variants, up to {}p", max_res.unwrap_or(0));
    }
    if let Some(exp) = sd["expiresInSeconds"].as_str() {
        println!("URLs expire in {}h", exp.parse::<u64>().unwrap_or(0) / 3600);
    }
    std::fs::write("last_streams.txt", format!("{}\n{}\n{}\n", v.url, a.url, sd["hlsManifestUrl"].as_str().unwrap_or("")))?;
    println!("(full URLs written to last_streams.txt: video, audio, hls)");
    let dur = formats.iter().find_map(|f| f["approxDurationMs"].as_str()?.parse::<u64>().ok()).unwrap_or(0);
    if let Some((mpd, nv, na)) = mpd::build(&formats, dur, "avc1", 1080) {
        std::fs::write("last.mpd", mpd)?;
        println!("DASH manifest written to last.mpd ({nv} H.264 video + {na} AAC audio representations)");
    }

    if tv {
        tv_report(api, id)?;
    }
    if let Some(secs) = sample {
        mux_sample(ua, &v.url, &a.url, secs)?;
    }
    Ok(())
}

/// What the signed-in TV client returns for the same video - tells us how much JS deobfuscation a
/// fully logged-in playback path would need.
fn tv_report(api: &Api, id: &str) -> Result<()> {
    let t = auth::load(api.http)?;
    let pr = api.player(Client::Tv, id, t.as_ref().map(|t| t.access_token.as_str()))?;
    let formats: Vec<Value> = pr["streamingData"]["adaptiveFormats"].as_array().cloned().unwrap_or_default();
    let direct = formats.iter().filter(|f| f["url"].is_string()).count();
    let ciphered = formats.iter().filter(|f| f["signatureCipher"].is_string()).count();
    let has_n = formats.iter().any(|f| {
        f["url"].as_str().or(f["signatureCipher"].as_str()).is_some_and(|u| u.contains("n%3D") || u.contains("&n="))
    });
    println!(
        "\nTVHTML5 ({}): status {} | {} formats: {direct} direct, {ciphered} signatureCipher, n-challenge: {has_n}",
        if t.is_some() { "signed in" } else { "anonymous" },
        pr["playabilityStatus"]["status"].as_str().unwrap_or("?"),
        formats.len()
    );
    Ok(())
}

fn mux_sample(ua: &str, video: &str, audio: &str, secs: u32) -> Result<()> {
    println!("\nMuxing first {secs}s into sample.mp4 with ffmpeg...");
    let st = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-user_agent", ua, "-i", video, "-user_agent", ua, "-i", audio])
        .args(["-t", &secs.to_string(), "-map", "0:v", "-map", "1:a", "-c", "copy", "sample.mp4"])
        .status()
        .context("running ffmpeg")?;
    if !st.success() {
        bail!("ffmpeg failed");
    }
    println!("Wrote sample.mp4");
    Ok(())
}

fn truncate(s: &str, n: usize) -> &str {
    &s[..s.char_indices().nth(n).map_or(s.len(), |(i, _)| i)]
}

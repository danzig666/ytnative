//! Live SABR tests against real YouTube (ignored by default; they need network, and the signed-in
//! one needs `token.json` in the repo root). Run with:
//!
//! ```text
//! cargo test -p yt-core sabr::live -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Each test downloads ~20 s of 1080p H.264 + AAC through SABR into `target/sabr/<name>/`
//! (`video.mp4`, `audio.m4a`: init + media segments concatenated) and checks them with `ffprobe`.
//! Override the video with `SABR_VIDEO=<id>`.
//!
//! Results 2026-11-12 (aqz-KE-bpKQ): `live_visionos_anonymous` OK (itag 299 1080p60 H.264 + 140
//! AAC, 27.4 s / 30.0 s in 3 requests, 0.6 s; seek and end detection OK); `live_visionos_vp9_opus`
//! OK (303 + 251); `live_player_mode_pacing` OK (server pauses at ~27 s readahead with a 2 s
//! backoff, resumes when the playhead moves); `live_tv_signed_in` FAILS with HTTP 403 (see the
//! table in `sabr/mod.rs`); `live_matrix*` print the per-client table.

use super::*;
use crate::bootstrap;
use crate::innertube::{Api, Client};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Instant;

const DEFAULT_VIDEO: &str = "aqz-KE-bpKQ"; // Big Buck Bunny (has 1080p H.264 + AAC)
const WANT_MS: i64 = 20_000;

fn video_id() -> String {
    std::env::var("SABR_VIDEO").unwrap_or_else(|_| DEFAULT_VIDEO.into())
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(60)).build().unwrap()
}

fn out_dir(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/sabr").join(name);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Checks a file with ffprobe/ffmpeg: returns (media duration in s from the packet timestamps,
/// "codec WxH"). The container header carries the whole video's duration (mehd), so the real
/// downloaded duration comes from the last packet. Also fully decodes it and fails on any error.
fn probe(path: &std::path::Path) -> Result<(f64, String)> {
    let run = |args: &[&str]| -> Result<Vec<u8>> {
        let o = std::process::Command::new(args[0]).args(&args[1..]).arg(path).output().with_context(|| format!("running {}", args[0]))?;
        if !o.status.success() {
            bail!("{} failed on {}: {}", args[0], path.display(), String::from_utf8_lossy(&o.stderr));
        }
        Ok(o.stdout)
    };
    let v: Value = serde_json::from_slice(&run(&[
        "ffprobe", "-v", "error", "-select_streams", "0", "-show_entries", "stream=codec_name,width,height:packet=pts_time,duration_time", "-of", "json",
    ])?)?;
    let f = |p: &Value, k: &str| p[k].as_str().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
    let packets = v["packets"].as_array().cloned().unwrap_or_default();
    let first = packets.iter().map(|p| f(p, "pts_time")).fold(f64::MAX, f64::min);
    let last = packets.iter().map(|p| f(p, "pts_time") + f(p, "duration_time")).fold(0.0, f64::max);
    let s = &v["streams"][0];
    let desc = format!("{} {}x{} ({} packets)", s["codec_name"].as_str().unwrap_or("?"), s["width"], s["height"], packets.len());
    // Full decode: ffmpeg prints decode errors at -v error.
    let o = std::process::Command::new("ffmpeg").args(["-v", "error", "-i"]).arg(path).args(["-f", "null", "-"]).output()?;
    // A partial WebM is "premature" by construction: its Segment element declares the whole file's
    // size. Anything else is a real decode problem.
    let stderr = String::from_utf8_lossy(&o.stderr);
    let problems: Vec<&str> = stderr.lines().filter(|l| !l.contains("File ended prematurely")).collect();
    if !o.status.success() || !problems.is_empty() {
        bail!("ffmpeg decode of {} reported: {}", path.display(), problems.join("\n"));
    }
    Ok((last - first.min(last), desc))
}

/// Picks 1080p H.264 + AAC, streams ≥ 20 s of each, writes and probes the files.
fn download(name: &str, http: reqwest::blocking::Client, pr: &Value, client: ClientInfo, ua: &str) -> Result<SabrStats> {
    download_codec(name, http, pr, client, ua, VideoCodec::H264, AudioCodec::Aac)
}

fn download_codec(
    name: &str,
    http: reqwest::blocking::Client,
    pr: &Value,
    client: ClientInfo,
    ua: &str,
    vc: VideoCodec,
    ac: AudioCodec,
) -> Result<SabrStats> {
    let adaptive = pr["streamingData"]["adaptiveFormats"].as_array().context("no adaptiveFormats")?;
    let with_url = adaptive.iter().filter(|f| f["url"].is_string()).count();
    let v = best_video(adaptive, vc, 1080).with_context(|| format!("no {vc:?} video"))?;
    let a = best_audio(adaptive, ac).with_context(|| format!("no {ac:?} audio"))?;
    let (vi, ai) = (v["itag"].as_i64().unwrap() as i32, a["itag"].as_i64().unwrap() as i32);
    println!(
        "[{name}] {} adaptive formats ({} with direct URLs); video itag {vi} {}p, audio itag {ai}",
        adaptive.len(),
        with_url,
        v["height"]
    );
    let mut s = SabrStream::from_player_response(http, pr, client, ua, Some(vi), Some(ai))?;
    let dir = out_dir(name);
    let (mut vbuf, mut abuf) = (Vec::new(), Vec::new());
    let (mut vend, mut aend) = (0i64, 0i64);
    let t0 = Instant::now();
    while (vend < WANT_MS || aend < WANT_MS) && !s.is_finished() {
        let segs = s.next_segments()?;
        for seg in segs {
            println!(
                "[{name}]   {:?} itag {} seq {} {}..{} ms {} B{}",
                seg.track,
                seg.itag,
                seg.sequence,
                seg.start_ms,
                seg.start_ms + seg.duration_ms,
                seg.data.len(),
                if seg.is_init { " (init)" } else { "" }
            );
            match seg.track {
                Track::Video => {
                    vbuf.extend_from_slice(&seg.data);
                    vend = vend.max(seg.start_ms + seg.duration_ms);
                }
                Track::Audio => {
                    abuf.extend_from_slice(&seg.data);
                    aend = aend.max(seg.start_ms + seg.duration_ms);
                }
            }
        }
    }
    println!("[{name}] got video to {vend} ms, audio to {aend} ms in {:?}; stats {:?}", t0.elapsed(), s.stats);
    let webm = vc != VideoCodec::H264;
    let (vp, ap) = if webm { (dir.join("video.webm"), dir.join("audio.webm")) } else { (dir.join("video.mp4"), dir.join("audio.m4a")) };
    let (want_v, want_a) = match (vc, ac) {
        (VideoCodec::H264, _) => ("h264", if ac == AudioCodec::Aac { "aac" } else { "opus" }),
        (VideoCodec::Vp9, _) => ("vp9", if ac == AudioCodec::Aac { "aac" } else { "opus" }),
        (VideoCodec::Av1, _) => ("av1", if ac == AudioCodec::Aac { "aac" } else { "opus" }),
    };
    std::fs::write(&vp, &vbuf)?;
    std::fs::write(&ap, &abuf)?;
    let (vd, vdesc) = probe(&vp)?;
    let (ad, adesc) = probe(&ap)?;
    println!("[{name}] ffprobe: {} -> {vdesc}, {vd:.2} s; {} -> {adesc}, {ad:.2} s", vp.display(), ap.display());
    if vd < (WANT_MS as f64 / 1000.0) - 1.0 || ad < (WANT_MS as f64 / 1000.0) - 1.0 {
        bail!("too short: video {vd} s, audio {ad} s");
    }
    if !vdesc.starts_with(want_v) || !vdesc.contains("1080") || !adesc.starts_with(want_a) {
        bail!("unexpected codecs: {vdesc} / {adesc}");
    }
    // The two tracks must also mux together (same as a player feeding both to one pipeline).
    let mux = std::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-i"])
        .arg(&vp)
        .arg("-i")
        .arg(&ap)
        .args(["-c", "copy"])
        .arg(dir.join(if webm { "muxed.mkv" } else { "muxed.mp4" }))
        .output()?;
    if !mux.status.success() {
        bail!("mux failed: {}", String::from_utf8_lossy(&mux.stderr));
    }

    // Seek: jump to 60 s and check the server continues there.
    s.seek(60_000);
    let segs = s.next_segments()?;
    let first = segs.iter().find(|x| !x.is_init).context("no media after seek")?;
    println!(
        "[{name}] after seek(60000): {} segments ({} init), first media {:?} seq {} at {} ms",
        segs.len(),
        segs.iter().filter(|x| x.is_init).count(),
        first.track,
        first.sequence,
        first.start_ms
    );
    if (first.start_ms - 60_000).abs() > 10_000 {
        bail!("seek landed at {} ms", first.start_ms);
    }

    // End detection: seek close to the end and read until the stream reports it is finished.
    let dur = s.cfg.duration_ms as i64;
    s.seek(dur - 15_000);
    let mut last = (0i64, 0i64);
    for _ in 0..20 {
        let segs = s.next_segments()?;
        if segs.is_empty() {
            break;
        }
        for x in segs.iter().filter(|x| !x.is_init) {
            last = last.max((x.start_ms + x.duration_ms, x.sequence));
        }
    }
    println!("[{name}] end: finished={} last media ends at {} ms (seq {}), duration {dur} ms", s.is_finished(), last.0, last.1);
    if !s.is_finished() || (last.0 - dur).abs() > 2_000 {
        bail!("end not detected");
    }
    // Once finished, more calls return nothing (EOF) without requests.
    let n = s.stats.requests;
    if !s.next_segments()?.is_empty() || s.stats.requests != n {
        bail!("activity after EOF");
    }
    Ok(s.stats.clone())
}

#[test]
#[ignore]
fn live_visionos_anonymous() {
    let http = http();
    let cfg = bootstrap::fetch(&http).unwrap();
    let api = Api { http: &http, cfg: &cfg };
    let pr = api.player(Client::VisionOs, &video_id(), None).unwrap();
    assert_eq!(pr["playabilityStatus"]["status"], "OK", "{}", pr["playabilityStatus"]);
    let client = ClientInfo::from_innertube(&Client::VisionOs.context(&cfg).0).unwrap();
    let stats = download("visionos", http.clone(), &pr, client, Client::VisionOs.user_agent()).unwrap();
    println!("VISIONOS anonymous OK: {stats:?}");
}

#[test]
#[ignore]
fn live_visionos_vp9_opus() {
    let http = http();
    let cfg = bootstrap::fetch(&http).unwrap();
    let api = Api { http: &http, cfg: &cfg };
    let pr = api.player(Client::VisionOs, &video_id(), None).unwrap();
    let client = ClientInfo::from_innertube(&Client::VisionOs.context(&cfg).0).unwrap();
    let stats = download_codec("visionos_vp9", http.clone(), &pr, client, Client::VisionOs.user_agent(), VideoCodec::Vp9, AudioCodec::Opus).unwrap();
    println!("VISIONOS VP9/Opus OK: {stats:?}");
}

#[test]
#[ignore]
fn live_tv_signed_in() {
    crate::paths::set_data_dir("C:/Download/prog/ytnative".into());
    let mut s = crate::session::Session::new().unwrap();
    assert!(s.signed_in(), "no token.json");
    let pr = s
        .raw(
            "player",
            json!({
                "videoId": video_id(),
                "playbackContext": { "contentPlaybackContext": { "html5Preference": "HTML5_PREF_WANTS" } },
                "contentCheckOk": true, "racyCheckOk": true,
            }),
        )
        .unwrap();
    assert_eq!(pr["playabilityStatus"]["status"], "OK", "{}", pr["playabilityStatus"]);
    let client = ClientInfo::from_innertube(&Client::Tv.context(&s.cfg).0).unwrap();
    match download("tv", s.http.clone(), &pr, client, Client::Tv.user_agent()) {
        Ok(stats) => println!("TVHTML5 signed-in OK: {stats:?}"),
        Err(e) => panic!("TVHTML5 signed-in SABR failed: {e:#}"),
    }
}

/// Player mode: the playhead stays at 0, so once the readahead target is buffered the server
/// answers without media (pacing). `next_segments` must wait it out, and resume when the playhead
/// moves, instead of treating it as EOF.
#[test]
#[ignore]
fn live_player_mode_pacing() {
    let http = http();
    let cfg = bootstrap::fetch(&http).unwrap();
    let api = Api { http: &http, cfg: &cfg };
    let pr = api.player(Client::VisionOs, &video_id(), None).unwrap();
    let adaptive = pr["streamingData"]["adaptiveFormats"].as_array().unwrap();
    let vi = best_video(adaptive, VideoCodec::H264, 720).unwrap()["itag"].as_i64().unwrap() as i32;
    let ai = best_audio(adaptive, AudioCodec::Aac).unwrap()["itag"].as_i64().unwrap() as i32;
    let client = ClientInfo::from_innertube(&Client::VisionOs.context(&cfg).0).unwrap();
    let mut s = SabrStream::from_player_response(http.clone(), &pr, client, Client::VisionOs.user_agent(), Some(vi), Some(ai)).unwrap();
    s.set_player_time(Some(0));
    let mut paced = 0;
    for round in 0..15 {
        let segs = s.poll_segments().unwrap();
        let v = s.buffered_until(Track::Video).unwrap();
        let a = s.buffered_until(Track::Audio).unwrap();
        println!(
            "poll {round}: {} segments, buffered video {v} ms audio {a} ms, next backoff {} ms",
            segs.len(),
            s.pending_backoff_ms()
        );
        if segs.is_empty() {
            assert!(!s.is_finished());
            paced += 1;
            if paced == 2 {
                break;
            }
        }
    }
    assert_eq!(paced, 2, "server never paced");
    let ahead = s.buffered_until(Track::Video).unwrap().min(s.buffered_until(Track::Audio).unwrap());
    println!("readahead held at playhead 0: {ahead} ms; stats {:?}", s.stats);
    // Move the playhead near the buffered end: delivery must resume.
    s.set_player_time(Some(ahead - 2000));
    let segs = s.next_segments().unwrap();
    let new_end = s.buffered_until(Track::Video).unwrap().max(s.buffered_until(Track::Audio).unwrap());
    println!("after playhead -> {} ms: {} more segments, buffered to {new_end} ms", ahead - 2000, segs.len());
    assert!(!segs.is_empty() && new_end > ahead);
}

/// Other InnerTube clients (not in `innertube::Client`), probed with raw `/player` calls:
/// (label, context.client JSON, X-YouTube-Client-Name, User-Agent).
fn extra_clients(visitor: &str) -> Vec<(&'static str, Value, &'static str, String)> {
    let ios_ua = "com.google.ios.youtube/20.10.4 (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)".to_string();
    let vr_ua = "com.google.android.apps.youtube.vr.oculus/1.65.10 (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip".to_string();
    let tv_ua = crate::bootstrap::TV_UA.to_string();
    vec![
        (
            "IOS",
            json!({"clientName":"IOS","clientVersion":"20.10.4","deviceMake":"Apple","deviceModel":"iPhone16,2","osName":"iPhone","osVersion":"18.3.2.22D82","userAgent":ios_ua,"hl":"en","gl":"US","visitorData":visitor}),
            "5",
            ios_ua.clone(),
        ),
        (
            "ANDROID_VR",
            json!({"clientName":"ANDROID_VR","clientVersion":"1.65.10","deviceMake":"Oculus","deviceModel":"Quest 3","androidSdkVersion":32,"osName":"Android","osVersion":"12L","userAgent":vr_ua,"hl":"en","gl":"US","visitorData":visitor}),
            "28",
            vr_ua.clone(),
        ),
        (
            "TVHTML5_SIMPLY",
            json!({"clientName":"TVHTML5_SIMPLY","clientVersion":"1.0","userAgent":tv_ua,"hl":"en","gl":"US","visitorData":visitor}),
            "75",
            tv_ua.clone(),
        ),
        (
            "TVHTML5_SIMPLY_EMBEDDED_PLAYER",
            json!({"clientName":"TVHTML5_SIMPLY_EMBEDDED_PLAYER","clientVersion":"2.0","userAgent":tv_ua,"hl":"en","gl":"US","visitorData":visitor}),
            "85",
            tv_ua.clone(),
        ),
    ]
}

fn raw_player(http: &reqwest::blocking::Client, ctx: &Value, id: &str, ua: &str, visitor: &str, bearer: Option<&str>) -> Result<Value> {
    let mut body = json!({
        "videoId": video_id(),
        "playbackContext": { "contentPlaybackContext": { "html5Preference": "HTML5_PREF_WANTS" } },
        "contentCheckOk": true, "racyCheckOk": true,
        "context": { "client": ctx },
    });
    if ctx["clientName"] == "TVHTML5_SIMPLY_EMBEDDED_PLAYER" {
        body["context"]["thirdParty"] = json!({ "embedUrl": "https://www.youtube.com/" });
    }
    let mut req = http
        .post("https://www.youtube.com/youtubei/v1/player?prettyPrint=false")
        .header("User-Agent", ua)
        .header("X-YouTube-Client-Name", id)
        .header("X-YouTube-Client-Version", ctx["clientVersion"].as_str().unwrap())
        .header("X-Goog-Visitor-Id", visitor)
        .header("Origin", "https://www.youtube.com")
        .json(&body);
    if let Some(t) = bearer {
        req = req.bearer_auth(t);
    }
    let r = req.send()?;
    let st = r.status();
    let v: Value = r.json()?;
    if !st.is_success() {
        bail!("HTTP {st}: {}", v["error"]["message"]);
    }
    Ok(v)
}

/// SABR delivery check for the extra clients, anonymous and signed in.
#[test]
#[ignore]
fn live_matrix_extra_clients() {
    crate::paths::set_data_dir("C:/Download/prog/ytnative".into());
    let http = http();
    let cfg = bootstrap::fetch(&http).unwrap();
    let visitor = cfg.visitor_data.clone().unwrap_or_default();
    let token = crate::auth::load(&http).unwrap().map(|t| t.access_token);
    for (label, ctx, id, ua) in extra_clients(&visitor) {
        for auth in [false, true] {
            if auth && token.is_none() {
                continue;
            }
            let outcome = (|| -> Result<String> {
                let pr = raw_player(&http, &ctx, id, &ua, &visitor, if auth { token.as_deref() } else { None })?;
                let st = pr["playabilityStatus"]["status"].as_str().unwrap_or("?").to_string();
                let adaptive = pr["streamingData"]["adaptiveFormats"].as_array().cloned().unwrap_or_default();
                let urls = adaptive.iter().filter(|f| f["url"].is_string()).count();
                let head = format!("status={st} formats={} urls={urls} sabr={}", adaptive.len(), pr["streamingData"]["serverAbrStreamingUrl"].is_string());
                if st != "OK" || adaptive.is_empty() {
                    return Ok(format!("{head} reason={}", pr["playabilityStatus"]["reason"]));
                }
                let v = best_video(&adaptive, VideoCodec::H264, 1080).context("no h264")?;
                let a = best_audio(&adaptive, AudioCodec::Aac).context("no aac")?;
                let ci = ClientInfo::from_innertube(&ctx).context("client name")?;
                let mut s = SabrStream::from_player_response(
                    http.clone(),
                    &pr,
                    ci,
                    &ua,
                    v["itag"].as_i64().map(|i| i as i32),
                    a["itag"].as_i64().map(|i| i as i32),
                )?;
                s.max_empty_responses = 3;
                let r = s.next_segments();
                Ok(match r {
                    Ok(segs) => format!("{head} -> {} segments, {} B, protection {}", segs.len(), s.stats.bytes, s.stats.protection_status),
                    Err(e) => format!("{head} -> ERR {e:#} (protection {})", s.stats.protection_status),
                })
            })();
            println!("{label} auth={auth}: {}", outcome.unwrap_or_else(|e| format!("ERR {e:#}")));
        }
    }
}

/// One SABR round per (client, auth, extra headers) combination; prints the outcome. Documents
/// which routes deliver media.
#[test]
#[ignore]
fn live_matrix() {
    crate::paths::set_data_dir("C:/Download/prog/ytnative".into());
    let http = http();
    let cfg = bootstrap::fetch(&http).unwrap();
    let token = crate::auth::load(&http).unwrap().map(|t| t.access_token);
    let api = Api { http: &http, cfg: &cfg };
    let clients: &[(Client, bool)] = &[(Client::VisionOs, false), (Client::VisionOs, true), (Client::Tv, false), (Client::Tv, true)];
    for &(client, auth) in clients {
        if auth && token.is_none() {
            continue;
        }
        let pr = match api.player(client, &video_id(), if auth { token.as_deref() } else { None }) {
            Ok(pr) => pr,
            Err(e) => {
                println!("{client:?} auth={auth}: /player error {e}");
                continue;
            }
        };
        let st = pr["playabilityStatus"]["status"].as_str().unwrap_or("?").to_string();
        let urls = pr["streamingData"]["adaptiveFormats"].as_array().map(|a| a.iter().filter(|f| f["url"].is_string()).count()).unwrap_or(0);
        let has_sabr = pr["streamingData"]["serverAbrStreamingUrl"].is_string();
        let header_sets: Vec<(&str, Vec<(String, String)>)> = vec![
            ("plain", vec![]),
            (
                "origin",
                vec![("Origin".into(), "https://www.youtube.com".into()), ("Referer".into(), "https://www.youtube.com/".into())],
            ),
            ("bearer", token.iter().map(|t| ("Authorization".to_string(), format!("Bearer {t}"))).collect()),
        ];
        for (hname, headers) in header_sets {
            let outcome = (|| -> Result<String> {
                let adaptive = pr["streamingData"]["adaptiveFormats"].as_array().context("no formats")?;
                let v = best_video(adaptive, VideoCodec::H264, 1080).context("no h264")?;
                let a = best_audio(adaptive, AudioCodec::Aac).context("no aac")?;
                let ci = ClientInfo::from_innertube(&client.context(&cfg).0).unwrap();
                let mut s = SabrStream::from_player_response(
                    http.clone(),
                    &pr,
                    ci,
                    client.user_agent(),
                    v["itag"].as_i64().map(|i| i as i32),
                    a["itag"].as_i64().map(|i| i as i32),
                )?;
                s.cfg.extra_headers = headers.clone();
                s.max_empty_responses = 3;
                let segs = s.next_segments()?;
                Ok(format!("{} segments, {} B, protection {}", segs.len(), s.stats.bytes, s.stats.protection_status))
            })();
            println!(
                "{client:?} auth={auth} status={st} urls={urls} sabr={has_sabr} headers={hname}: {}",
                match outcome {
                    Ok(s) => s,
                    Err(e) => format!("ERR {e:#}"),
                }
            );
        }
    }
}

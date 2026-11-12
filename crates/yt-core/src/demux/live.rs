//! Live demux checks against real YouTube formats (ignored by default; network + ffprobe/ffmpeg).
//!
//! ```text
//! cargo test -p yt-core demux::live -- --ignored --nocapture --test-threads=1
//! ```
//!
//! For each format: open it through [`RangedTrack`] (init + `sidx`/`Cues` via Range requests),
//! download the first ~10 s as one file, demux it, and compare with ffprobe's view of that same
//! file (packet count, every packet's PTS, keyframes). Then frame the units the way the Feed
//! engine does and write the elementary stream (`.h264` Annex-B, `.ivf` for VP9/AV1, `.aac`
//! ADTS), which ffprobe must count frame-for-frame and ffmpeg must decode without errors. Also
//! re-fetches one segment by index (the seek path) and checks it yields the same units.
//! `live_sabr_*` does the same with SABR segments. Output: `target/demux/<label>/`.
//! Override the video with `DEMUX_VIDEO=<id>`.

use super::framing::Framer;
use super::ranged::{fetch_range, RangedTrack};
use super::*;
use crate::bootstrap;
use crate::innertube::{Api, Client};
use crate::sabr::formats::by_itag;
use anyhow::{bail, Context};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_VIDEO: &str = "aqz-KE-bpKQ"; // Big Buck Bunny 60 fps 4K (H.264, VP9, AV1)
const WANT_NS: i64 = 10_000_000_000;

fn video_id() -> String {
    std::env::var("DEMUX_VIDEO").unwrap_or_else(|_| DEFAULT_VIDEO.into())
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(60)).build().unwrap()
}

fn out_dir(label: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/demux").join(label);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn player() -> (reqwest::blocking::Client, Value, bootstrap::TvConfig) {
    let http = http();
    let cfg = bootstrap::fetch(&http).unwrap();
    let pr = Api { http: &http, cfg: &cfg }.player(Client::VisionOs, &video_id(), None).unwrap();
    assert_eq!(pr["playabilityStatus"]["status"], "OK", "{}", pr["playabilityStatus"]);
    (http, pr, cfg)
}

fn run(cmd: &mut Command) -> anyhow::Result<Vec<u8>> {
    let o = cmd.output().with_context(|| format!("running {cmd:?}"))?;
    if !o.status.success() {
        bail!("{cmd:?} failed: {}", String::from_utf8_lossy(&o.stderr));
    }
    Ok(o.stdout)
}

struct Probe {
    pts_ms: Vec<f64>,
    keys: usize,
}

/// ffprobe's packets of stream 0: PTS (ms) and keyframe count.
fn ffprobe_packets(path: &Path) -> anyhow::Result<Probe> {
    let v: Value = serde_json::from_slice(&run(Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "0", "-show_entries", "packet=pts_time,flags", "-of", "json"])
        .arg(path))?)?;
    let mut pts_ms = Vec::new();
    let mut keys = 0;
    for p in v["packets"].as_array().cloned().unwrap_or_default() {
        if let Some(t) = p["pts_time"].as_str().and_then(|s| s.parse::<f64>().ok()) {
            pts_ms.push(t * 1000.0);
        }
        if p["flags"].as_str().is_some_and(|f| f.starts_with('K')) {
            keys += 1;
        }
    }
    Ok(Probe { pts_ms, keys })
}

/// ffprobe's decoded frame count + codec of an elementary stream file.
fn ffprobe_count(path: &Path) -> anyhow::Result<(u64, String)> {
    let v: Value = serde_json::from_slice(&run(Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-select_streams", "0", "-show_entries", "stream=codec_name,nb_read_frames,width,height", "-of", "json"])
        .arg(path))?)?;
    let s = &v["streams"][0];
    let n = s["nb_read_frames"].as_str().and_then(|x| x.parse().ok()).unwrap_or(0);
    Ok((n, format!("{} {}x{}", s["codec_name"].as_str().unwrap_or("?"), s["width"], s["height"])))
}

/// Full decode with ffmpeg; any error line fails (a partial WebM's "File ended prematurely" is
/// expected and ignored).
fn ffmpeg_decode_clean(path: &Path) -> anyhow::Result<()> {
    let o = Command::new("ffmpeg").args(["-v", "error", "-i"]).arg(path).args(["-f", "null", "-"]).output()?;
    let err = String::from_utf8_lossy(&o.stderr);
    let problems: Vec<&str> = err.lines().filter(|l| !l.contains("File ended prematurely")).collect();
    if !o.status.success() || !problems.is_empty() {
        bail!("ffmpeg decode of {}: {}", path.display(), problems.join(" | "));
    }
    Ok(())
}

fn ivf(fourcc: &[u8; 4], w: u32, h: u32, frames: &[(i64, Vec<u8>)]) -> Vec<u8> {
    let mut v = b"DKIF".to_vec();
    v.extend_from_slice(&0u16.to_le_bytes());
    v.extend_from_slice(&32u16.to_le_bytes());
    v.extend_from_slice(fourcc);
    v.extend_from_slice(&(w as u16).to_le_bytes());
    v.extend_from_slice(&(h as u16).to_le_bytes());
    v.extend_from_slice(&1000u32.to_le_bytes()); // timebase den (ms)
    v.extend_from_slice(&1u32.to_le_bytes());
    v.extend_from_slice(&(frames.len() as u32).to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    for (pts_ns, f) in frames {
        v.extend_from_slice(&(f.len() as u32).to_le_bytes());
        v.extend_from_slice(&((pts_ns / 1_000_000).max(0) as u64).to_le_bytes());
        v.extend_from_slice(f);
    }
    v
}

/// Compares our units with ffprobe's packets of the same file, then writes + checks the framed
/// elementary stream. Returns a one-line summary.
fn verify(label: &str, source: &Path, track: &TrackInfo, aus: &[AccessUnit]) -> anyhow::Result<String> {
    let dir = source.parent().unwrap().to_path_buf();
    let probe = ffprobe_packets(source)?;
    if probe.pts_ms.len() != aus.len() {
        bail!("{label}: demuxed {} units, ffprobe sees {} packets", aus.len(), probe.pts_ms.len());
    }
    let mut ours: Vec<f64> = aus.iter().map(|a| a.pts_ns as f64 / 1e6).collect();
    let mut theirs = probe.pts_ms.clone();
    ours.sort_by(f64::total_cmp);
    theirs.sort_by(f64::total_cmp);
    // Matroska timestamps are whole milliseconds; MP4 ones are exact.
    let worst = ours.iter().zip(&theirs).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
    if worst > 1.5 {
        bail!("{label}: PTS differ from ffprobe by up to {worst:.3} ms");
    }
    let keys = aus.iter().filter(|a| a.keyframe).count();
    // H.264: we only call IDRs keyframes; ffmpeg's parser also flags recovery-point I-frames
    // (SEI type 6 + non-IDR slice, seen in SABR fragments), which are not clean random-access
    // points for the TV's decoder.
    let keys_ok = match track.codec {
        Codec::H264 => keys >= 1 && keys <= probe.keys,
        _ => track.kind != Kind::Video || keys == probe.keys,
    };
    if !keys_ok {
        bail!("{label}: {keys} keyframes, ffprobe {}", probe.keys);
    }
    let framer = Framer::new(track)?;
    let span_ms = (ours.last().unwrap() - ours.first().unwrap()) + aus.last().map(|a| a.duration_ns as f64 / 1e6).unwrap_or(0.0);
    let mut line = format!("{label}: {:?} {}x{} {} units ({keys} key, ffprobe {}) {:.2} s, PTS match ffprobe (max diff {worst:.3} ms)", track.codec, track.width, track.height, aus.len(), probe.keys, span_ms / 1000.0);
    let es = match track.codec {
        Codec::H264 => {
            let p = dir.join("es.h264");
            std::fs::write(&p, aus.iter().flat_map(|a| framer.frame(a)).collect::<Vec<u8>>())?;
            Some(p)
        }
        Codec::Vp9 | Codec::Av1 => {
            let p = dir.join("es.ivf");
            let frames: Vec<(i64, Vec<u8>)> = aus.iter().map(|a| (a.pts_ns, framer.frame(a))).collect();
            let fourcc = if track.codec == Codec::Vp9 { b"VP90" } else { b"AV01" };
            std::fs::write(&p, ivf(fourcc, track.width, track.height, &frames))?;
            Some(p)
        }
        Codec::Aac => {
            let p = dir.join("es.aac");
            std::fs::write(&p, aus.iter().flat_map(|a| framer.frame(a)).collect::<Vec<u8>>())?;
            Some(p)
        }
        _ => None,
    };
    if let Some(p) = es {
        let (n, desc) = ffprobe_count(&p)?;
        if n != aus.len() as u64 {
            bail!("{label}: framed stream {} decodes {n} frames, expected {}", p.display(), aus.len());
        }
        ffmpeg_decode_clean(&p)?;
        line.push_str(&format!("; framed {} -> ffprobe {n} frames ({desc}), clean decode", p.file_name().unwrap().to_string_lossy()));
    } else {
        ffmpeg_decode_clean(source)?;
    }
    Ok(line)
}

fn check_ranged(label: &str, candidates: &[i32]) {
    let (http, pr, _) = player();
    let adaptive = pr["streamingData"]["adaptiveFormats"].as_array().cloned().unwrap_or_default();
    let Some(fmt) = candidates.iter().find_map(|&i| by_itag(&adaptive, i).filter(|f| f["url"].is_string())) else {
        panic!("{label}: none of itags {candidates:?} offered for {}", video_id());
    };
    let ua = Client::VisionOs.user_agent();
    let mut t = RangedTrack::open(http.clone(), fmt, ua).unwrap();
    let itag = t.itag;
    let info = t.track().clone();
    // Enough whole segments for WANT_NS.
    let n = t.segments.iter().position(|s| s.start_ns + s.duration_ns >= WANT_NS).map(|i| i + 1).unwrap_or(t.segments.len());
    let last = t.segments[n - 1];
    println!(
        "[{label}] itag {itag} {:?}: {} segments in index, first {:?}; fetching {n} ({} bytes)",
        t.container,
        t.segments.len(),
        t.segments[0],
        last.offset + last.size
    );
    let bytes = fetch_range(&http, &t.url, ua, 0, last.offset + last.size - 1).unwrap();
    let dir = out_dir(label);
    let source = dir.join(if t.container == Container::WebM { "source.webm" } else { "source.mp4" });
    std::fs::write(&source, &bytes).unwrap();
    // Linear demux of the whole file, in odd-sized chunks.
    let mut d = t.container.demuxer();
    let mut aus = Vec::new();
    for c in bytes.chunks(100_003) {
        d.push(c).unwrap();
        aus.extend(std::iter::from_fn(|| d.pop()));
    }
    let line = verify(label, &source, &info, &aus).unwrap_or_else(|e| panic!("{e:#}"));
    // The seek path: one segment by index must give the same units as the linear pass.
    let k = n / 2;
    let seg = t.fetch_segment(k).unwrap();
    let linear: Vec<(i64, usize)> = aus.iter().map(|a| (a.pts_ns, a.data.len())).collect();
    let fetched: Vec<(i64, usize)> = seg.iter().map(|a| (a.pts_ns, a.data.len())).collect();
    assert!(seg[0].keyframe, "{label}: segment {k} does not start with a keyframe");
    let at = linear.iter().position(|x| *x == fetched[0]).unwrap_or_else(|| panic!("{label}: segment {k}'s first unit not in the linear pass"));
    assert_eq!(&linear[at..at + fetched.len()], &fetched[..], "{label}: segment {k} by index vs linear pass");
    println!("[{label}] OK {line}; segment {k} by index: {} units, starts with a keyframe", seg.len());
}

#[test]
#[ignore]
fn live_h264() {
    check_ranged("h264", &[299, 137]);
}

#[test]
#[ignore]
fn live_vp9() {
    check_ranged("vp9", &[303, 248]);
}

#[test]
#[ignore]
fn live_vp9_2160() {
    check_ranged("vp9_2160", &[315, 313]);
}

#[test]
#[ignore]
fn live_av1() {
    check_ranged("av1", &[399, 401]);
}

#[test]
#[ignore]
fn live_av1_2160() {
    check_ranged("av1_2160", &[401]);
}

#[test]
#[ignore]
fn live_aac() {
    check_ranged("aac", &[140]);
}

#[test]
#[ignore]
fn live_opus() {
    check_ranged("opus", &[251]);
}

/// SABR segments (init + media) through the same demuxers.
fn check_sabr(label: &str, video_itag: i32, audio_itag: i32) {
    use crate::sabr::{ClientInfo, SabrStream, Track};
    let (http, pr, cfg) = player();
    let client = ClientInfo::from_innertube(&Client::VisionOs.context(&cfg).0).unwrap();
    let mut s = SabrStream::from_player_response(http, &pr, client, Client::VisionOs.user_agent(), Some(video_itag), Some(audio_itag)).unwrap();
    let (mut vbytes, mut abytes) = (Vec::new(), Vec::new());
    let (mut vd, mut ad): (Option<Box<dyn Demuxer>>, Option<Box<dyn Demuxer>>) = (None, None);
    let (mut vaus, mut aaus) = (Vec::new(), Vec::new());
    let (mut vend, mut aend) = (0i64, 0i64);
    while (vend < WANT_NS / 1_000_000 || aend < WANT_NS / 1_000_000) && !s.is_finished() {
        for seg in s.next_segments().unwrap() {
            let (bytes, d, aus, end) = match seg.track {
                Track::Video => (&mut vbytes, &mut vd, &mut vaus, &mut vend),
                Track::Audio => (&mut abytes, &mut ad, &mut aaus, &mut aend),
            };
            if seg.is_init {
                *d = Some(Container::sniff(&seg.data).expect("init segment container").demuxer());
            }
            let d = d.as_mut().expect("media before init");
            bytes.extend_from_slice(&seg.data);
            d.push(&seg.data).unwrap();
            aus.extend(std::iter::from_fn(|| d.pop()));
            *end = (*end).max(seg.start_ms + seg.duration_ms);
        }
    }
    let dir = out_dir(label);
    for (name, bytes, d, aus) in [("video", &vbytes, &vd, &vaus), ("audio", &abytes, &ad, &aaus)] {
        let d = d.as_ref().unwrap();
        let ext = if Container::sniff(bytes) == Some(Container::WebM) { "webm" } else { "mp4" };
        let sub = dir.join(name);
        std::fs::create_dir_all(&sub).unwrap();
        let p = sub.join(format!("source.{ext}"));
        std::fs::write(&p, bytes).unwrap();
        let line = verify(&format!("{label}/{name}"), &p, &d.tracks()[0], aus).unwrap_or_else(|e| panic!("{e:#}"));
        println!("[{label}] OK {line}");
    }
}

#[test]
#[ignore]
fn live_sabr_h264_opus() {
    check_sabr("sabr_h264_opus", 299, 251);
}

#[test]
#[ignore]
fn live_sabr_av1_aac() {
    check_sabr("sabr_av1_aac", 399, 140);
}

//! Builds a static DASH manifest (MPD) from InnerTube `adaptiveFormats`, the same approach
//! SmartTube uses to feed ExoPlayer. Each format is a single fragmented-MP4 file with a sidx
//! index, so a SegmentBase (initRange + indexRange) per Representation is all a DASH client needs.

use serde_json::Value;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// "https://h/videoplayback?a=1&b=x%2Fy" -> "https://h/videoplayback/a/1/b/x%2Fy".
/// googlevideo accepts parameters as path segments (its own HLS manifests use this form), and
/// webOS's GStreamer dashdemux2 drops the query string of a BaseURL.
fn path_style(url: &str) -> String {
    let Some((base, query)) = url.split_once('?') else { return url.to_string() };
    let mut out = base.trim_end_matches('/').to_string();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        // Values arrive percent-encoded already; '/' must stay encoded to keep segments intact.
        out.push('/');
        out.push_str(k);
        out.push('/');
        out.push_str(&v.replace('/', "%2F"));
    }
    out
}

fn range(f: &Value, key: &str) -> Option<String> {
    Some(format!("{}-{}", f[key]["start"].as_str()?, f[key]["end"].as_str()?))
}

fn codecs(mime: &str) -> Option<&str> {
    mime.split("codecs=\"").nth(1)?.split('"').next()
}

/// `video_codec` is a prefix such as "avc1"; the ladder is limited to `min_height..=max_height`.
///
/// The floor matters on webOS: the firmware's DASH adaptation starts at the lowest rung and climbs
/// slowly (144p for ~10 s on the C3), so offering only the rungs we want makes playback start sharp.
/// If the video has nothing that tall, the floor drops to its best rung under the cap.
pub fn build(formats: &[Value], duration_ms: u64, video_codec: &str, min_height: u64, max_height: u64) -> Option<(String, usize, usize)> {
    build_with_audio(formats, duration_ms, video_codec, min_height, max_height, None)
}

/// As [`build`], with a chosen audio track (`audioTrack.id`, e.g. "en.4" / "de.3") for videos
/// that have several languages; `None` keeps the default track.
pub fn build_with_audio(
    formats: &[Value],
    duration_ms: u64,
    video_codec: &str,
    min_height: u64,
    max_height: u64,
    audio_track: Option<&str>,
) -> Option<(String, usize, usize)> {
    let built = build_inner(formats, duration_ms, video_codec, min_height, max_height, audio_track);
    // A track id that matches nothing here (kept from another video): the default track.
    if built.is_none() && audio_track.is_some() {
        return build_inner(formats, duration_ms, video_codec, min_height, max_height, None);
    }
    built
}

/// A video format's quality rung: its short side (a vertical 1080p video is 1080x1920).
pub fn rung(f: &Value) -> u64 {
    let h = f["height"].as_u64().unwrap_or(0);
    match f["width"].as_u64() {
        Some(w) if w > 0 => w.min(h),
        _ => h,
    }
}

fn build_inner(
    formats: &[Value],
    duration_ms: u64,
    video_codec: &str,
    min_height: u64,
    max_height: u64,
    audio_track: Option<&str>,
) -> Option<(String, usize, usize)> {
    // The tallest rung under the cap among the formats this manifest can use (a direct URL and
    // both ranges).
    let best = formats
        .iter()
        .filter(|f| f["url"].is_string() && range(f, "initRange").is_some() && range(f, "indexRange").is_some())
        .filter(|f| f["mimeType"].as_str().is_some_and(|m| m.starts_with("video/mp4") && codecs(m).is_some_and(|c| c.starts_with(video_codec))))
        .map(rung)
        .filter(|&h| h <= max_height)
        .max()
        .unwrap_or(0);
    let min_height = min_height.min(best);
    let mut video = String::new();
    let mut audio = String::new();
    let (mut nv, mut na) = (0, 0);
    for f in formats {
        let (Some(mime), Some(url)) = (f["mimeType"].as_str(), f["url"].as_str()) else { continue };
        let (Some(init), Some(index)) = (range(f, "initRange"), range(f, "indexRange")) else { continue };
        let Some(c) = codecs(mime) else { continue };
        let itag = f["itag"].as_u64().unwrap_or(0);
        let bw = f["bitrate"].as_u64().unwrap_or(0);
        let seg = format!(
            "<BaseURL>{}</BaseURL><SegmentBase indexRange=\"{index}\"><Initialization range=\"{init}\"/></SegmentBase>",
            esc(&path_style(url))
        );
        if mime.starts_with("video/mp4") && c.starts_with(video_codec) {
            let h = f["height"].as_u64().unwrap_or(0);
            if rung(f) > max_height || rung(f) < min_height {
                continue;
            }
            video += &format!(
                "<Representation id=\"{itag}\" codecs=\"{c}\" bandwidth=\"{bw}\" width=\"{}\" height=\"{h}\" frameRate=\"{}\">{seg}</Representation>\n",
                f["width"].as_u64().unwrap_or(0), f["fps"].as_u64().unwrap_or(30)
            );
            nv += 1;
        } else if mime.starts_with("audio/mp4")
            && f["isDrc"].as_bool() != Some(true)
            && match audio_track {
                Some(id) => f["audioTrack"]["id"].as_str() == Some(id),
                None => f["audioTrack"]["audioIsDefault"].as_bool() != Some(false),
            }
        {
            audio += &format!(
                "<Representation id=\"{itag}\" codecs=\"{c}\" bandwidth=\"{bw}\" audioSamplingRate=\"{}\"><AudioChannelConfiguration schemeIdUri=\"urn:mpeg:dash:23003:3:audio_channel_configuration:2011\" value=\"{}\"/>{seg}</Representation>\n",
                f["audioSampleRate"].as_str().unwrap_or("44100"),
                f["audioChannels"].as_u64().unwrap_or(2)
            );
            na += 1;
        }
    }
    if nv == 0 || na == 0 {
        return None;
    }
    let secs = duration_ms as f64 / 1000.0;
    let mpd = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011" type="static" minBufferTime="PT1.5S" mediaPresentationDuration="PT{secs:.3}S">
<Period>
<AdaptationSet id="0" contentType="video" mimeType="video/mp4" subsegmentAlignment="true" startWithSAP="1">
{video}</AdaptationSet>
<AdaptationSet id="1" contentType="audio" mimeType="audio/mp4" lang="und" subsegmentAlignment="true" startWithSAP="1">
{audio}</AdaptationSet>
</Period>
</MPD>
"#
    );
    Some((mpd, nv, na))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fmt(itag: u64, mime: &str, w: u64, h: u64) -> Value {
        json!({"itag": itag, "mimeType": mime, "width": w, "height": h, "bitrate": 1000, "url": "https://x/v",
               "initRange": {"start": "0", "end": "9"}, "indexRange": {"start": "10", "end": "19"}})
    }

    #[test]
    fn a_vertical_video_is_capped_by_its_short_side() {
        let v = "video/mp4; codecs=\"avc1.640028\"";
        let a = json!({"itag": 140, "mimeType": "audio/mp4; codecs=\"mp4a.40.2\"", "bitrate": 128000, "url": "https://x/a",
                       "initRange": {"start": "0", "end": "9"}, "indexRange": {"start": "10", "end": "19"}});
        let f = [fmt(137, v, 1080, 1920), fmt(136, v, 720, 1280), fmt(135, v, 480, 854), a];
        let (mpd, nv, na) = build(&f, 60_000, "avc1", 720, 1080).unwrap();
        assert_eq!((nv, na), (2, 1));
        assert!(mpd.contains("id=\"137\"") && !mpd.contains("id=\"135\""));
        // A track id that is not there falls back to the default track.
        assert!(build_with_audio(&f, 60_000, "avc1", 720, 1080, Some("xx.9")).is_some());
    }
}

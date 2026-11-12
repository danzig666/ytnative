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
    let best = formats
        .iter()
        .filter(|f| f["mimeType"].as_str().is_some_and(|m| m.starts_with("video/mp4") && codecs(m).is_some_and(|c| c.starts_with(video_codec))))
        .filter_map(|f| f["height"].as_u64())
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
            if h > max_height || h < min_height {
                continue;
            }
            video += &format!(
                "<Representation id=\"{itag}\" codecs=\"{c}\" bandwidth=\"{bw}\" width=\"{}\" height=\"{h}\" frameRate=\"{}\">{seg}</Representation>\n",
                f["width"], f["fps"].as_u64().unwrap_or(30)
            );
            nv += 1;
        } else if mime.starts_with("audio/mp4")
            && f["isDrc"].as_bool() != Some(true)
            && f["audioTrack"]["audioIsDefault"].as_bool() != Some(false)
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

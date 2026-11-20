//! Audio only (GitHub issue #9): the video plays as sound alone over a black screen.
//!
//! Engine A (the system player) gets a DASH manifest with the audio adaptation set alone, so no
//! video is downloaded or decoded (`yt_core::mpd::build_audio_only`). Engine B clocks everything on
//! the decoded video frames (position, priming, seeks land on video keyframes), so a stream only it
//! plays (a live stream) keeps the smallest video rendition there, under the same black screen.
//!
//! The screen is black but for a small, dim card (thumbnail, title, channel) that moves to another
//! place every minute: on an OLED panel nothing bright stays put while the sound plays.

use crate::player::Status;
use crate::ui;
use plx_ui::Rect;
use serde_json::Value;

/// The card: a 16:9 thumbnail with the title and channel beside it.
const THUMB_W: f32 = 256.0;
const THUMB_H: f32 = 144.0;
const CARD_W: f32 = 900.0;
/// The card moves this often (ms).
const MOVE_MS: u32 = 60_000;
/// Where the card may go: the upper part of the screen, clear of the controls under it.
const AREA: Rect = Rect::new(96.0, 72.0, 1920.0 - 192.0, 560.0);
/// How bright the card is (alpha over black): readable, never bright.
const CARD_ALPHA: f32 = 0.42;

/// The smallest video rung (short side) among the formats with a direct URL, for a stream that
/// must keep a video track: the least video to download and decode.
#[cfg_attr(windows, allow(dead_code))] // the TV-only feed engine uses it
pub fn lowest_rung(formats: &[Value]) -> Option<u32> {
    let video = |f: &&Value| f["mimeType"].as_str().is_some_and(|m| m.starts_with("video/")) && yt_core::mpd::rung(f) > 0;
    let lowest = |direct: bool| formats.iter().filter(video).filter(|f| !direct || f["url"].is_string()).map(yt_core::mpd::rung).min();
    lowest(true).or_else(|| lowest(false)).map(|h| h as u32)
}

/// The card's top-left at `now_ms`: one of a spread of places in [`AREA`], another every
/// [`MOVE_MS`]. A hash of the minute, not a cycle: neighbouring minutes land far apart.
pub fn card_origin(now_ms: u32) -> (f32, f32) {
    let slot = (now_ms / MOVE_MS) as u64;
    let mut h = slot.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 32;
    let fx = (h & 0xFFFF) as f32 / 65535.0;
    let fy = ((h >> 16) & 0xFFFF) as f32 / 65535.0;
    let room_x = (AREA.w - CARD_W).max(0.0);
    let room_y = (AREA.h - THUMB_H).max(0.0);
    ((AREA.x + fx * room_x).round(), (AREA.y + fy * room_y).round())
}

/// Black over the whole picture, and the dim card. Drawn under the controls, captions and the
/// screen dimming, which all still apply over it.
pub fn draw(p: plx_ui::Painter, m: &dyn plx_machine::machine::Measure, st: &Status, now_ms: u32) {
    ui::fill(p, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.0, 0.0, 0.0, 1.0]);
    let (x, y) = card_origin(now_ms);
    let thumb = Rect::new(x, y, THUMB_W, THUMB_H);
    let url = format!("https://i.ytimg.com/vi/{}/mqdefault.jpg", st.video_id);
    let tex = if st.video_id.is_empty() { 0 } else { plx_ui::tex::resolve_wh_on(0, &url, 0, 0, false).0 };
    if tex != 0 {
        p.tex_uv(tex, [0.0, 0.0, 1.0, 1.0], thumb, 10.0, [1.0, 1.0, 1.0, CARD_ALPHA]);
    } else {
        ui::fill(p, thumb, 10.0, [1.0, 1.0, 1.0, 0.06]);
    }
    let tx = x + THUMB_W + 28.0;
    let tw = CARD_W - THUMB_W - 28.0;
    let ink = [1.0, 1.0, 1.0, CARD_ALPHA];
    let faint = [1.0, 1.0, 1.0, CARD_ALPHA * 0.7];
    ui::text_cut(p, m, &st.title, tx, y + 18.0, tw, 30, ink, ui::ALIGN_LEFT, true);
    ui::text_cut(p, m, &st.author, tx, y + 64.0, tw, 26, faint, ui::ALIGN_LEFT, false);
    ui::text(p, "Audio only", tx, y + 106.0, 22, faint, ui::ALIGN_LEFT, false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lowest_rung_prefers_direct_formats() {
        let f = [
            json!({"mimeType": "video/mp4; codecs=\"avc1\"", "width": 256, "height": 144}),
            json!({"mimeType": "video/mp4; codecs=\"avc1\"", "width": 640, "height": 360, "url": "u"}),
            json!({"mimeType": "video/webm; codecs=\"vp9\"", "width": 1920, "height": 1080, "url": "u"}),
            json!({"mimeType": "audio/mp4; codecs=\"mp4a.40.2\"", "url": "u"}),
        ];
        assert_eq!(lowest_rung(&f), Some(360));
        assert_eq!(lowest_rung(&f[..1]), Some(144));
        assert_eq!(lowest_rung(&f[3..]), None);
    }

    #[test]
    fn the_card_stays_in_its_area_and_moves_every_minute() {
        let mut seen = std::collections::HashSet::new();
        for minute in 0..200u32 {
            let (x, y) = card_origin(minute * MOVE_MS + 1234);
            assert!(x >= AREA.x && x + CARD_W <= AREA.x + AREA.w + 0.5, "x {x}");
            assert!(y >= AREA.y && y + THUMB_H <= AREA.y + AREA.h + 0.5, "y {y}");
            // Within a minute it stays put.
            assert_eq!(card_origin(minute * MOVE_MS), card_origin(minute * MOVE_MS + MOVE_MS - 1));
            seen.insert((x as i32, y as i32));
        }
        assert!(seen.len() > 150, "only {} places", seen.len());
        // Neighbouring minutes are not neighbours on screen.
        let (a, b) = (card_origin(0), card_origin(MOVE_MS));
        assert!((a.0 - b.0).abs() + (a.1 - b.1).abs() > 40.0);
    }
}

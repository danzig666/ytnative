//! Audio loudness from the player response, and the volume YouTube's own players derive from it.
//!
//! YouTube measures every upload and reports how much louder than its reference level it is
//! (`loudnessDb`: 0 is the reference, about -14 LUFS; positive is louder). Its players turn a loud
//! video down by that much so videos play at about the same level, and never turn a quiet one up
//! (that would clip). The TV web player's rule, quoted in MediaServiceCore's
//! `YouTubeMediaItemFormatInfo.getVolumeLevel`: `Math.min(1, 10 ** (-loudnessDb / 20))`.
//!
//! Where the value is read from, best first:
//! - the played audio format's own `loudnessDb` (a dubbed track is measured on its own),
//! - `playerConfig.audioConfig.loudnessDb` (the default audio track),
//! - `playerConfig.audioConfig.perceptualLoudnessDb`, an absolute level that sits 14 dB below
//!   `loudnessDb` in every response seen (the reference is -14), so it converts back.

use serde_json::Value;

/// The reference level `loudnessDb` is relative to, as a `perceptualLoudnessDb` value.
pub const REFERENCE_DB: f32 = -14.0;

/// How much louder than YouTube's reference the video is, in decibels (`None`: not reported,
/// as for most live streams).
pub fn loudness_db(player_response: &Value, audio_format: Option<&Value>) -> Option<f32> {
    let num = |v: &Value| v.as_f64().map(|f| f as f32).filter(|f| f.is_finite());
    let config = &player_response["playerConfig"]["audioConfig"];
    audio_format
        .and_then(|f| num(&f["loudnessDb"]))
        .or_else(|| num(&config["loudnessDb"]))
        .or_else(|| num(&config["perceptualLoudnessDb"]).map(|p| p - REFERENCE_DB))
}

/// The gain to play the video at, in decibels: the loudness turned back to the reference, but
/// only ever down (0 for a video at or below the reference).
pub fn gain_db(loudness_db: f32) -> f32 {
    (-loudness_db).min(0.0)
}

/// [`gain_db`] as an amplitude factor (0 < factor <= 1), the scale a volume control works in.
pub fn gain_factor(loudness_db: f32) -> f32 {
    10f32.powf(gain_db(loudness_db) / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn loud_videos_are_turned_down_quiet_ones_are_left() {
        assert_eq!(gain_db(6.0), -6.0);
        assert_eq!(gain_db(0.0), 0.0);
        assert_eq!(gain_db(-8.5), 0.0);
        assert!((gain_factor(6.0) - 0.501).abs() < 0.001);
        assert!((gain_factor(20.0) - 0.1).abs() < 0.0001);
        assert_eq!(gain_factor(-3.0), 1.0);
        assert_eq!(gain_factor(0.0), 1.0);
    }

    #[test]
    fn reads_the_best_value_available() {
        let pr = json!({"playerConfig": {"audioConfig": {"loudnessDb": 3.5, "perceptualLoudnessDb": -10.5}}});
        let dub = json!({"itag": 251, "loudnessDb": 1.25});
        assert_eq!(loudness_db(&pr, Some(&dub)), Some(1.25));
        assert_eq!(loudness_db(&pr, Some(&json!({"itag": 140}))), Some(3.5));
        assert_eq!(loudness_db(&pr, None), Some(3.5));
        let perceptual = json!({"playerConfig": {"audioConfig": {"perceptualLoudnessDb": -9.0}}});
        assert_eq!(loudness_db(&perceptual, None), Some(5.0));
        assert_eq!(loudness_db(&json!({"playerConfig": {"audioConfig": {}}}), None), None);
        assert_eq!(loudness_db(&json!({}), None), None);
    }
}

//! **The 4K engine's start and seek decisions** (`feed::engine`), as plain functions so they can
//! be tested off the TV: the engine itself compiles for the TV only.

use std::time::Duration;

/// Buffer this much video and audio (paused) before Play, after a Load or a seek.
pub const PRIME_NS: i64 = 700_000_000;
pub const PRIME_AUDIO_NS: i64 = 300_000_000;
/// ...or start anyway once this much video is buffered (audioless / starved audio)...
pub const PRIME_VIDEO_MAX_NS: i64 = 2_500_000_000;
/// ...but with an audio track, only after waiting this long for its first audio: video
/// downloads faster, and Play without audio left the picture without its clock (about one start
/// in ten on the C3; after a reload it ran at about 3x).
pub const PRIME_AUDIO_WAIT: Duration = Duration::from_secs(8);
/// How long a seek may wait for the first video unit of its generation (slow networks included).
pub const SEEK_LANDING_TIMEOUT: Duration = Duration::from_secs(30);

/// What priming has buffered so far.
#[derive(Clone, Copy, Debug, Default)]
pub struct Prime {
    pub has_audio: bool,
    /// Video and audio buffered past the start point (ns).
    pub video_ns: i64,
    pub audio_ns: i64,
    /// Any video / audio unit fed at all.
    pub any_video: bool,
    pub any_audio: bool,
    /// Both tracks are fed to their end (a short clip, or the end of the video).
    pub all_fed: bool,
    /// Time since priming began.
    pub waited: Duration,
}

/// Whether priming may end and Play be sent: enough of both tracks; or a lot of video with the
/// audio started (or given up on); or everything fed.
pub fn primed(p: Prime) -> bool {
    let audio_ready_or_given_up = !p.has_audio || p.any_audio || p.waited > PRIME_AUDIO_WAIT;
    (p.video_ns >= PRIME_NS && (!p.has_audio || p.audio_ns >= PRIME_AUDIO_NS))
        || (p.video_ns >= PRIME_VIDEO_MAX_NS && audio_ready_or_given_up)
        || (p.all_fed && p.any_video)
}

/// A seek waiting for its first video unit: keep waiting, or give up with the reason.
pub fn landing_wait(video_drained: bool, waited: Duration) -> Result<(), &'static str> {
    if video_drained {
        Err("no video after the seek target")
    } else if waited > SEEK_LANDING_TIMEOUT {
        Err("timed out waiting for media after a seek")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1_000_000;

    fn p() -> Prime {
        Prime { has_audio: true, ..Default::default() }
    }

    #[test]
    fn play_waits_for_both_tracks() {
        assert!(!primed(p()));
        assert!(!primed(Prime { video_ns: 700 * MS, ..p() }), "video alone is not enough");
        assert!(primed(Prime { video_ns: 700 * MS, audio_ns: 300 * MS, any_video: true, any_audio: true, ..p() }));
        assert!(!primed(Prime { video_ns: 699 * MS, audio_ns: 5000 * MS, any_audio: true, ..p() }));
    }

    #[test]
    fn a_lot_of_video_starts_once_the_audio_has_begun() {
        let lots = Prime { video_ns: 2500 * MS, any_video: true, ..p() };
        assert!(!primed(lots), "no audio yet: the picture would have no clock");
        assert!(primed(Prime { any_audio: true, audio_ns: 100 * MS, ..lots }));
        assert!(!primed(Prime { waited: Duration::from_secs(7), ..lots }));
        assert!(primed(Prime { waited: Duration::from_secs(9), ..lots }), "audio given up on after 8 s");
    }

    #[test]
    fn a_video_without_audio_needs_video_only() {
        assert!(primed(Prime { has_audio: false, video_ns: 700 * MS, any_video: true, ..Default::default() }));
    }

    #[test]
    fn a_short_clip_fed_to_its_end_starts_whatever_its_length() {
        assert!(primed(Prime { video_ns: 200 * MS, audio_ns: 200 * MS, any_video: true, any_audio: true, all_fed: true, ..p() }));
        assert!(!primed(Prime { all_fed: true, ..p() }), "nothing to show at all");
    }

    #[test]
    fn a_seek_waits_up_to_30_seconds_for_its_keyframe() {
        assert_eq!(landing_wait(false, Duration::from_secs(29)), Ok(()));
        assert!(landing_wait(false, Duration::from_secs(31)).is_err());
        assert!(landing_wait(true, Duration::ZERO).is_err(), "nothing more will come");
    }
}

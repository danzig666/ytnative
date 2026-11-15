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

/// No new picture for this long while playing: the picture has stalled.
pub const STALL_AFTER: Duration = Duration::from_millis(700);
/// A rebuffering pause ends once this much video is fed past the picture (or all of it is).
pub const REBUFFER_TARGET_NS: i64 = 3_000_000_000;
/// How much further than usual the feed may run while the picture is stalled with media at hand.
pub const STALL_EXTRA_MAX_NS: i64 = 4_000_000_000;

/// What to do about a picture that has not moved for `stalled_for` while playing.
#[derive(Debug, PartialEq)]
pub enum Stall {
    Fine,
    /// The download has media: the feed limit, which follows the picture, held it back. Feed
    /// this much further than the limit (it grows with the stall).
    FeedFurther(i64),
    /// The download has run dry: pause audio and video together until enough is buffered. A
    /// pipeline that kept its clock running (it is loaded as a live one) played the audio on and
    /// showed the video seconds late afterwards: the picture froze and then ran out of sync.
    Rebuffer,
}

/// Rebuffering only with less than this fed past the picture: a video of a few still pictures
/// (long frames) has seconds fed ahead of a picture that rightly does not move.
pub const REBUFFER_BELOW_NS: i64 = 500_000_000;

pub fn stall(stalled_for: Duration, download_empty: bool, video_ahead_ns: i64) -> Stall {
    if stalled_for < STALL_AFTER {
        Stall::Fine
    } else if download_empty && video_ahead_ns < REBUFFER_BELOW_NS {
        Stall::Rebuffer
    } else {
        Stall::FeedFurther((stalled_for.as_nanos() as i64).min(STALL_EXTRA_MAX_NS))
    }
}

/// Whether a rebuffering pause may end: enough video fed past the picture, or all of it.
pub fn rebuffered(video_ahead_ns: i64, all_fed: bool) -> bool {
    all_fed || video_ahead_ns >= REBUFFER_TARGET_NS
}

/// Two rebufferings within this time: the connection cannot carry this quality.
pub const SLOW_WINDOW_MS: u64 = 60_000;

/// Counts rebufferings; `note` says when the video should drop a quality step.
#[derive(Default)]
pub struct SlowWatch {
    last: Option<u64>,
}

impl SlowWatch {
    /// A rebuffering began at `now_ms`: true when it is the second within [`SLOW_WINDOW_MS`].
    pub fn note(&mut self, now_ms: u64) -> bool {
        let slow = self.last.is_some_and(|t| now_ms.saturating_sub(t) <= SLOW_WINDOW_MS);
        self.last = Some(now_ms);
        slow
    }
}

/// The quality steps (the short side of the picture) a too slow video drops through.
pub const QUALITY_STEPS: [u32; 6] = [2160, 1440, 1080, 720, 480, 360];

/// The next step below a picture of `width` x `height` (its short side), if any.
pub fn lower_quality(width: u32, height: u32) -> Option<u32> {
    let short = if width > 0 && height > 0 { width.min(height) } else { height.max(width) };
    QUALITY_STEPS.into_iter().find(|q| *q < short.saturating_sub(8))
}

#[cfg(test)]
mod slow_tests {
    use super::*;

    #[test]
    fn two_rebufferings_within_a_minute_mean_too_slow() {
        let mut w = SlowWatch::default();
        assert!(!w.note(1_000), "one is a hiccup");
        assert!(w.note(40_000));
        let mut w = SlowWatch::default();
        assert!(!w.note(0));
        assert!(!w.note(61_000), "a minute apart: still fine");
        assert!(w.note(100_000));
    }

    #[test]
    fn the_quality_drops_one_step_at_a_time() {
        assert_eq!(lower_quality(3840, 2160), Some(1440));
        assert_eq!(lower_quality(2560, 1440), Some(1080));
        assert_eq!(lower_quality(1920, 1080), Some(720));
        assert_eq!(lower_quality(1920, 1036), Some(720), "a cropped 1080p");
        assert_eq!(lower_quality(1080, 1920), Some(720), "portrait: its short side");
        assert_eq!(lower_quality(640, 360), None, "nothing lower");
        assert_eq!(lower_quality(0, 0), None);
    }
}

#[cfg(test)]
mod stall_tests {
    use super::*;

    #[test]
    fn a_short_gap_between_pictures_is_fine() {
        assert_eq!(stall(Duration::from_millis(300), true, 0), Stall::Fine);
        assert_eq!(stall(Duration::from_millis(699), false, 0), Stall::Fine);
    }

    #[test]
    fn a_stall_with_an_empty_download_pauses_both_tracks() {
        assert_eq!(stall(Duration::from_millis(800), true, 100_000_000), Stall::Rebuffer);
    }

    #[test]
    fn a_still_picture_with_video_fed_ahead_is_not_rebuffered() {
        assert_eq!(stall(Duration::from_millis(1200), true, 1_500_000_000), Stall::FeedFurther(1_200_000_000));
    }

    #[test]
    fn a_stall_with_media_at_hand_feeds_further_up_to_a_cap() {
        assert_eq!(stall(Duration::from_millis(1000), false, 0), Stall::FeedFurther(1_000_000_000));
        assert_eq!(stall(Duration::from_secs(60), false, 0), Stall::FeedFurther(STALL_EXTRA_MAX_NS));
    }

    #[test]
    fn rebuffering_ends_with_three_seconds_ahead_or_at_the_end() {
        assert!(!rebuffered(2_999_000_000, false));
        assert!(rebuffered(3_000_000_000, false));
        assert!(rebuffered(0, true));
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

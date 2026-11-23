//! **The player's start rules** (which speed a video starts at, where it resumes, what the speed
//! button toggles to, whether a stop stores the position), as plain functions so they can be
//! tested without a TV. `host` gathers the inputs and applies the results.

use crate::settings::RememberSpeed;
use std::collections::HashMap;

/// Speeds this close to 1 count as normal speed.
const NORMAL: f32 = 0.01;

pub fn is_normal(r: f32) -> bool {
    (r - 1.0).abs() <= NORMAL
}

/// The speed a video starts at: a reload's own speed first (the system player taking over a
/// speed), else the remembered one for its channel or itself, else the default.
pub fn start_speed(remember: RememberSpeed, default: f32, channel: Option<&str>, by_channel: &HashMap<String, f32>, video_speed: Option<f32>, reload: Option<f32>) -> f32 {
    if let Some(r) = reload {
        return r;
    }
    match remember {
        RememberSpeed::PerChannel => channel.and_then(|c| by_channel.get(c).copied()),
        RememberSpeed::PerVideo => video_speed,
        _ => None,
    }
    .unwrap_or(default)
}

/// "Remember speed per channel" for a video that started without knowing its channel: once the
/// channel is known, its remembered speed, when it differs from the speed the video plays at.
pub fn late_channel_speed(remember: RememberSpeed, channel: &str, by_channel: &HashMap<String, f32>, playing_at: f32) -> Option<f32> {
    if remember != RememberSpeed::PerChannel {
        return None;
    }
    by_channel.get(channel).copied().filter(|r| (r - playing_at).abs() > NORMAL)
}

/// What a chosen speed changes in the settings.
#[derive(Debug, PartialEq)]
pub enum SpeedMemory {
    Nothing,
    /// The default speed becomes this.
    Default(f32),
    /// Stored for the playing video.
    Video(String, f32),
    /// Stored for the playing video's channel.
    Channel(String, f32),
}

pub fn remember_speed(remember: RememberSpeed, video_id: &str, channel: &str, r: f32) -> SpeedMemory {
    match remember {
        RememberSpeed::Off => SpeedMemory::Nothing,
        RememberSpeed::All => SpeedMemory::Default(r),
        RememberSpeed::PerVideo if !video_id.is_empty() => SpeedMemory::Video(video_id.to_string(), r),
        RememberSpeed::PerChannel if !channel.is_empty() => SpeedMemory::Channel(channel.to_string(), r),
        _ => SpeedMemory::Nothing,
    }
}

/// The speed button's toggle: from any other speed back to normal; from normal to the last speed
/// other than normal (`other`), or nowhere when there was none.
pub fn toggle_speed(current: f32, other: f32) -> f32 {
    let cur = if current > 0.0 { current } else { 1.0 };
    if !is_normal(cur) || is_normal(other) {
        1.0
    } else {
        other
    }
}

/// The resume settings.
#[derive(Clone, Copy)]
pub struct Resume {
    pub enabled: bool,
    pub short_videos: bool,
    pub live: bool,
}

/// Videos shorter than this (seconds) are "short" for the resume rule.
pub const SHORT_SECS: u64 = 300;

/// Where a video starts (ms), if not at the beginning. `asked`: a start the caller asked for (a
/// phone's position, a replay's 0), always kept. `stored`: this TV's own record (seconds, video
/// length in seconds). `card_secs`: the length the video's card shows. `card_point_ms`: YouTube's
/// watched share on the card, for videos never played here.
pub fn resume_start(r: Resume, asked: Option<u64>, stored: Option<(u64, u64)>, card_secs: Option<u64>, live: bool, card_point_ms: Option<u64>) -> Option<u64> {
    if asked.is_some() {
        return asked;
    }
    let length = stored.map(|(_, d)| d).filter(|d| *d > 0).or(card_secs);
    let short = length.is_some_and(|d| d < SHORT_SECS);
    let resume = r.enabled && (r.short_videos || !short) && (r.live || !live);
    if !resume {
        return None;
    }
    // This TV's own exact record first (0 = start over).
    stored.map(|(secs, _)| secs * 1000).or(card_point_ms)
}

/// Whether stopping stores the position: only a video that really played (a BACK while it was
/// still loading would store 0 over the resume point), with a length, live only when allowed, and
/// with the watch history on for it.
pub fn keeps_position(video_id: &str, duration_ms: u64, played: bool, live: bool, resume_live: bool, history_on: bool) -> bool {
    !video_id.is_empty() && duration_ms > 0 && played && (!live || resume_live) && history_on
}

/// What SponsorBlock does at one moment.
#[derive(Debug, PartialEq)]
pub enum SbEffect {
    Seek(u64),
    /// A toast (already translated).
    Toast(String),
    /// OK jumps to `to` until playback reaches `until` (`label` names it).
    Offer { to: u64, label: String, until: u64 },
}

/// Playback entered a segment whose category is set to `action`.
pub fn sb_segment(action: crate::settings::SbAction, seg: &yt_core::ext::sponsorblock::Segment) -> Vec<SbEffect> {
    use crate::settings::SbAction;
    let label = seg.category.label();
    let shown = crate::i18n::tr(label);
    match action {
        SbAction::Off => vec![],
        SbAction::Skip => vec![SbEffect::Seek(seg.end_ms)],
        SbAction::SkipWithToast => vec![SbEffect::Seek(seg.end_ms), SbEffect::Toast(crate::i18n::tr("Skipped: {}").replace("{}", shown))],
        SbAction::Ask => vec![
            SbEffect::Toast(crate::i18n::tr("{} \u{2014} press OK to skip").replace("{}", shown)),
            SbEffect::Offer { to: seg.end_ms, label: label.to_string(), until: seg.end_ms },
        ],
    }
}

/// The video's highlight at `start_ms`, checked once when playback starts at `pos_ms`: nothing
/// when playback is already there or past it.
pub fn sb_highlight(action: crate::settings::SbAction, pos_ms: u64, start_ms: u64) -> Vec<SbEffect> {
    use crate::settings::SbAction;
    if pos_ms + 1000 >= start_ms {
        return vec![];
    }
    match action {
        SbAction::Off => vec![],
        SbAction::Skip => vec![SbEffect::Seek(start_ms)],
        SbAction::SkipWithToast => vec![SbEffect::Seek(start_ms), SbEffect::Toast(crate::i18n::tr("Jumped to the highlight").to_string())],
        SbAction::Ask => vec![
            SbEffect::Toast(crate::i18n::tr("Highlight: press OK to jump there").to_string()),
            SbEffect::Offer { to: start_ms, label: "Highlight".into(), until: (pos_ms + 15_000).min(start_ms) },
        ],
    }
}

#[cfg(test)]
mod sponsorblock_tests {
    use super::*;
    use crate::settings::SbAction;
    use yt_core::ext::sponsorblock::{ActionType, Category, Segment, SkipTracker};

    fn seg(cat: Category, a: u64, b: u64, id: &str) -> Segment {
        Segment { category: cat, start_ms: a, end_ms: b, action_type: ActionType::Skip, uuid: id.into() }
    }

    #[test]
    fn each_action_does_what_its_name_says() {
        let s = seg(Category::Sponsor, 10_000, 40_000, "a");
        assert_eq!(sb_segment(SbAction::Off, &s), vec![]);
        assert_eq!(sb_segment(SbAction::Skip, &s), vec![SbEffect::Seek(40_000)]);
        let t = sb_segment(SbAction::SkipWithToast, &s);
        assert_eq!(t[0], SbEffect::Seek(40_000));
        assert!(matches!(&t[1], SbEffect::Toast(m) if !m.contains("{}")));
        let ask = sb_segment(SbAction::Ask, &s);
        assert!(!ask.iter().any(|e| matches!(e, SbEffect::Seek(_))), "asking never skips by itself");
        assert!(ask.contains(&SbEffect::Offer { to: 40_000, label: "Sponsor".into(), until: 40_000 }));
    }

    #[test]
    fn the_highlight_is_offered_only_ahead_and_for_at_most_15_seconds() {
        assert_eq!(sb_highlight(SbAction::Skip, 0, 60_000), vec![SbEffect::Seek(60_000)]);
        assert_eq!(sb_highlight(SbAction::Skip, 59_500, 60_000), vec![], "already there");
        assert_eq!(sb_highlight(SbAction::Skip, 90_000, 60_000), vec![], "already past it");
        let ask = sb_highlight(SbAction::Ask, 0, 60_000);
        assert!(ask.contains(&SbEffect::Offer { to: 60_000, label: "Highlight".into(), until: 15_000 }));
        let near = sb_highlight(SbAction::Ask, 50_000, 60_000);
        assert!(near.contains(&SbEffect::Offer { to: 60_000, label: "Highlight".into(), until: 60_000 }));
    }

    /// Plays a video through the tracker as the host does, 250 ms a step: a seek lands
    /// `land_early` ms before its target (a keyframe), as on the TV. `seek_back`: at the first
    /// position past `.0` the viewer seeks to `.1`. Returns the seeks SponsorBlock made.
    fn play(segments: &[Segment], skip_again: bool, land_early: u64, seek_back: Option<(u64, u64)>) -> Vec<u64> {
        let mut t = SkipTracker::new(skip_again);
        let (mut pos, mut seeks, mut sought_back) = (0u64, vec![], false);
        while pos < 300_000 && seeks.len() < 20 {
            if let Some(s) = t.check_where(segments, pos, |_| true).cloned() {
                for e in sb_segment(SbAction::Skip, &s) {
                    if let SbEffect::Seek(to) = e {
                        seeks.push(to);
                        pos = to.saturating_sub(land_early);
                    }
                }
                t.mark_done(&s);
                continue;
            }
            if let Some((at, back)) = seek_back {
                if !sought_back && pos >= at {
                    sought_back = true;
                    pos = back;
                    continue;
                }
            }
            pos += 250;
        }
        seeks
    }

    #[test]
    fn a_skip_landing_just_before_the_segments_end_does_not_loop() {
        let segs = [seg(Category::Sponsor, 30_000, 90_000, "s")];
        assert_eq!(play(&segs, true, 3_000, None), vec![90_000], "skip again on: still one skip");
        assert_eq!(play(&segs, false, 3_000, None), vec![90_000]);
    }

    #[test]
    fn seeking_back_into_a_skipped_segment_skips_again_only_when_asked() {
        let segs = [seg(Category::Sponsor, 30_000, 90_000, "s")];
        assert_eq!(play(&segs, true, 0, Some((120_000, 20_000))), vec![90_000, 90_000]);
        assert_eq!(play(&segs, false, 0, Some((120_000, 20_000))), vec![90_000]);
    }

    #[test]
    fn overlapping_segments_that_end_together_do_not_loop() {
        // jP9yjEDlc4k: a sponsor and an outro both run to the video's end; the skip lands on a
        // keyframe inside the sponsor, before the outro starts.
        let segs = [seg(Category::Sponsor, 252_209, 270_876, "s"), seg(Category::Outro, 265_401, 270_876, "o")];
        for again in [true, false] {
            let seeks = play(&segs, again, 7_800, None);
            assert_eq!(seeks, vec![270_876], "skip again {again}");
        }
    }

    #[test]
    fn back_to_back_segments_are_skipped_one_after_the_other() {
        let segs = [seg(Category::Intro, 0, 10_000, "i"), seg(Category::Sponsor, 10_000, 40_000, "s")];
        assert_eq!(play(&segs, false, 0, None), vec![10_000, 40_000]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ON: Resume = Resume { enabled: true, short_videos: false, live: true };

    #[test]
    fn the_start_speed_follows_the_remember_setting() {
        let mut ch = HashMap::new();
        ch.insert("Chan".to_string(), 1.5);
        assert_eq!(start_speed(RememberSpeed::Off, 1.25, Some("Chan"), &ch, Some(2.0), None), 1.25);
        assert_eq!(start_speed(RememberSpeed::All, 1.25, Some("Chan"), &ch, Some(2.0), None), 1.25);
        assert_eq!(start_speed(RememberSpeed::PerChannel, 1.0, Some("Chan"), &ch, Some(2.0), None), 1.5);
        assert_eq!(start_speed(RememberSpeed::PerChannel, 1.0, Some("Other"), &ch, None, None), 1.0);
        assert_eq!(start_speed(RememberSpeed::PerChannel, 1.0, None, &ch, None, None), 1.0, "no card: the default");
        assert_eq!(start_speed(RememberSpeed::PerVideo, 1.0, None, &ch, Some(2.0), None), 2.0);
        assert_eq!(start_speed(RememberSpeed::PerVideo, 1.0, None, &ch, Some(2.0), Some(0.5)), 0.5, "a reload keeps its speed");
    }

    #[test]
    fn a_video_without_a_card_gets_its_channels_speed_once_the_channel_is_known() {
        let mut ch = HashMap::new();
        ch.insert("Chan".to_string(), 1.5);
        assert_eq!(late_channel_speed(RememberSpeed::PerChannel, "Chan", &ch, 1.0), Some(1.5));
        assert_eq!(late_channel_speed(RememberSpeed::PerChannel, "Chan", &ch, 1.5), None, "already at it");
        assert_eq!(late_channel_speed(RememberSpeed::PerChannel, "Other", &ch, 1.0), None);
        assert_eq!(late_channel_speed(RememberSpeed::PerVideo, "Chan", &ch, 1.0), None);
        // Stored under the name the player shows, looked up under the same name.
        match remember_speed(RememberSpeed::PerChannel, "v", "Chan", 2.0) {
            SpeedMemory::Channel(c, r) => {
                ch.insert(c, r);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(late_channel_speed(RememberSpeed::PerChannel, "Chan", &ch, 1.0), Some(2.0));
    }

    #[test]
    fn a_chosen_speed_is_stored_where_the_setting_says() {
        assert_eq!(remember_speed(RememberSpeed::Off, "v", "c", 2.0), SpeedMemory::Nothing);
        assert_eq!(remember_speed(RememberSpeed::All, "v", "c", 2.0), SpeedMemory::Default(2.0));
        assert_eq!(remember_speed(RememberSpeed::PerVideo, "v", "c", 2.0), SpeedMemory::Video("v".into(), 2.0));
        assert_eq!(remember_speed(RememberSpeed::PerChannel, "v", "c", 2.0), SpeedMemory::Channel("c".into(), 2.0));
        assert_eq!(remember_speed(RememberSpeed::PerChannel, "v", "", 2.0), SpeedMemory::Nothing, "channel not known yet");
        assert_eq!(remember_speed(RememberSpeed::PerVideo, "", "c", 2.0), SpeedMemory::Nothing);
    }

    #[test]
    fn the_toggle_goes_back_to_normal_and_returns_to_the_last_speed() {
        assert_eq!(toggle_speed(1.0, 1.5), 1.5);
        assert_eq!(toggle_speed(1.5, 1.5), 1.0);
        assert_eq!(toggle_speed(2.0, 1.5), 1.0);
        assert_eq!(toggle_speed(1.0, 1.0), 1.0, "no other speed chosen yet");
        assert_eq!(toggle_speed(0.0, 1.75), 1.75, "an unknown rate counts as normal");
    }

    #[test]
    fn resume_prefers_this_tvs_record_then_youtubes_progress() {
        assert_eq!(resume_start(ON, None, Some((600, 1200)), None, false, Some(30_000)), Some(600_000));
        assert_eq!(resume_start(ON, None, Some((0, 1200)), None, false, Some(30_000)), Some(0), "0 = watched to the end here, start over");
        assert_eq!(resume_start(ON, None, None, Some(1200), false, Some(30_000)), Some(30_000));
        assert_eq!(resume_start(ON, None, None, Some(1200), false, None), None);
    }

    #[test]
    fn resume_skips_short_videos_and_live_unless_allowed() {
        assert_eq!(resume_start(ON, None, Some((100, 200)), None, false, None), None, "under 5 minutes");
        let shorts = Resume { short_videos: true, ..ON };
        assert_eq!(resume_start(shorts, None, Some((100, 200)), None, false, None), Some(100_000));
        assert_eq!(resume_start(ON, None, None, Some(200), false, Some(50_000)), None, "the card's length counts too");
        let no_live = Resume { live: false, ..ON };
        assert_eq!(resume_start(no_live, None, Some((600, 1200)), None, true, None), None);
        let off = Resume { enabled: false, ..ON };
        assert_eq!(resume_start(off, None, Some((600, 1200)), None, false, Some(1)), None);
        assert_eq!(resume_start(off, Some(42), None, None, false, None), Some(42), "an asked start always wins");
        assert_eq!(resume_start(ON, Some(0), Some((600, 1200)), None, false, None), Some(0), "a replay starts at 0");
    }

    #[test]
    fn a_stop_stores_the_position_only_after_real_playback() {
        assert!(keeps_position("v", 1000, true, false, false, true));
        assert!(!keeps_position("v", 1000, false, false, false, true), "still loading");
        assert!(!keeps_position("v", 0, true, false, false, true));
        assert!(!keeps_position("", 1000, true, false, false, true));
        assert!(!keeps_position("v", 1000, true, true, false, true), "live without the live setting");
        assert!(keeps_position("v", 1000, true, true, true, true));
        assert!(!keeps_position("v", 1000, true, false, false, false), "history off");
    }
}

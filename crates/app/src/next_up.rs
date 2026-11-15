//! **What plays when a video ends**, as plain data in and a decision out, so the playback modes can
//! be tested without a screen. `screens::player` gathers the [`EndContext`] and carries out the
//! [`EndAction`].

use crate::settings::PlaybackMode;

/// Everything the end-of-video decision looks at.
pub struct EndContext<'a> {
    /// The video that just ended.
    pub current: &'a str,
    pub mode: PlaybackMode,
    /// A Short in a Shorts list with "loop Shorts" on: it plays again (as on YouTube).
    pub loop_short: bool,
    /// The "Play next" queue (video ids, in order).
    pub play_next: &'a [String],
    /// The video plays from a playlist on this TV.
    pub in_local: bool,
    /// The list the video is in (the YouTube playlist's items, else the list it was opened from),
    /// with the YouTube playlist's id when it is one.
    pub list: Option<(&'a [String], Option<&'a str>)>,
    /// The YouTube playlist the watch page reports (id, items).
    pub yt_playlist: Option<(&'a str, &'a [String])>,
    /// The watch page's autoplay pick (video, playlist), other than `current`.
    pub autoplay: Option<(String, Option<String>)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum EndAction {
    /// Play the same video again.
    Replay,
    /// Play the first video of "Play next" (no playlist).
    Queued(String),
    /// Play this video; the playlist, else keep the current one.
    Go(String, Option<String>),
    /// Leave the player.
    Close,
    /// Stay on the ended video with the controls shown.
    Stay,
}

/// The decision when a video ends. `rand` picks the shuffled video.
pub fn on_ended(c: &EndContext<'_>, rand: u64) -> EndAction {
    use PlaybackMode as M;
    if c.loop_short {
        return EndAction::Replay;
    }
    // "Play next" goes first, unless this video repeats or the mode stops after each video.
    if !matches!(c.mode, M::RepeatOne | M::PauseEach | M::CloseEach) {
        if let Some(v) = c.play_next.iter().find(|v| v.as_str() != c.current) {
            return EndAction::Queued(v.clone());
        }
    }
    let in_list = |dir, wrap| c.list.and_then(|(ids, pl)| list_step(ids, c.current, dir, wrap).map(|v| (v.clone(), pl.map(str::to_string))));
    let yt = |repeat| yt_next(c.yt_playlist, c.autoplay.clone(), c.current, repeat);
    let target = match c.mode {
        M::RepeatOne => return EndAction::Replay,
        M::PauseEach => None,
        M::CloseEach => return EndAction::Close,
        // A playlist on this TV plays on in its order, then YouTube's pick (as a YouTube playlist
        // does).
        M::PlayNext if c.in_local => in_list(1, false).or_else(|| yt(false)),
        M::PlayNext => yt(false),
        M::ListOnce => in_list(1, false),
        M::ListRepeat => in_list(1, true).or_else(|| yt(true)),
        M::Shuffle => c
            .list
            .and_then(|(ids, pl)| list_random(ids, c.current, rand).map(|v| (v.clone(), pl.map(str::to_string))))
            .or_else(|| yt(false)),
        M::Reverse => in_list(-1, false),
    };
    match target {
        Some((v, pl)) => EndAction::Go(v, pl),
        None => EndAction::Stay,
    }
}

/// The item `dir` places from `current` in `ids`; at an end, wraps (`wrap`) or stops.
pub fn list_step<'a>(ids: &'a [String], current: &str, dir: i32, wrap: bool) -> Option<&'a String> {
    let i = ids.iter().position(|v| v == current)? as i32 + dir;
    let n = ids.len() as i32;
    let j = if (0..n).contains(&i) {
        i
    } else if wrap {
        i.rem_euclid(n)
    } else {
        return None;
    };
    ids.get(j as usize)
}

/// A random item of `ids` other than `current` (`rand` picks it), if `current` is in the list.
pub fn list_random<'a>(ids: &'a [String], current: &str, rand: u64) -> Option<&'a String> {
    if !ids.iter().any(|v| v == current) {
        return None;
    }
    let others: Vec<&String> = ids.iter().filter(|v| *v != current).collect();
    if others.is_empty() {
        return None;
    }
    let seed = rand.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    Some(others[(seed >> 33) as usize % others.len()])
}

/// YouTube's next video: the next item of the YouTube playlist (its first when `repeat_list`),
/// else the autoplay pick.
pub fn yt_next(playlist: Option<(&str, &[String])>, autoplay: Option<(String, Option<String>)>, current: &str, repeat_list: bool) -> Option<(String, Option<String>)> {
    if let Some((id, items)) = playlist {
        if let Some(i) = items.iter().position(|v| v == current) {
            if let Some(n) = items.get(i + 1) {
                return Some((n.clone(), Some(id.to_string())));
            }
            if repeat_list {
                return items.first().map(|v| (v.clone(), Some(id.to_string())));
            }
        }
    }
    autoplay.filter(|(v, _)| v != current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use PlaybackMode as M;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    struct Case {
        current: &'static str,
        list: Vec<String>,
        list_pl: Option<&'static str>,
        play_next: Vec<String>,
        in_local: bool,
        yt: Option<(&'static str, Vec<String>)>,
        autoplay: Option<(String, Option<String>)>,
        loop_short: bool,
    }

    impl Case {
        fn new(current: &'static str, list: &[&str]) -> Self {
            Case { current, list: ids(list), list_pl: None, play_next: vec![], in_local: false, yt: None, autoplay: Some(("auto".into(), None)), loop_short: false }
        }
        fn end(&self, mode: PlaybackMode) -> EndAction {
            self.end_rand(mode, 7)
        }
        fn end_rand(&self, mode: PlaybackMode, rand: u64) -> EndAction {
            let c = EndContext {
                current: self.current,
                mode,
                loop_short: self.loop_short,
                play_next: &self.play_next,
                in_local: self.in_local,
                list: (!self.list.is_empty()).then_some((self.list.as_slice(), self.list_pl)),
                yt_playlist: self.yt.as_ref().map(|(id, v)| (*id, v.as_slice())),
                autoplay: self.autoplay.clone(),
            };
            on_ended(&c, rand)
        }
    }

    fn go(v: &str) -> EndAction {
        EndAction::Go(v.into(), None)
    }

    #[test]
    fn play_next_takes_youtubes_autoplay_pick() {
        let c = Case::new("b", &["a", "b", "c"]);
        assert_eq!(c.end(M::PlayNext), go("auto"));
    }

    #[test]
    fn play_next_in_a_playlist_on_this_tv_goes_through_the_list_then_autoplay() {
        let mut c = Case::new("b", &["a", "b", "c"]);
        c.in_local = true;
        assert_eq!(c.end(M::PlayNext), go("c"));
        c.current = "c";
        assert_eq!(c.end(M::PlayNext), go("auto"), "at the end of the list YouTube's pick follows");
    }

    #[test]
    fn the_play_next_queue_goes_first_but_not_when_repeating_or_stopping() {
        let mut c = Case::new("b", &["a", "b", "c"]);
        c.play_next = ids(&["b", "q1", "q2"]);
        for m in [M::PlayNext, M::ListOnce, M::ListRepeat, M::Shuffle, M::Reverse] {
            assert_eq!(c.end(m), EndAction::Queued("q1".into()), "{m:?}: skips the video that just played");
        }
        assert_eq!(c.end(M::RepeatOne), EndAction::Replay);
        assert_eq!(c.end(M::PauseEach), EndAction::Stay);
        assert_eq!(c.end(M::CloseEach), EndAction::Close);
    }

    #[test]
    fn list_once_stops_at_the_end_and_list_repeat_wraps() {
        let mut c = Case::new("c", &["a", "b", "c"]);
        assert_eq!(c.end(M::ListOnce), EndAction::Stay);
        assert_eq!(c.end(M::ListRepeat), go("a"));
        c.current = "a";
        assert_eq!(c.end(M::ListOnce), go("b"));
    }

    #[test]
    fn reverse_walks_backwards_and_stops_at_the_first() {
        let mut c = Case::new("c", &["a", "b", "c"]);
        assert_eq!(c.end(M::Reverse), go("b"));
        c.current = "a";
        assert_eq!(c.end(M::Reverse), EndAction::Stay);
    }

    #[test]
    fn shuffle_never_replays_the_same_video_and_reaches_every_other() {
        let c = Case::new("b", &["a", "b", "c", "d"]);
        let mut seen = std::collections::BTreeSet::new();
        for r in 0..200u64 {
            match c.end_rand(M::Shuffle, r.wrapping_mul(0x9E37_79B9_7F4A_7C15)) {
                EndAction::Go(v, _) => {
                    assert_ne!(v, "b");
                    seen.insert(v);
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(seen.into_iter().collect::<Vec<_>>(), ids(&["a", "c", "d"]));
    }

    #[test]
    fn shuffle_outside_any_list_falls_back_to_autoplay() {
        let c = Case::new("x", &[]);
        assert_eq!(c.end(M::Shuffle), go("auto"));
        let single = Case::new("x", &["x"]);
        assert_eq!(single.end(M::Shuffle), go("auto"));
    }

    #[test]
    fn a_youtube_playlist_keeps_its_id_and_repeats_from_its_first_item() {
        let mut c = Case::new("p3", &["p1", "p2", "p3"]);
        c.list_pl = Some("PL1");
        c.yt = Some(("PL1", ids(&["p1", "p2", "p3"])));
        assert_eq!(c.end(M::ListRepeat), EndAction::Go("p1".into(), Some("PL1".into())));
        c.current = "p2";
        assert_eq!(c.end(M::PlayNext), EndAction::Go("p3".into(), Some("PL1".into())));
    }

    #[test]
    fn a_looping_short_replays_whatever_the_mode() {
        let mut c = Case::new("s", &["s", "t"]);
        c.loop_short = true;
        c.play_next = ids(&["q"]);
        assert_eq!(c.end(M::PlayNext), EndAction::Replay);
    }

    #[test]
    fn autoplay_that_points_back_at_the_same_video_is_ignored() {
        let mut c = Case::new("b", &[]);
        c.autoplay = Some(("b".into(), None));
        assert_eq!(c.end(M::PlayNext), EndAction::Stay);
    }

    #[test]
    fn list_step_handles_missing_videos_and_empty_lists() {
        assert_eq!(list_step(&ids(&["a"]), "z", 1, true), None);
        assert_eq!(list_step(&[], "a", 1, true), None);
        assert_eq!(list_step(&ids(&["a"]), "a", 1, true).map(String::as_str), Some("a"));
    }
}

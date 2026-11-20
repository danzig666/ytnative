//! **What plays when a video ends**, as plain data in and a decision out, so the playback modes can
//! be tested without a screen. `screens::player` gathers the [`EndContext`] and carries out the
//! [`EndAction`].

use crate::play_queue::PlayQueue;
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
    /// The list the video was started from ([`PlayQueue`]).
    pub queue: Option<&'a PlayQueue>,
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
    /// Play this video in this playlist (None: none).
    Go(String, Option<String>),
    /// The queue's play order becomes this (a shuffle started or went round again), then play
    /// this video in this playlist.
    Shuffled(Vec<usize>, String, Option<String>),
    /// Leave the player.
    Close,
    /// Stay on the ended video with the controls shown.
    Stay,
}

/// The decision when a video ends. `rand` seeds a shuffle.
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
    match c.mode {
        M::RepeatOne => return EndAction::Replay,
        M::PauseEach => return EndAction::Stay,
        M::CloseEach => return EndAction::Close,
        _ => {}
    }
    // The list the video was started from decides, while the video is in it: a playlist in every
    // mode, another list in the modes that walk the opened list.
    if let Some(q) = c.queue.filter(|q| q.contains(c.current) && (q.playlist || c.mode != M::PlayNext)) {
        return from_queue(c, q, rand);
    }
    // A "Play next" video cut into a playlist: the playlist goes on as if the video it cut in
    // after had just ended (the queued video's watch page knows nothing of the playlist).
    if let Some((q, from)) = c.queue.and_then(|q| q.resume_after.as_deref().filter(|v| q.playlist && q.contains(v)).map(|v| (q, v))) {
        let resumed = EndContext {
            current: from,
            mode: c.mode,
            loop_short: false,
            play_next: c.play_next,
            queue: c.queue,
            yt_playlist: None,
            autoplay: c.autoplay.clone().filter(|(v, _)| v != c.current),
        };
        return from_queue(&resumed, q, rand);
    }
    // Not in the list it was started from (YouTube's pick, a phone's video): the watch page's
    // playlist when it has one, else its pick.
    let in_panel = |dir, wrap| c.yt_playlist.and_then(|(id, ids)| list_step(ids, c.current, dir, wrap).map(|v| (v.clone(), Some(id.to_string()))));
    let yt = |repeat| yt_next(c.yt_playlist, c.autoplay.clone(), c.current, repeat);
    let target = match c.mode {
        M::ListOnce => in_panel(1, false),
        M::ListRepeat => in_panel(1, true).or_else(|| yt(true)),
        M::Reverse => in_panel(-1, false),
        _ => yt(false),
    };
    match target {
        Some((v, pl)) => EndAction::Go(v, pl),
        None => EndAction::Stay,
    }
}

/// The next video from the list the video was started from (which holds it).
fn from_queue(c: &EndContext<'_>, q: &PlayQueue, rand: u64) -> EndAction {
    use PlaybackMode as M;
    let pl = || q.playlist_id.clone();
    let go = |v: &String| EndAction::Go(v.clone(), pl());
    let cur = c.current;
    let yt = |repeat| yt_next(c.yt_playlist, c.autoplay.clone(), cur, repeat).map(|(v, p)| EndAction::Go(v, p));
    // The list went round: shuffled, a new round in a new order (not starting with this video).
    let again = || {
        let here = q.items.iter().position(|v| v == cur);
        let order = q.shuffled_order(None, here, rand);
        EndAction::Shuffled(order.clone(), q.items[order[0]].clone(), pl())
    };
    // The watch page's playlist has a next video the queue does not know yet (the rest of the
    // list still loading, or its loading failed): the playlist goes on with it before any wrap,
    // reshuffle or YouTube's pick.
    let unknown = || {
        c.yt_playlist
            .filter(|(id, _)| q.playlist_id.as_deref() == Some(*id))
            .and_then(|(id, ids)| list_step(ids, cur, 1, false).filter(|v| !q.contains(v)).map(|v| EndAction::Go(v.clone(), Some(id.to_string()))))
    };
    let next = || q.step(cur, 1, false).map(go).or_else(unknown);
    let action = match c.mode {
        M::PlayNext => next().or_else(|| after_queue(c, q)),
        M::ListOnce => next(),
        M::Reverse => q.step(cur, -1, false).map(go),
        M::ListRepeat => next().or_else(|| match () {
            // The rest of the list is still loading: YouTube's next of the playlist.
            _ if !q.complete() => yt(true),
            _ if q.shuffled() => Some(again()),
            _ => q.step(cur, 1, true).map(go),
        }),
        M::Shuffle if !q.shuffled() && !q.keep_order && q.can_shuffle() => {
            // The first end in Shuffle mode: a play order with this video first, each video once.
            let order = q.shuffled_order(q.items.iter().position(|v| v == cur), None, rand);
            Some(EndAction::Shuffled(order.clone(), q.items[order[1]].clone(), pl()))
        }
        M::Shuffle => next().or_else(|| match () {
            _ if !q.complete() || !q.can_shuffle() => yt(false),
            // Shuffle switched off in the player: the list's order, round again.
            _ if q.keep_order => q.step(cur, 1, true).map(go),
            _ => Some(again()),
        }),
        M::RepeatOne | M::PauseEach | M::CloseEach => None,
    };
    action.unwrap_or(EndAction::Stay)
}

/// "Play next" after the list's last video. The list still loading (or endless): YouTube's next
/// of the playlist. Complete: YouTube's pick, as YouTube plays on after a playlist, unless that
/// pick is a video of the list (a shuffled list ends anywhere; it does not start over).
fn after_queue(c: &EndContext<'_>, q: &PlayQueue) -> Option<EndAction> {
    let pick = if q.complete() {
        c.autoplay.clone().filter(|(v, _)| v != c.current && !q.contains(v))
    } else {
        yt_next(c.yt_playlist, c.autoplay.clone(), c.current, false)
    };
    pick.map(|(v, p)| EndAction::Go(v, p))
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
        current: String,
        queue: Option<PlayQueue>,
        play_next: Vec<String>,
        yt: Option<(&'static str, Vec<String>)>,
        autoplay: Option<(String, Option<String>)>,
        loop_short: bool,
    }

    impl Case {
        /// `current` in a list of cards (not a playlist).
        fn new(current: &str, list: &[&str]) -> Self {
            let queue = (!list.is_empty()).then(|| PlayQueue::list(ids(list), false));
            Case { current: current.into(), queue, play_next: vec![], yt: None, autoplay: Some(("auto".into(), None)), loop_short: false }
        }
        /// `current` in a complete YouTube playlist "PL1".
        fn playlist(current: &str, list: &[&str]) -> Self {
            let mut c = Case::new(current, &[]);
            c.queue = Some(PlayQueue::playlist(Some("PL1".into()), "List".into(), ids(list)));
            c
        }
        fn end(&self, mode: PlaybackMode) -> EndAction {
            self.end_rand(mode, 7)
        }
        fn end_rand(&self, mode: PlaybackMode, rand: u64) -> EndAction {
            let c = EndContext {
                current: &self.current,
                mode,
                loop_short: self.loop_short,
                play_next: &self.play_next,
                queue: self.queue.as_ref(),
                yt_playlist: self.yt.as_ref().map(|(id, v)| (*id, v.as_slice())),
                autoplay: self.autoplay.clone(),
            };
            on_ended(&c, rand)
        }
        /// Plays on from `current` the way the player does, `n` times; the videos played.
        fn play(&mut self, mode: PlaybackMode, n: usize) -> Vec<String> {
            let mut out = Vec::new();
            for r in 0..n {
                let v = match self.end_rand(mode, r as u64 * 31 + 5) {
                    EndAction::Go(v, _) | EndAction::Queued(v) => v,
                    EndAction::Shuffled(order, v, _) => {
                        let cur = self.current.clone();
                        self.queue.as_mut().unwrap().set_order(order, &cur, 0);
                        v
                    }
                    other => panic!("{other:?} after {out:?}"),
                };
                out.push(v.clone());
                self.current = v;
            }
            out
        }
    }

    fn go(v: &str) -> EndAction {
        EndAction::Go(v.into(), None)
    }

    fn pl(v: &str) -> EndAction {
        EndAction::Go(v.into(), Some("PL1".into()))
    }

    #[test]
    fn play_next_from_a_list_of_cards_takes_youtubes_pick() {
        let c = Case::new("b", &["a", "b", "c"]);
        assert_eq!(c.end(M::PlayNext), go("auto"));
    }

    #[test]
    fn play_next_in_a_playlist_follows_the_playlist_not_youtubes_pick() {
        // The report: YouTube's pick was a "My Mix" video while the playlist went on.
        let mut c = Case::playlist("a", &["a", "b", "c"]);
        c.autoplay = Some(("djV11Xbc914".into(), Some("RDMM".into())));
        assert_eq!(c.end(M::PlayNext), pl("b"));
        assert_eq!(c.play(M::PlayNext, 2), ids(&["b", "c"]));
        c.autoplay = Some(("after".into(), None));
        assert_eq!(c.end(M::PlayNext), go("after"), "after the last video YouTube's pick follows");
    }

    #[test]
    fn the_end_of_a_playlist_falls_through_to_youtubes_pick_only_in_play_next() {
        let c = Case::playlist("c", &["a", "b", "c"]);
        assert_eq!(c.end(M::PlayNext), go("auto"));
        assert_eq!(c.end(M::ListOnce), EndAction::Stay);
        assert_eq!(c.end(M::ListRepeat), pl("a"));
        assert_eq!(Case::playlist("a", &["a", "b", "c"]).end(M::Reverse), EndAction::Stay);
    }

    #[test]
    fn youtubes_pick_from_inside_the_finished_playlist_is_not_taken() {
        let mut c = Case::playlist("c", &["a", "b", "c"]);
        c.autoplay = Some(("a".into(), Some("PL1".into())));
        assert_eq!(c.end(M::PlayNext), EndAction::Stay);
    }

    #[test]
    fn a_playlist_still_loading_goes_on_with_the_watch_pages_playlist() {
        let mut c = Case::playlist("b", &["a", "b"]);
        c.queue.as_mut().unwrap().more = Some("token".into());
        c.yt = Some(("PL1", ids(&["a", "b", "c"])));
        assert_eq!(c.end(M::PlayNext), pl("c"));
        assert_eq!(c.end(M::ListRepeat), pl("c"), "no wrapping before the list is complete");
    }

    #[test]
    fn a_playlist_on_this_tv_goes_through_the_list_then_autoplay() {
        let mut c = Case::new("b", &[]);
        c.queue = Some(PlayQueue::playlist(None, "Mine".into(), ids(&["a", "b", "c"])));
        assert_eq!(c.end(M::PlayNext), go("c"));
        c.current = "c".into();
        assert_eq!(c.end(M::PlayNext), go("auto"));
    }

    #[test]
    fn the_play_next_queue_goes_first_but_not_when_repeating_or_stopping() {
        let mut c = Case::playlist("b", &["a", "b", "c"]);
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
        c.current = "a".into();
        assert_eq!(c.end(M::ListOnce), go("b"));
    }

    #[test]
    fn reverse_walks_backwards_and_stops_at_the_first() {
        let mut c = Case::playlist("c", &["a", "b", "c"]);
        assert_eq!(c.play(M::Reverse, 2), ids(&["b", "a"]));
        assert_eq!(c.end(M::Reverse), EndAction::Stay);
    }

    #[test]
    fn a_shuffled_playlist_plays_in_its_order_and_list_once_stops_after_all() {
        let mut c = Case::playlist("v0", &["v0", "v1", "v2", "v3", "v4", "v5"]);
        c.queue.as_mut().unwrap().shuffle(Some("v0"), 42);
        let q = c.queue.clone().unwrap();
        let mut expected = Vec::new();
        let mut cur = "v0".to_string();
        while let Some(n) = q.step(&cur, 1, false) {
            expected.push(n.clone());
            cur = n.clone();
        }
        assert_eq!(expected.len(), 5);
        assert_eq!(c.play(M::ListOnce, 5), expected);
        assert_eq!(c.end(M::ListOnce), EndAction::Stay);
        // Previous goes back in the same order.
        assert_eq!(q.step(&expected[2], -1, false), Some(&expected[1]));
    }

    #[test]
    fn shuffle_mode_plays_every_video_once_before_any_repeats() {
        for start in ["a", "c", "f"] {
            let mut c = Case::new(start, &["a", "b", "c", "d", "e", "f"]);
            let mut round: Vec<String> = c.play(M::Shuffle, 5);
            round.push(start.into());
            round.sort();
            assert_eq!(round, ids(&["a", "b", "c", "d", "e", "f"]), "starting at {start}");
            // Then a new round, again each once, not starting with the last video.
            let last = c.current.clone();
            let mut second = c.play(M::Shuffle, 6);
            assert_ne!(second[0], last);
            second.sort();
            assert_eq!(second, ids(&["a", "b", "c", "d", "e", "f"]));
        }
    }

    #[test]
    fn list_repeat_reshuffles_on_wrap() {
        let mut c = Case::playlist("a", &["a", "b", "c", "d"]);
        c.queue.as_mut().unwrap().shuffle(Some("a"), 1);
        let first: Vec<String> = c.play(M::ListRepeat, 3);
        match c.end(M::ListRepeat) {
            EndAction::Shuffled(order, v, list) => {
                assert_eq!(order.len(), 4);
                assert_ne!(v, first[2], "the new round does not start with the video that just played");
                assert_eq!(list.as_deref(), Some("PL1"));
            }
            other => panic!("{other:?}"),
        }
        let mut next_round = c.play(M::ListRepeat, 4);
        next_round.sort();
        assert_eq!(next_round, ids(&["a", "b", "c", "d"]));
    }

    #[test]
    fn shuffle_outside_any_list_falls_back_to_autoplay() {
        assert_eq!(Case::new("x", &[]).end(M::Shuffle), go("auto"));
        assert_eq!(Case::new("x", &["x"]).end(M::Shuffle), go("auto"));
    }

    #[test]
    fn a_video_not_in_the_queue_uses_the_watch_page_as_before() {
        let mut c = Case::playlist("z", &["a", "b"]);
        assert_eq!(c.end(M::PlayNext), go("auto"));
        assert_eq!(c.end(M::ListOnce), EndAction::Stay);
        c.yt = Some(("PL2", ids(&["y", "z"])));
        assert_eq!(c.end(M::ListRepeat), EndAction::Go("y".into(), Some("PL2".into())));
        assert_eq!(c.end(M::Reverse), EndAction::Go("y".into(), Some("PL2".into())));
    }

    #[test]
    fn a_mix_plays_on_into_the_mix_after_the_videos_known_so_far() {
        let mut c = Case::new("m2", &[]);
        let mut q = PlayQueue::playlist(Some("RDMM".into()), "My Mix".into(), ids(&["m1", "m2"]));
        q.more = Some("token".into());
        c.queue = Some(q);
        c.autoplay = Some(("m3".into(), Some("RDMM".into())));
        for m in [M::PlayNext, M::Shuffle] {
            assert_eq!(c.end(m), EndAction::Go("m3".into(), Some("RDMM".into())), "{m:?}");
        }
        c.current = "m1".into();
        assert_eq!(c.end(M::Shuffle), EndAction::Go("m2".into(), Some("RDMM".into())), "a mix is not shuffled");
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
    fn a_failed_load_is_not_the_end_of_the_playlist() {
        // Only the page's videos are known and loading the rest failed: the watch page's
        // playlist knows the next one.
        let mut c = Case::playlist("b", &["a", "b"]);
        c.queue.as_mut().unwrap().truncated = true;
        c.yt = Some(("PL1", ids(&["a", "b", "c"])));
        for m in [M::PlayNext, M::ListOnce, M::ListRepeat] {
            assert_eq!(c.end(m), pl("c"), "{m:?}");
        }
        // Without the watch page's playlist, List repeat does not loop over the part known.
        c.yt = None;
        assert_ne!(c.end(M::ListRepeat), pl("a"));
        assert!(!c.queue.as_ref().unwrap().complete());
    }

    #[test]
    fn the_watch_pages_next_video_is_taken_before_wrapping_a_complete_queue() {
        let mut c = Case::playlist("b", &["a", "b"]);
        c.yt = Some(("PL1", ids(&["a", "b", "c"])));
        assert_eq!(c.end(M::ListRepeat), pl("c"));
        // A playlist of another id says nothing about this one.
        c.yt = Some(("PL2", ids(&["a", "b", "c"])));
        assert_eq!(c.end(M::ListRepeat), pl("a"));
    }

    #[test]
    fn the_playlist_goes_on_after_a_play_next_video_that_cut_in() {
        let mut c = Case::playlist("q1", &["a", "b", "c"]);
        c.queue.as_mut().unwrap().resume_after = Some("b".into());
        c.autoplay = Some(("recommended".into(), None));
        for m in [M::PlayNext, M::ListOnce, M::ListRepeat] {
            assert_eq!(c.end(m), pl("c"), "{m:?}");
        }
        assert_eq!(c.end(M::Reverse), pl("a"));
        // More "Play next" videos still go first.
        c.play_next = ids(&["q2"]);
        assert_eq!(c.end(M::PlayNext), EndAction::Queued("q2".into()));
        // Not for a list of cards in Play next mode, nor a marker outside the list.
        let mut cards = Case::new("q1", &["a", "b"]);
        cards.queue.as_mut().unwrap().resume_after = Some("a".into());
        assert_eq!(cards.end(M::PlayNext), go("auto"));
        c.play_next.clear();
        c.queue.as_mut().unwrap().resume_after = Some("zz".into());
        assert_eq!(c.end(M::PlayNext), go("recommended"));
    }

    #[test]
    fn shuffle_mode_keeps_the_order_the_viewer_chose() {
        let mut c = Case::playlist("a", &["a", "b", "c"]);
        c.queue.as_mut().unwrap().keep_order = true;
        assert_eq!(c.end(M::Shuffle), pl("b"));
        c.current = "c".into();
        assert_eq!(c.end(M::Shuffle), pl("a"), "round again in the list's order");
        // Switching shuffle on again forgets it.
        c.queue.as_mut().unwrap().shuffle(Some("c"), 3);
        assert!(!c.queue.as_ref().unwrap().keep_order);
    }

    #[test]
    fn list_step_handles_missing_videos_and_empty_lists() {
        assert_eq!(list_step(&ids(&["a"]), "z", 1, true), None);
        assert_eq!(list_step(&[], "a", 1, true), None);
        assert_eq!(list_step(&ids(&["a"]), "a", 1, true).map(String::as_str), Some("a"));
    }
}

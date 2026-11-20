//! **The play queue**: the list a video was started from, which decides what plays next.
//!
//! A playlist (a YouTube playlist or mix, Watch later, Liked videos, a playlist on this TV) is
//! followed by the app itself, in its own order or shuffled, rather than through the watch page's
//! up-next pick: that pick is a recommendation whenever the page does not know the playlist (the
//! viewer's report: after the first video of a playlist came a "My Mix" video). Any other list
//! (a Home row, search results, a channel's videos) is kept too, for the playback modes that walk
//! the opened list. Plain data, so `next_up` can be tested without a screen.

/// The list the playing video was started from.
#[derive(Clone, Debug, Default)]
pub struct PlayQueue {
    /// The YouTube playlist (without "VL"); None for a playlist on this TV and other lists.
    pub playlist_id: Option<String>,
    /// The playlist's title, shown under the player's title.
    pub title: String,
    /// A playlist: it plays on in "Play next" mode and the player shows where it is. Not one: a
    /// Home row, search results (YouTube's own pick follows in "Play next" mode).
    pub playlist: bool,
    /// The list is of Shorts (Up / Down switch videos, "Loop Shorts").
    pub shorts: bool,
    /// A mix: endless, it grows as it plays, and is not shuffled.
    pub infinite: bool,
    /// The videos in the list's order, each once.
    pub items: Vec<String>,
    /// The play order when shuffled: indexes into `items`, each once.
    order: Option<Vec<usize>>,
    /// What is known of each video (title, channel, thumbnail), for the player's playlist row.
    pub info: std::collections::HashMap<String, yt_core::model::Item>,
    /// The token of the list's next window, while more is to load.
    pub more: Option<String>,
    /// A window of the list is being loaded.
    pub loading: bool,
    /// Loading the rest of the list failed: the videos known are not all of it, so its end is not
    /// the playlist's end (`complete` stays false).
    pub truncated: bool,
    /// The viewer switched shuffle off: the Shuffle playback mode then keeps the list's order.
    pub keep_order: bool,
    /// The playlist video a "Play next" video cut in after: the playlist goes on from it once
    /// the queued videos are done.
    pub resume_after: Option<String>,
}

impl PlayQueue {
    /// A list of cards (not a playlist): its videos in order.
    pub fn list(ids: Vec<String>, shorts: bool) -> PlayQueue {
        let mut q = PlayQueue { shorts, ..Default::default() };
        q.extend(ids, None, 0);
        q
    }

    /// A playlist: a YouTube one (`playlist_id`) or one on this TV (None), with the videos known
    /// so far.
    pub fn playlist(playlist_id: Option<String>, title: String, ids: Vec<String>) -> PlayQueue {
        let infinite = playlist_id.as_deref().is_some_and(yt_core::playlist::is_mix);
        let mut q = PlayQueue { playlist_id, title, playlist: true, infinite, ..Default::default() };
        q.extend(ids, None, 0);
        q
    }

    pub fn contains(&self, video: &str) -> bool {
        self.items.iter().any(|v| v == video)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Every video of the list is known (nothing more to load, nothing loading, no load failed).
    pub fn complete(&self) -> bool {
        self.more.is_none() && !self.loading && !self.truncated
    }

    pub fn shuffled(&self) -> bool {
        self.order.is_some()
    }

    /// Shuffle makes sense: a finite list of more than one video.
    pub fn can_shuffle(&self) -> bool {
        !self.infinite && self.items.len() > 1
    }

    /// The video `k`th in play order.
    pub fn at(&self, k: usize) -> Option<&String> {
        match &self.order {
            Some(o) => o.get(k).and_then(|&i| self.items.get(i)),
            None => self.items.get(k),
        }
    }

    /// Where `video` is in play order (0-based).
    pub fn index(&self, video: &str) -> Option<usize> {
        let i = self.items.iter().position(|v| v == video)?;
        match &self.order {
            Some(o) => o.iter().position(|&j| j == i),
            None => Some(i),
        }
    }

    /// "3 of 15": `video`'s place in play order (1-based) and the number of videos.
    pub fn position(&self, video: &str) -> Option<(usize, usize)> {
        self.index(video).map(|k| (k + 1, self.items.len()))
    }

    /// The video `dir` places from `video` in play order; at an end, wraps (`wrap`) or None.
    pub fn step(&self, video: &str, dir: i32, wrap: bool) -> Option<&String> {
        let n = self.items.len() as i32;
        let k = self.index(video)? as i32 + dir;
        if (0..n).contains(&k) {
            self.at(k as usize)
        } else if wrap && n > 0 {
            self.at(k.rem_euclid(n) as usize)
        } else {
            None
        }
    }

    /// The play order of a fresh shuffle: `first` (an index into `items`) first, else any video
    /// but `avoid` first (a repeat does not start with the video that just played).
    pub fn shuffled_order(&self, first: Option<usize>, avoid: Option<usize>, rand: u64) -> Vec<usize> {
        let n = self.items.len();
        let mut o: Vec<usize> = (0..n).collect();
        let mut r = Rng(rand);
        for i in (1..n).rev() {
            o.swap(i, (r.next() % (i as u64 + 1)) as usize);
        }
        if let Some(f) = first.filter(|f| *f < n) {
            let p = o.iter().position(|&i| i == f).unwrap_or(0);
            o.swap(0, p);
        } else if let Some(a) = avoid.filter(|_| n > 1) {
            if o[0] == a {
                o.swap(0, n - 1);
            }
        }
        o
    }

    /// Shuffle on, `video` (if in the list) first.
    pub fn shuffle(&mut self, video: Option<&str>, rand: u64) {
        let first = video.and_then(|v| self.items.iter().position(|x| x == v));
        self.order = Some(self.shuffled_order(first, None, rand));
        self.keep_order = false;
    }

    /// Shuffle off: the list's own order.
    pub fn unshuffle(&mut self) {
        self.order = None;
    }

    /// Takes a play order `next_up` worked out (a shuffle started or repeated); one that no longer
    /// fits (the list grew meanwhile) is shuffled afresh.
    pub fn set_order(&mut self, order: Vec<usize>, video: &str, rand: u64) {
        if order.len() == self.items.len() {
            self.order = Some(order);
        } else {
            self.shuffle(Some(video), rand);
        }
    }

    /// Adds the videos not in the list yet, at its end. Shuffled, each new one goes to a random
    /// place after `video` (the one playing), so it still plays in this round.
    pub fn extend(&mut self, ids: Vec<String>, video: Option<&str>, rand: u64) -> usize {
        let mut r = Rng(rand);
        let mut added = 0;
        // Watch later can hold thousands of videos, arriving 20 at a time.
        let mut known: std::collections::HashSet<String> = self.items.iter().cloned().collect();
        for id in ids {
            if id.is_empty() || !known.insert(id.clone()) {
                continue;
            }
            self.items.push(id);
            added += 1;
            let i = self.items.len() - 1;
            let after = video.and_then(|v| self.index(v)).map_or(0, |k| k + 1);
            if let Some(o) = &mut self.order {
                let lo = after.min(o.len());
                let at = lo + (r.next() % (o.len() - lo + 1) as u64) as usize;
                o.insert(at, i);
            }
        }
        added
    }
}

impl PlayQueue {
    /// [`PlayQueue::extend`] with videos as YouTube lists them (their details kept for the row).
    pub fn add_items(&mut self, items: Vec<yt_core::model::Item>, video: Option<&str>, rand: u64) -> usize {
        let ids = items.iter().map(|i| i.id.clone()).collect();
        for i in items {
            self.info.entry(i.id.clone()).or_insert(i);
        }
        self.extend(ids, video, rand)
    }

    /// The `k`th video in play order as a card: its details when known, else its id and YouTube's
    /// thumbnail for it.
    pub fn item(&self, k: usize) -> Option<yt_core::model::Item> {
        let id = self.at(k)?;
        Some(self.info.get(id).cloned().unwrap_or_else(|| yt_core::model::Item {
            id: id.clone(),
            thumbnail: Some(format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")),
            ..Default::default()
        }))
    }
}

/// A seed for a shuffle: the clock's nanoseconds mixed with the frame clock.
pub fn seed(now_ms: u32) -> u64 {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    nanos ^ ((now_ms as u64) << 32)
}

/// A small random number generator (splitmix64) for the shuffle.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("v{i}")).collect()
    }

    #[test]
    fn steps_in_list_order_and_wraps_only_when_asked() {
        let q = PlayQueue::playlist(Some("PL1".into()), "List".into(), ids(3));
        assert_eq!(q.step("v0", 1, false).map(String::as_str), Some("v1"));
        assert_eq!(q.step("v2", 1, false), None);
        assert_eq!(q.step("v2", 1, true).map(String::as_str), Some("v0"));
        assert_eq!(q.step("v0", -1, false), None);
        assert_eq!(q.step("v0", -1, true).map(String::as_str), Some("v2"));
        assert_eq!(q.step("x", 1, true), None, "a video not in the list has no neighbours");
        assert_eq!(q.position("v1"), Some((2, 3)));
    }

    #[test]
    fn duplicates_are_kept_once() {
        let q = PlayQueue::list(vec!["a".into(), "b".into(), "a".into()], false);
        assert_eq!(q.items, vec!["a", "b"]);
    }

    #[test]
    fn a_shuffle_starts_with_the_playing_video_and_plays_each_once() {
        for seed in 0..50u64 {
            let mut q = PlayQueue::playlist(None, String::new(), ids(12));
            q.shuffle(Some("v5"), seed);
            assert_eq!(q.position("v5"), Some((1, 12)));
            let mut seen = vec!["v5".to_string()];
            let mut cur = "v5".to_string();
            while let Some(n) = q.step(&cur, 1, false) {
                seen.push(n.clone());
                cur = n.clone();
            }
            seen.sort();
            let mut all = ids(12);
            all.sort();
            assert_eq!(seen, all, "seed {seed}");
        }
    }

    #[test]
    fn shuffles_differ_between_seeds() {
        let q = PlayQueue::playlist(None, String::new(), ids(10));
        assert_ne!(q.shuffled_order(None, None, 1), q.shuffled_order(None, None, 2));
    }

    #[test]
    fn a_repeat_does_not_start_with_the_video_that_just_played() {
        let q = PlayQueue::playlist(None, String::new(), ids(3));
        for seed in 0..100 {
            assert_ne!(q.shuffled_order(None, Some(1), seed)[0], 1);
        }
    }

    #[test]
    fn videos_loaded_while_shuffled_still_play_in_this_round() {
        let mut q = PlayQueue::playlist(Some("PL1".into()), String::new(), ids(4));
        q.shuffle(Some("v0"), 3);
        let second = q.step("v0", 1, false).unwrap().clone();
        q.extend(vec!["n1".into(), "n2".into(), "v1".into()], Some(&second), 9);
        assert_eq!(q.len(), 6);
        let k = q.index(&second).unwrap();
        for n in ["n1", "n2"] {
            assert!(q.index(n).unwrap() > k, "{n} plays after the current video");
        }
        assert_eq!(q.index("v0"), Some(0));
    }

    #[test]
    fn a_play_order_that_no_longer_fits_is_made_again() {
        let mut q = PlayQueue::playlist(None, String::new(), ids(3));
        q.set_order(vec![2, 0], "v1", 4);
        assert_eq!(q.position("v1"), Some((1, 3)));
    }

    #[test]
    fn mixes_are_endless_and_not_shuffled() {
        let q = PlayQueue::playlist(Some("RDMM".into()), "My Mix".into(), ids(5));
        assert!(q.infinite && !q.can_shuffle());
        assert!(PlayQueue::playlist(Some("PL1".into()), String::new(), ids(5)).can_shuffle());
    }
}

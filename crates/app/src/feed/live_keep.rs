//! "Keep up with live" (SmartTube c8873b53): a live stream watched near its live edge that falls
//! behind (a rebuffer, a pause) jumps back to the edge when it plays again. A seek of the viewer's
//! own turns it off for the video; a seek to the edge (the FF key) turns it back on.

use super::source::LIVE_DELAY_NS;

/// Normal playback sits `LIVE_DELAY` behind the newest media; this much more counts as behind.
const BEHIND_MS: u64 = (LIVE_DELAY_NS / 1_000_000) as u64 + 8_000;
/// Within this of the edge when playback first starts: the viewer is watching live.
const NEAR_MS: u64 = 60_000;

#[derive(Debug, Default)]
pub struct LiveKeep {
    on: bool,
    /// Undecided until the stream first plays.
    keep: Option<bool>,
    was_playing: bool,
}

impl LiveKeep {
    pub fn new(on: bool) -> Self {
        LiveKeep { on, keep: None, was_playing: false }
    }

    /// Every frame. `playing`: playing and not waiting for data. Returns the position to seek to.
    pub fn tick(&mut self, live: bool, playing: bool, position_ms: u64, edge_ms: u64) -> Option<u64> {
        let resumed = playing && !self.was_playing;
        self.was_playing = playing;
        if !self.on || !live || edge_ms == 0 || !playing {
            return None;
        }
        let behind = edge_ms.saturating_sub(position_ms);
        let keep = *self.keep.get_or_insert(behind < NEAR_MS.max(BEHIND_MS));
        (resumed && keep && behind > BEHIND_MS).then(|| edge_ms.saturating_sub((LIVE_DELAY_NS / 1_000_000) as u64))
    }

    /// The viewer seeked; `to_edge`: to the live edge.
    pub fn seeked(&mut self, to_edge: bool) {
        self.keep = Some(self.on && to_edge);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DELAY: u64 = (LIVE_DELAY_NS / 1_000_000) as u64;

    #[test]
    fn falls_behind_and_jumps_back_on_resume() {
        let mut k = LiveKeep::new(true);
        assert_eq!(k.tick(true, true, 100_000, 100_000 + DELAY), None);
        // A pause of a minute, then play again.
        assert_eq!(k.tick(true, false, 100_000, 160_000 + DELAY), None);
        assert_eq!(k.tick(true, true, 100_000, 160_000 + DELAY), Some(160_000));
    }

    #[test]
    fn off_or_started_far_behind_never_jumps() {
        let mut off = LiveKeep::new(false);
        off.tick(true, true, 100_000, 100_000 + DELAY);
        off.tick(true, false, 100_000, 200_000);
        assert_eq!(off.tick(true, true, 100_000, 200_000), None);

        let mut far = LiveKeep::new(true);
        far.tick(true, true, 0, 3_600_000);
        far.tick(true, false, 0, 3_600_000);
        assert_eq!(far.tick(true, true, 0, 3_700_000), None);
    }

    #[test]
    fn a_seek_back_stops_it_and_a_seek_to_the_edge_restarts_it() {
        let mut k = LiveKeep::new(true);
        k.tick(true, true, 100_000, 100_000 + DELAY);
        k.seeked(false);
        k.tick(true, false, 50_000, 100_000 + DELAY);
        assert_eq!(k.tick(true, true, 50_000, 100_000 + DELAY), None);
        k.seeked(true);
        k.tick(true, false, 100_000, 160_000 + DELAY);
        assert_eq!(k.tick(true, true, 100_000, 160_000 + DELAY), Some(160_000));
    }

    #[test]
    fn a_video_that_is_not_live_is_left_alone() {
        let mut k = LiveKeep::new(true);
        k.tick(false, true, 0, 0);
        k.tick(false, false, 0, 0);
        assert_eq!(k.tick(false, true, 0, 600_000), None);
    }
}

//! "Same volume for every video": each pipeline's volume set from the video's loudness.
//!
//! Both engines take a volume of 0..100 for their own pipeline (engine A: uMediaServer's
//! `setVolume` on its media id; engine B: StarfishMediaAPIs `setVolume`), which leaves the TV's
//! own volume alone: a loud video plays quieter, and the next one starts from the full level
//! again. The rule is YouTube's (`yt_core::loudness`): turn loud videos down, never quiet ones up.

/// Which pipeline is playing: the player's start number and the engine (`A` system player,
/// `B` engine B). A new pipeline starts at its full volume.
pub type Pipeline = (u64, char);

/// The pipeline volume (1..=100) that plays a video of this loudness at YouTube's reference level.
pub fn volume_for(loudness_db: f32) -> i32 {
    ((yt_core::loudness::gain_factor(loudness_db) * 100.0).round() as i32).clamp(1, 100)
}

/// Remembers the volume sent to the current pipeline, so the volume is sent once per pipeline and
/// again only when the option or the video's loudness changes.
#[derive(Default)]
pub struct Leveller {
    applied: Option<(Pipeline, i32)>,
}

impl Leveller {
    /// Every frame: the volume to send now, if any. A pipeline that is not levelled is left at its
    /// own volume; one that was turned down goes back to 100 when the option is switched off.
    pub fn step(&mut self, pipeline: Option<Pipeline>, enabled: bool, loudness_db: Option<f32>) -> Option<i32> {
        let Some(p) = pipeline else {
            self.applied = None;
            return None;
        };
        let applied = match self.applied {
            Some((q, v)) if q == p => v,
            _ => 100,
        };
        let want = match loudness_db {
            Some(db) if enabled => volume_for(db),
            _ => 100,
        };
        self.applied = Some((p, want));
        (want != applied).then_some(want)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_follows_youtubes_rule() {
        assert_eq!(volume_for(0.0), 100);
        assert_eq!(volume_for(-6.0), 100);
        assert_eq!(volume_for(6.0), 50);
        assert_eq!(volume_for(3.5), 67);
        assert_eq!(volume_for(20.0), 10);
        assert_eq!(volume_for(90.0), 1);
    }

    #[test]
    fn sends_once_per_pipeline_and_on_changes() {
        let mut l = Leveller::default();
        // Nothing playing yet; then a loud video on engine A.
        assert_eq!(l.step(None, true, Some(6.0)), None);
        assert_eq!(l.step(Some((1, 'A')), true, Some(6.0)), Some(50));
        assert_eq!(l.step(Some((1, 'A')), true, Some(6.0)), None);
        // Switched off: back to full; on again: down again.
        assert_eq!(l.step(Some((1, 'A')), false, Some(6.0)), Some(100));
        assert_eq!(l.step(Some((1, 'A')), false, Some(6.0)), None);
        assert_eq!(l.step(Some((1, 'A')), true, Some(6.0)), Some(50));
        // The next video on engine B: its own pipeline, sent again.
        assert_eq!(l.step(Some((2, 'B')), true, Some(6.0)), Some(50));
        // A quiet video or one without a loudness leaves its new pipeline alone.
        assert_eq!(l.step(Some((3, 'B')), true, Some(-2.0)), None);
        assert_eq!(l.step(Some((4, 'A')), true, None), None);
        // Off from the start: nothing is sent.
        assert_eq!(l.step(Some((5, 'A')), false, Some(9.0)), None);
    }
}

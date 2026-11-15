//! **Pointer edge glide**, modelled on plx-native's `app::input::EdgeScroll` (GPL-3.0-or-later):
//! a pointer resting in an edge band moves the page after a short dwell, eases in, and goes faster
//! the nearer it is to the very edge, so creeping toward the edge feels like pushing harder.

/// Rest time in a band before anything moves.
pub const DWELL_MS: u32 = 150;
/// The ease-in to full speed after the dwell.
pub const RAMP_MS: u32 = 350;
/// The speed at the band's inner border, as a share of the speed at the very edge.
pub const MIN_SHARE: f32 = 0.25;

/// One axis of the glide: which way it runs and since when.
#[derive(Default)]
pub struct Glide {
    since: Option<(i8, u32)>,
    last: Option<u32>,
    step_due: f32,
}

impl Glide {
    /// One frame at `now` (ms). `side` is the band the pointer is in (`dir`, distance from the edge,
    /// band width), or `None`. Returns the speed share (0..1, already eased in) and the frame's
    /// seconds, or `None` while there is nothing to move.
    pub fn frame(&mut self, side: Option<(i8, f32, f32)>, now: u32) -> Option<(i8, f32, f32)> {
        let dt = self.last.map_or(0.0, |l| now.wrapping_sub(l).min(100) as f32 / 1000.0);
        self.last = Some(now);
        let Some((dir, from_edge, band)) = side else {
            self.since = None;
            return None;
        };
        let since = match self.since {
            Some((d, s)) if d == dir => s,
            _ => {
                self.since = Some((dir, now));
                self.step_due = 0.0;
                now
            }
        };
        let t = now.wrapping_sub(since);
        if t < DWELL_MS {
            return None;
        }
        let ramp = ((t - DWELL_MS) as f32 / RAMP_MS as f32).min(1.0);
        let ramp = ramp * ramp * (3.0 - 2.0 * ramp);
        Some((dir, share(from_edge, band) * ramp, dt))
    }

    /// For lists that move by whole rows: whether a row step is due this frame, `rows_per_s` at the
    /// very edge. The first step comes right after the dwell.
    pub fn step(&mut self, side: Option<(i8, f32, f32)>, now: u32, rows_per_s: f32) -> Option<i8> {
        let (dir, _, dt) = self.frame(side, now)?;
        if self.step_due == 0.0 {
            self.step_due = f32::EPSILON;
            return Some(dir);
        }
        let (_, from_edge, band) = side?;
        // Steps without the ease-in, so the first ones do not lag; nearness still sets the pace.
        self.step_due += rows_per_s * share(from_edge, band) * dt;
        if self.step_due >= 1.0 {
            self.step_due -= 1.0;
            return Some(dir);
        }
        None
    }
}

/// The speed share for a pointer `from_edge` px inside a band `band` px wide.
pub fn share(from_edge: f32, band: f32) -> f32 {
    let depth = (1.0 - from_edge.max(0.0) / band).clamp(0.0, 1.0);
    MIN_SHARE + (1.0 - MIN_SHARE) * depth * depth.sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearer_the_edge_is_faster_and_nothing_moves_during_the_dwell() {
        assert!(share(0.0, 50.0) > 0.99 && (share(50.0, 50.0) - MIN_SHARE).abs() < 1e-6);
        assert!(share(10.0, 50.0) > share(30.0, 50.0));
        let mut g = Glide::default();
        assert!(g.frame(Some((1, 0.0, 50.0)), 1000).is_none());
        assert!(g.frame(Some((1, 0.0, 50.0)), 1100).is_none());
        let (_, s1, _) = g.frame(Some((1, 0.0, 50.0)), 1200).unwrap();
        let (_, s2, _) = g.frame(Some((1, 0.0, 50.0)), 1600).unwrap();
        assert!(s1 < s2 && s2 > 0.99, "eases in: {s1} {s2}");
        assert!(g.frame(None, 1616).is_none());
        assert!(g.frame(Some((1, 0.0, 50.0)), 1632).is_none(), "leaving the band restarts the dwell");
    }

    #[test]
    fn row_steps_come_faster_nearer_the_edge() {
        let count = |from_edge: f32| {
            let mut g = Glide::default();
            (0..200).filter(|i| g.step(Some((-1, from_edge, 50.0)), 1000 + i * 16, 6.0).is_some()).count()
        };
        let (far, near) = (count(45.0), count(0.0));
        assert!(near > far * 2 && far >= 2, "near {near}, far {far}");
    }
}

//! The Windows simulator's playback engine: no media pipeline and no video plane. `start` resolves
//! the video exactly like the TV engine (title, author, duration, captions, storyboard, then the
//! extras: watch info, SponsorBlock, dislikes, captions), then runs a fake clock that advances the
//! position at the playback rate so the HUD, seeking, chapters, SponsorBlock and menus can be
//! exercised. Nothing is reported to YouTube's watch history from the simulator.

use crate::player::{load_extras, ExtrasPrefs, Phase, Status};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const TICK_MS: u64 = 100;

pub struct Player {
    pub status: Arc<Mutex<Status>>,
    active: bool,
    /// Bumped on every start/stop; a clock thread exits when it no longer owns the generation.
    generation: Arc<AtomicU64>,
}

impl Player {
    pub fn new() -> Self {
        Player { status: Default::default(), active: false, generation: Default::default() }
    }

    /// A playback session exists.
    pub fn active(&self) -> bool {
        self.active
    }

    pub fn start(
        &mut self,
        video_id: String,
        session: Arc<Mutex<Option<yt_core::session::Session>>>,
        prefs: yt_core::streams::Prefs,
        extras: ExtrasPrefs,
        start_ms: Option<u64>,
    ) {
        self.stop();
        self.active = true;
        *self.status.lock().unwrap() = Status { phase: Phase::Resolving, rate: 1.0, ..Default::default() };
        let gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let (status, generation) = (self.status.clone(), self.generation.clone());
        let max_h = prefs.max_height;
        let spawned = plx_base::task::spawn("play", move || {
            let r = crate::host::with_session(&session, |s| s.resolve(&video_id));
            if generation.load(Ordering::SeqCst) != gen {
                return;
            }
            match r {
                Ok(resolved) => {
                    {
                        let mut s = status.lock().unwrap();
                        s.title = resolved.title.clone();
                        s.author = resolved.author.clone();
                        s.duration_ms = resolved.length_secs * 1000;
                        s.video_id = video_id.clone();
                        s.channel_id = resolved.channel_id.clone();
                        s.storyboard = resolved.storyboard_spec.clone();
                        s.captions = resolved.captions.clone();
                        s.height = max_h.min(1080) as u32;
                        s.width = s.height * 16 / 9;
                        s.fps = 30;
                        s.position_ms = start_ms.filter(|ms| *ms > 2000).unwrap_or(0);
                        s.phase = Phase::Playing;
                    }
                    crate::log(&format!("sim player: {video_id} resolved; fake clock running (no video)"));
                    plx_machine::present::wake_from_worker();
                    let (st2, g2) = (status.clone(), generation.clone());
                    let _ = plx_base::task::spawn("simclock", move || clock(&st2, &g2, gen));
                    load_extras(&video_id, &session, &status, &extras);
                }
                Err(e) => {
                    crate::log(&format!("sim player: {e:#}"));
                    status.lock().unwrap().phase = Phase::Failed(format!("{e:#}"));
                }
            }
            plx_machine::present::wake_from_worker();
        });
        if spawned.is_none() {
            self.status.lock().unwrap().phase = Phase::Failed("could not start the player thread".into());
        }
    }

    pub fn toggle_pause(&mut self) {
        let mut s = self.status.lock().unwrap();
        s.phase = match s.phase {
            Phase::Playing => Phase::Paused,
            Phase::Paused => Phase::Playing,
            ref p => p.clone(),
        };
    }

    pub fn seek_to(&mut self, to_ms: u64) {
        let pos = self.status.lock().unwrap().position_ms as i64;
        self.seek_by(to_ms as i64 - pos);
    }

    pub fn set_rate(&mut self, rate: f32) {
        self.status.lock().unwrap().rate = rate;
    }

    pub fn seek_by(&mut self, delta_ms: i64) {
        let mut s = self.status.lock().unwrap();
        if !matches!(s.phase, Phase::Playing | Phase::Paused | Phase::Ended) {
            return;
        }
        s.position_ms = (s.position_ms as i64 + delta_ms).clamp(0, (s.duration_ms as i64 - 1000).max(0)) as u64;
        if s.phase == Phase::Ended {
            s.phase = Phase::Playing;
        }
    }

    pub fn stop(&mut self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.active = false;
        self.status.lock().unwrap().phase = Phase::Idle;
    }
}

/// Worker: advances the position while playing, ends at the duration.
fn clock(status: &Mutex<Status>, generation: &AtomicU64, gen: u64) {
    let mut last_sec = u64::MAX;
    while generation.load(Ordering::SeqCst) == gen {
        std::thread::sleep(std::time::Duration::from_millis(TICK_MS));
        let mut s = status.lock().unwrap();
        if s.phase != Phase::Playing {
            continue;
        }
        s.position_ms += (TICK_MS as f32 * s.rate.max(0.0)) as u64;
        if s.duration_ms > 0 && s.position_ms >= s.duration_ms {
            s.position_ms = s.duration_ms;
            s.phase = Phase::Ended;
        }
        let sec = s.position_ms / 1000;
        drop(s);
        if sec != last_sec {
            last_sec = sec;
            plx_machine::present::wake_from_worker();
        }
    }
}

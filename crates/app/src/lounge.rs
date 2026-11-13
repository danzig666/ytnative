//! "Link with TV code": runs YouTube's Lounge session on a worker so a phone can cast to and
//! control the app. Commands arrive here and are turned into navigation / player actions by the
//! main loop; playback state is reported back to the phone.

use crate::player::Phase;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use yt_core::lounge::{Command, DpadKey, Event, LoungeScreen, NowPlaying, PlayerState, Status};

pub struct Lounge {
    commands: Receiver<Command>,
    events: Sender<Event>,
    screen: LoungeScreen,
    /// Pairing code for the settings page ("xxx xxx xxx xxx").
    pub code: Arc<Mutex<Option<String>>>,
    /// DIAL server (phone cast menu); `None` when off or the ports are taken.
    _dial: Option<yt_core::lounge::dial::DialServer>,
    last_report_ms: u32,
    last_phase: Phase,
    last_video: String,
}

/// What a command asks the main loop to do.
pub enum Action {
    Play { video_id: String, start_ms: Option<u64>, playlist: Option<String> },
    TogglePause(bool),
    Seek(u64),
    Stop,
    Key(DpadKey),
    None,
}

impl Lounge {
    pub fn start(device_name: &str, dial: bool) -> anyhow::Result<Lounge> {
        let http = yt_core::net::builder().timeout(std::time::Duration::from_secs(90)).build()?;
        let screen = LoungeScreen::new(http, device_name)?;
        let (cmd_tx, commands) = channel();
        let (events, ev_rx) = channel();
        let code = Arc::new(Mutex::new(None));
        let (runner, coder, code_slot) = (screen.clone(), screen.clone(), code.clone());
        let dial_tx = cmd_tx.clone();
        plx_base::task::spawn("lounge", move || runner.run(cmd_tx, ev_rx));
        let dial = if dial { start_dial(&screen, device_name, dial_tx) } else { None };
        // Keep a fresh pairing code available while the app runs (codes expire after ~5 min).
        plx_base::task::spawn("lounge-code", move || loop {
            match coder.pairing_code() {
                Ok(c) => *code_slot.lock().unwrap() = Some(c),
                Err(e) => crate::log(&format!("lounge: pairing code: {e:#}")),
            }
            std::thread::sleep(std::time::Duration::from_secs(60));
        });
        Ok(Lounge { commands, events, screen, code, _dial: dial, last_report_ms: 0, last_phase: Phase::Idle, last_video: String::new() })
    }

    pub fn stop(&self) {
        self.screen.stop();
    }

    /// Main thread, every frame: next command, if any, as an action.
    pub fn poll(&mut self, status: &crate::player::Status) -> Action {
        let Ok(cmd) = self.commands.try_recv() else { return Action::None };
        crate::log(&format!("lounge: {cmd:?}"));
        match cmd {
            // A queue change for the video already playing: keep playing it.
            Command::UpdatePlaylist(p) if p.video_id == status.video_id => Action::None,
            Command::SetPlaylist(p) | Command::UpdatePlaylist(p) if !p.video_id.is_empty() => Action::Play {
                video_id: p.video_id,
                start_ms: p.current_time.map(|t| (t * 1000.0) as u64),
                playlist: p.list_id,
            },
            Command::SetVideo { video_id, current_time } => Action::Play { video_id, start_ms: current_time.map(|t| (t * 1000.0) as u64), playlist: None },
            Command::Play => Action::TogglePause(true),
            Command::Pause => Action::TogglePause(false),
            Command::SeekTo(t) => Action::Seek((t * 1000.0) as u64),
            Command::StopVideo => Action::Stop,
            Command::Dpad(k) => Action::Key(k),
            Command::GetNowPlaying | Command::RemoteConnected(_) => {
                self.report(status, true);
                Action::None
            }
            _ => Action::None,
        }
    }

    /// Main thread, every frame: tell the phones what is playing (on change, else every 3 s).
    pub fn tick(&mut self, status: &crate::player::Status, now_ms: u32) {
        let changed = status.phase != self.last_phase || status.video_id != self.last_video;
        if changed || (status.phase == Phase::Playing && now_ms.wrapping_sub(self.last_report_ms) > 3000) {
            self.report(status, changed);
            self.last_report_ms = now_ms;
            self.last_phase = status.phase.clone();
            self.last_video = status.video_id.clone();
        }
    }

    fn report(&self, s: &crate::player::Status, now_playing: bool) {
        let state = match s.phase {
            Phase::Playing => PlayerState::Playing,
            Phase::Paused => PlayerState::Paused,
            Phase::Ended => PlayerState::Ended,
            Phase::Resolving | Phase::Loading => PlayerState::Buffering,
            _ => PlayerState::Unstarted,
        };
        let st = Status {
            state,
            current_time: s.position_ms as f64 / 1000.0,
            duration: s.duration_ms as f64 / 1000.0,
            loaded_time: s.position_ms as f64 / 1000.0,
            cpn: None,
        };
        if now_playing {
            let np = (!s.video_id.is_empty()).then(|| NowPlaying { video_id: s.video_id.clone(), status: st.clone(), list_id: None, current_index: None });
            let _ = self.events.send(Event::NowPlaying(np));
        }
        let _ = self.events.send(Event::StateChange(st));
    }
}

/// Starts the DIAL server so the TV shows up in the phone's cast menu. A launch carries a pairing
/// code (registered with the lounge so the phone joins our session) and maybe a video to start.
/// Failures (port 1900 owned by the system DIAL server, sandbox) only disable DIAL.
fn start_dial(screen: &LoungeScreen, device_name: &str, tx: Sender<Command>) -> Option<yt_core::lounge::dial::DialServer> {
    use yt_core::lounge::dial;
    let mut cfg = dial::DialConfig::new(device_name, &screen.device_id());
    cfg.screen_id = Some(screen.screen_id());
    let screen = screen.clone();
    let tx = Mutex::new(tx);
    let r = dial::start(cfg.clone(), move |req| {
        if let Some(code) = &req.pairing_code {
            if let Err(e) = screen.register_pairing_code(code) {
                crate::log(&format!("dial: register pairing code: {e:#}"));
            }
        }
        if let Some(v) = req.video_id.filter(|v| !v.is_empty()) {
            let _ = tx.lock().unwrap().send(Command::SetVideo { video_id: v, current_time: req.start_time });
        }
    });
    match r {
        Ok(s) => {
            crate::log(&format!("dial: listening on :{}", s.http_port()));
            Some(s)
        }
        Err(e) => {
            crate::log(&format!("dial: not started: {e:#}"));
            // Without SSDP the phone cannot discover us; HTTP alone is pointless.
            None
        }
    }
}

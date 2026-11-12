//! ytnative: native YouTube client for LG webOS.
//!
//! Boot and frame loop follow plx-native's `app/boot.rs` + `app/run.rs` (GPL-3.0-or-later):
//! SDL + GLES2 window, transparent UI plane over the video plane, the plx_ui dispatcher.

mod feed;
mod host;
mod images;
mod input;
mod lounge;
mod ls2;
mod player;
mod remote;
mod screens;
mod sdl;
mod settings;
mod signin;
mod store;
mod ui;
mod wl;

use host::{AppRig, YtArg, YtHost};
use plx_machine::machine::{MachineId, NavOp, Tick};
use plx_ui::containers::transition::PageDip;
use plx_ui::dispatch::{Dispatcher, NoTap};
use std::ffi::CString;
use std::time::Instant;

static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

pub fn now_us() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// Persistent, writable, survives reinstall in the Developer Mode jail (plx-native's finding);
/// falls back to the app's tmp dir.
fn data_dir() -> std::path::PathBuf {
    let id = plx_base::paths::app_id();
    for d in [format!("/media/developer/{id}-data"), format!("/media/internal/.{id}"), "/tmp/ytnative/data".to_string()] {
        let p = std::path::PathBuf::from(&d);
        if std::fs::create_dir_all(&p).is_ok() && std::fs::write(p.join(".probe"), b"ok").is_ok() {
            return p;
        }
    }
    std::path::PathBuf::from("/tmp")
}

pub fn log(s: &str) {
    plx_base::eventlog::log(s);
    eprintln!("{s}");
}

fn main() {
    START.get_or_init(Instant::now);
    // Before anything SDL: ask webOS to deliver BACK (and EXIT) to the app.
    std::env::set_var("SDL_WEBOS_ACCESS_POLICY_KEYS_BACK", "true");
    std::env::set_var("SDL_WEBOS_ACCESS_POLICY_KEYS_EXIT", "true");
    let _ = plx_platform::tv::install(&wl::PORT);
    if let Some(arg) = std::env::args().nth(1) {
        plx_platform::tv::launch::offer(&arg);
    }
    yt_core::paths::set_data_dir(data_dir());
    log("ytnative: boot");

    unsafe {
        sdl::SDL_SetMainReady();
        let hint = |k: &str, v: &str| {
            let (k, v) = (CString::new(k).unwrap(), CString::new(v).unwrap());
            sdl::SDL_SetHint(k.as_ptr(), v.as_ptr());
        };
        // Let the TV's own screensaver run outside playback (OLED burn-in protection); the
        // media pipeline keeps the panel awake while a video actually plays.
        hint("SDL_VIDEO_ALLOW_SCREENSAVER", "1");
        if sdl::SDL_Init(sdl::SDL_INIT_VIDEO) != 0 {
            log(&format!("SDL_Init failed: {}", sdl::error()));
            std::process::exit(1);
        }
        for (a, v) in [
            (sdl::SDL_GL_CONTEXT_PROFILE_MASK, sdl::SDL_GL_CONTEXT_PROFILE_ES),
            (sdl::SDL_GL_CONTEXT_MAJOR_VERSION, 2),
            (sdl::SDL_GL_CONTEXT_MINOR_VERSION, 0),
            (sdl::SDL_GL_RED_SIZE, 8),
            (sdl::SDL_GL_GREEN_SIZE, 8),
            (sdl::SDL_GL_BLUE_SIZE, 8),
            (sdl::SDL_GL_ALPHA_SIZE, 8),
            (sdl::SDL_GL_BUFFER_SIZE, 32),
            (sdl::SDL_GL_DEPTH_SIZE, 0),
            (sdl::SDL_GL_STENCIL_SIZE, 0),
        ] {
            sdl::SDL_GL_SetAttribute(a, v);
        }
        let title = CString::new("ytnative").unwrap();
        let win = sdl::SDL_CreateWindow(title.as_ptr(), 0, 0, 1920, 1080, sdl::SDL_WINDOW_OPENGL | sdl::SDL_WINDOW_FULLSCREEN);
        if win.is_null() {
            log(&format!("SDL_CreateWindow failed: {}", sdl::error()));
            std::process::exit(1);
        }
        if sdl::SDL_GL_CreateContext(win).is_null() {
            log(&format!("SDL_GL_CreateContext failed: {}", sdl::error()));
            std::process::exit(1);
        }
        plx_base::surface::probe(win);
        sdl::SDL_GL_SetSwapInterval(1);
        plx_platform::tv::window::bind_window(win);
        plx_platform::tv::window::grab(win);

        plx_gfx::gfx::init_gl();
        plx_gfx::text::init_text();
        plx_gfx::gfx::init_image();
        plx_gfx::gfx::init_blur();
        plx_platform::i18n::initialize(plx_platform::i18n::Preference::System, false);
        plx_ui::tex::install(&*images::THUMBS);
        log("ytnative: graphics ready");

        let mut rig = AppRig::new();
        let mut d = Dispatcher::<YtHost>::with_transition(Box::new(PageDip::new()));
        d.request(MachineId::Nav, NavOp::Root(YtArg::Browse));

        let mut remote = remote::Remote::open();
        let mut lounge = if rig.store.settings.lounge_enabled {
            match lounge::Lounge::start(&rig.store.settings.device_name) {
                Ok(l) => Some(l),
                Err(e) => {
                    log(&format!("lounge: not started: {e:#}"));
                    None
                }
            }
        } else {
            None
        };
        let mut ev = [0u8; 128];
        let mut prev = 0u32;
        let mut running = true;
        // OLED protection: blank the screen after IDLE_BLANK_MS without input unless a video is
        // playing. Any key wakes it and is swallowed.
        let mut last_input = sdl::SDL_GetTicks();
        let mut blanked = false;
        let mut foreground = true;
        while running {
            let now = sdl::SDL_GetTicks();
            let dt = if prev == 0 { 0.016 } else { ((now - prev) as f32 / 1000.0).min(0.05) };
            prev = now;
            let tick = Tick { ms: now, dt_us: (dt * 1e6) as u32 };

            let mut inputs = Vec::new();
            while sdl::SDL_PollEvent(ev.as_mut_ptr().cast()) != 0 {
                match input::rd_u32(&ev, 0) {
                    sdl::SDL_QUIT => running = false,
                    sdl::SDL_KEYDOWN | sdl::SDL_KEYUP => {
                        let (state, wcode, sym) = input::decode_key(&ev);
                        if wcode == 505 {
                            running = false; // EXIT key
                        }
                        inputs.push(input::key_input(sym, wcode, state, tick));
                    }
                    sdl::SDL_APP_WILLENTERBACKGROUND | sdl::SDL_APP_DIDENTERBACKGROUND => {
                        if foreground {
                            log("lifecycle: background");
                        }
                        foreground = false;
                        wl::release();
                    }
                    sdl::SDL_APP_DIDENTERFOREGROUND => {
                        log("lifecycle: foreground");
                        foreground = true;
                        wl::grab(win);
                    }
                    _ => {}
                }
            }

            if let Some(r) = remote.as_mut() {
                let mut quit = false;
                inputs.extend(r.poll(tick, &mut quit));
                if quit {
                    running = false;
                }
            }

            if !inputs.is_empty() {
                last_input = now;
                if blanked {
                    blanked = false;
                    inputs.clear(); // the waking key does nothing else
                    plx_machine::idle::invalidate();
                    log("idle: wake");
                }
            }
            let playing = rig.playing();
            if playing {
                last_input = now;
            }
            if !blanked && now.wrapping_sub(last_input) > rig.idle_blank_ms() {
                blanked = true;
                log("idle: blank screen (no input, nothing playing)");
                plx_gfx::gfx::frame_clear(0.0, 0.0, 0.0);
                sdl::SDL_GL_SwapWindow(win);
            }

            if let Some(l) = lounge.as_mut() {
                rig.store.lounge_code = l.code.lock().unwrap().clone();
                let in_player = d.top_screen().map(|s| s.name()) == Some("player");
                match l.poll(&rig.player_view) {
                    lounge::Action::Play { video_id, start_ms, playlist } => {
                        if let Some(ms) = start_ms {
                            rig.pending_start = Some((video_id.clone(), ms));
                        }
                        let arg = YtArg::Player { video_id, playlist };
                        d.request(MachineId::Nav, if in_player { NavOp::Replace(arg) } else { NavOp::Push(arg) });
                        last_input = now;
                    }
                    lounge::Action::TogglePause(want_play) => {
                        let paused = rig.player_view.phase == player::Phase::Paused;
                        if paused == want_play {
                            rig.player.toggle_pause();
                        }
                    }
                    lounge::Action::Seek(ms) => rig.player.seek_to(ms),
                    lounge::Action::Stop => {
                        if in_player {
                            d.request(MachineId::Nav, NavOp::Pop);
                        }
                    }
                    lounge::Action::Key(k) => {
                        use yt_core::lounge::DpadKey as K;
                        const ARROW: u32 = 1 << 30;
                        let (sym, wcode) = match k {
                            K::Up => (82 | ARROW, 0),
                            K::Down => (81 | ARROW, 0),
                            K::Left => (80 | ARROW, 0),
                            K::Right => (79 | ARROW, 0),
                            K::Enter => (13, 0),
                            K::Back => (0, 482),
                        };
                        inputs.push(input::key_input(sym, wcode, 1, tick));
                        inputs.push(input::key_input(sym, wcode, 0, tick));
                    }
                    lounge::Action::None => {}
                }
                l.tick(&rig.player_view, now);
            }

            plx_machine::idle::frame_begin(dt);
            images::THUMBS.deliver();
            let results = rig.drain(now);
            let r = d.frame_with(&mut rig, tick, inputs, results, &mut NoTap, false);
            d.prune(&r.unmounted);
            plx_ui::tex::note_queued(&mut d.budget);

            let present = foreground && !blanked && (r.presented || plx_machine::idle::should_present(now) || plx_ui::tex::has_pending());
            if present {
                plx_gfx::text::begin_frame();
                let (x, y, w, h) = plx_base::surface::viewport();
                sdl::glViewport(x, y, w, h);
                // Keep the video plane visible under the player's own overlays and menus.
                let video = rig.player.active()
                    || d.top_screen().map(|s| s.render()) == Some(plx_ui::screen::RenderStrategy::VideoPlane);
                if video {
                    // Alpha 0: the compositor shows the hardware video plane through the UI plane.
                    plx_gfx::gfx::frame_clear_through();
                } else {
                    let c = plx_ui::theme::CLEAR_RGB;
                    plx_gfx::gfx::frame_clear(c.0, c.1, c.2);
                }
                plx_ui::guard(|| {
                    d.draw(&mut rig, true);
                });
                sdl::SDL_GL_SwapWindow(win);
                plx_gfx::gfx::blur_frame_end();
                plx_gfx::gfx::region_frame_end();
                plx_gfx::gfx::snapshot_frame_end();
                plx_gfx::gfx::field_frame_end();
                plx_gfx::gfx::ground_probes_frame_end();
                plx_machine::idle::note_present(now);
            } else {
                plx_gfx::text::warm_fonts_idle(2000, now_us);
                sdl::SDL_Delay(plx_machine::idle::IDLE_POLL_MS);
            }
        }
        log("ytnative: exit");
        sdl::SDL_Quit();
    }
}

//! ytnative: native YouTube client for LG webOS.
//!
//! Boot and frame loop follow plx-native's `app/boot.rs` + `app/run.rs` (GPL-3.0-or-later):
//! SDL + GLES2 window, transparent UI plane over the video plane, the plx_ui dispatcher.

mod crashtrace; // fatal-signal tracer -> /tmp/ytnative/crash.log (see `cargo xtask crash`)
mod host;
mod images;
mod input;
mod i18n;
mod lounge;
// The Windows desktop simulator swaps the TV-only modules (LS2 bus, Wayland surface, FIFO remote)
// for stand-ins under `sim/`; see `sim/mod.rs`.
#[cfg_attr(windows, path = "sim/ls2.rs")]
mod ls2;
mod player;
#[cfg_attr(windows, path = "sim/remote.rs")]
mod remote;
mod screens;
mod sdl;
mod settings;
mod signin;
#[cfg(windows)]
mod sim;
mod store;
mod ui;
#[cfg_attr(windows, path = "sim/wl.rs")]
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
#[cfg(not(windows))]
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

/// Windows simulator: `%LOCALAPPDATA%\ytnative` (token, accounts, settings).
#[cfg(windows)]
fn data_dir() -> std::path::PathBuf {
    let d = sim::base_dir();
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Windows simulator: a 1920x1080 window (`YTNATIVE_SIM_WINDOW=WxH` to change; the UI is scaled
/// to fit) with a desktop GL context. Tries a 4.1 core profile first (what plx-native's macOS
/// simulator uses: the vendored renderer then compiles its GLES2 shaders behind a `#version 410
/// core` preamble and binds a VAO), then an OpenGL ES 2.0 context (shaders as on the TV), then
/// whatever the driver gives by default. Resolves the GL trampolines once a context exists.
#[cfg(windows)]
unsafe fn sim_window() -> *mut std::os::raw::c_void {
    let (w, h) = std::env::var("YTNATIVE_SIM_WINDOW")
        .ok()
        .and_then(|s| s.split_once('x').and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?))))
        .unwrap_or((1920, 1080));
    let mut flags = sdl::SDL_WINDOW_OPENGL;
    if std::env::var_os("YTNATIVE_SIM_HIDDEN").is_some() {
        flags |= sdl::SDL_WINDOW_HIDDEN;
    }
    let title = CString::new("ytnative (simulator)").unwrap();
    let attempts: [(&str, Option<(i32, i32, i32)>); 3] = [
        ("GL 4.1 core", Some((sdl::SDL_GL_CONTEXT_PROFILE_CORE, 4, 1))),
        ("GLES 2.0", Some((sdl::SDL_GL_CONTEXT_PROFILE_ES, 2, 0))),
        ("driver default", None),
    ];
    for (name, profile) in attempts {
        sdl::SDL_GL_ResetAttributes();
        if let Some((mask, major, minor)) = profile {
            sdl::SDL_GL_SetAttribute(sdl::SDL_GL_CONTEXT_PROFILE_MASK, mask);
            sdl::SDL_GL_SetAttribute(sdl::SDL_GL_CONTEXT_MAJOR_VERSION, major);
            sdl::SDL_GL_SetAttribute(sdl::SDL_GL_CONTEXT_MINOR_VERSION, minor);
        }
        for (a, v) in [(sdl::SDL_GL_RED_SIZE, 8), (sdl::SDL_GL_GREEN_SIZE, 8), (sdl::SDL_GL_BLUE_SIZE, 8), (sdl::SDL_GL_ALPHA_SIZE, 8)] {
            sdl::SDL_GL_SetAttribute(a, v);
        }
        let win = sdl::SDL_CreateWindow(title.as_ptr(), sdl::SDL_WINDOWPOS_CENTERED, sdl::SDL_WINDOWPOS_CENTERED, w, h, flags);
        if win.is_null() {
            log(&format!("sim: SDL_CreateWindow failed: {}", sdl::error()));
            std::process::exit(1);
        }
        if !sdl::SDL_GL_CreateContext(win).is_null() {
            log(&format!("sim: {w}x{h} window, {name} context"));
            sim::gl::load();
            return win;
        }
        log(&format!("sim: {name} context refused: {}", sdl::error()));
        sdl::SDL_DestroyWindow(win);
    }
    log("sim: no usable OpenGL context");
    std::process::exit(1);
}

pub fn log(s: &str) {
    plx_base::eventlog::log(s);
    eprintln!("{s}");
}

fn main() {
    crashtrace::install(); // first: every later crash gets a record
    START.get_or_init(Instant::now);
    // Simulator: plx's runtime root (event log, trigger files) under %LOCALAPPDATA%\ytnative\run,
    // before any plx path is resolved (the default is the TV's /tmp).
    #[cfg(windows)]
    if std::env::var_os("PLXNATIVE_RUNTIME_DIR").is_none() {
        let run = sim::base_dir().join("run");
        let _ = std::fs::create_dir_all(&run);
        std::env::set_var("PLXNATIVE_RUNTIME_DIR", run);
    }
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
        #[cfg(windows)]
        let win = sim_window();
        #[cfg(not(windows))]
        let win = {
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
            win
        };
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
        {
            let s = &rig.store.settings;
            if let Err(e) = yt_core::net::configure(yt_core::net::NetConfig { proxy: s.proxy_url.clone(), doh: s.doh.clone() }) {
                log(&format!("net: bad proxy/DoH setting ignored: {e:#}"));
            }
        }
        // The simulator would register a second "TV" for phone casting on the signed-in account;
        // opt in with YTNATIVE_SIM_LOUNGE=1.
        let lounge_on = rig.store.settings.lounge_enabled && (!cfg!(windows) || std::env::var_os("YTNATIVE_SIM_LOUNGE").is_some());
        let mut lounge = if lounge_on {
            match lounge::Lounge::start(&rig.store.settings.device_name, rig.store.settings.dial_enabled) {
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
                        if (403..=406).contains(&wcode) {
                            let act = rig.store.settings.color_keys[(wcode - 403) as usize].clone();
                            if act != "none" {
                                if state & 0xff != 0 && state & 0x100 == 0 {
                                    color_key(&act, &mut rig, &mut d);
                                }
                                continue;
                            }
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
            // No OLED to protect in the simulator, and a blank frame would spoil screenshots.
            if !cfg!(windows) && !blanked && now.wrapping_sub(last_input) > rig.idle_blank_ms() {
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
            // Simulator: a requested screenshot forces a full draw of the current state.
            #[cfg(windows)]
            let present = present || remote.as_ref().is_some_and(|r| r.shot_pending());
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
                #[cfg(windows)]
                if let Some(r) = remote.as_mut() {
                    let (mut dw, mut dh) = (0, 0);
                    sdl::SDL_GL_GetDrawableSize(win, &mut dw, &mut dh);
                    r.take_shots(dw, dh);
                }
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

/// A remapped colour button (settings → Remote control).
fn color_key(act: &str, rig: &mut AppRig, d: &mut Dispatcher<YtHost>) {
    match act {
        "search" => d.request(MachineId::Nav, NavOp::Push(YtArg::Search)),
        "settings" => d.request(MachineId::Nav, NavOp::Push(YtArg::Settings(host::SettingsPage::Root))),
        "accounts" => d.request(MachineId::Nav, NavOp::Push(YtArg::Accounts)),
        "subtitles" => {
            rig.store.settings.subtitles = !rig.store.settings.subtitles;
            rig.store.settings.save();
            let msg = if rig.store.settings.subtitles { "Subtitles on" } else { "Subtitles off" };
            rig.store.toast = Some((msg.into(), rig.now_ms + 2500));
        }
        "speed" => {
            const R: [f32; 7] = [0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 0.5];
            let cur = rig.player_view.rate;
            let i = R.iter().position(|r| (r - cur).abs() < 0.01).map_or(1, |i| (i + 1) % R.len());
            rig.player.set_rate(R[i]);
            rig.store.toast = Some((format!("{}x", R[i]), rig.now_ms + 2500));
        }
        _ => {}
    }
}

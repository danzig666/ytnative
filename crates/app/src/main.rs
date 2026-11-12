//! ytnative: native YouTube client for LG webOS.
//!
//! Boot and frame loop follow plx-native's `app/boot.rs` + `app/run.rs` (GPL-3.0-or-later):
//! SDL + GLES2 window, transparent UI plane over the video plane, the plx_ui dispatcher.

mod host;
mod input;
mod screens;
mod sdl;
mod wl;

use host::{AppRig, Card, YtArg, YtHost};
use plx_machine::machine::{MachineId, NavOp, Tick};
use plx_ui::containers::transition::PageDip;
use plx_ui::dispatch::{Dispatcher, NoTap};
use std::ffi::CString;
use std::time::Instant;

static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

pub fn now_us() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_micros() as u64
}

fn log(s: &str) {
    plx_base::eventlog::log(s);
    eprintln!("{s}");
}

fn demo_cards() -> Vec<Card> {
    (1..=24)
        .map(|i| Card {
            elem: i,
            title: format!("Video {i}"),
            detail: "Channel name \u{2022} 1.2M views".into(),
            thumb: String::new(),
        })
        .collect()
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
    log("ytnative: boot");

    unsafe {
        sdl::SDL_SetMainReady();
        let hint = |k: &str, v: &str| {
            let (k, v) = (CString::new(k).unwrap(), CString::new(v).unwrap());
            sdl::SDL_SetHint(k.as_ptr(), v.as_ptr());
        };
        hint("SDL_VIDEO_ALLOW_SCREENSAVER", "0");
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
        log("ytnative: graphics ready");

        let mut rig = AppRig::new(demo_cards());
        let mut d = Dispatcher::<YtHost>::with_transition(Box::new(PageDip::new()));
        d.request(MachineId::Nav, NavOp::Root(YtArg::Home));

        let mut ev = [0u8; 128];
        let mut prev = 0u32;
        let mut running = true;
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

            plx_machine::idle::frame_begin(dt);
            let results = std::mem::take(&mut *rig.inbox.lock().unwrap());
            let r = d.frame_with(&mut rig, tick, inputs, results, &mut NoTap, false);
            d.prune(&r.unmounted);
            plx_ui::tex::note_queued(&mut d.budget);

            let present = foreground && (r.presented || plx_machine::idle::should_present(now));
            if present {
                plx_gfx::text::begin_frame();
                let (x, y, w, h) = plx_base::surface::viewport();
                sdl::glViewport(x, y, w, h);
                let c = plx_ui::theme::CLEAR_RGB;
                plx_gfx::gfx::frame_clear(c.0, c.1, c.2);
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

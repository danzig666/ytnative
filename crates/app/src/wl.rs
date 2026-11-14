//! Wayland surface handling and the LS2 pump, adapted from plx-native's `src/system.rs`
//! (GPL-3.0-or-later). The UI plane must have an empty opaque region, or the compositor treats it
//! as opaque and the video plane beneath it never shows.

use crate::sdl;
use plx_base::eventlog::log;
use std::os::raw::{c_int, c_uint, c_void};
use std::sync::atomic::{AtomicPtr, Ordering};

extern "C" {
    fn wl_proxy_marshal(proxy: *mut c_void, opcode: c_uint, ...);
    fn g_main_context_pending(ctx: *mut c_void) -> c_int;
    fn g_main_context_iteration(ctx: *mut c_void, may_block: c_int) -> c_int;
}

const WL_SURFACE_SET_OPAQUE_REGION: c_uint = 4;
const SDL_SYSWM_WAYLAND: i32 = 6;

static SURFACE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static DISPLAY: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Marks the whole surface non-opaque. Takes effect at the next buffer commit (the GL swap).
pub fn clear_opaque_region() {
    let s = SURFACE.load(Ordering::Relaxed);
    if !s.is_null() {
        unsafe { wl_proxy_marshal(s, WL_SURFACE_SET_OPAQUE_REGION, std::ptr::null_mut::<c_void>()) };
    }
}

/// Drops the borrowed Wayland handles (the app is going to the background).
pub fn release() -> bool {
    let had = !SURFACE.swap(std::ptr::null_mut(), Ordering::Relaxed).is_null();
    DISPLAY.store(std::ptr::null_mut(), Ordering::Relaxed);
    if had {
        log("wl: released borrowed Wayland handles");
    }
    had
}

/// Reads wl_display / wl_surface out of SDL_SysWMinfo. The version handshake must use the RUNTIME
/// SDL version (SDL >= 2.0.6 refuses older callers), and the buffer is over-allocated because LG's
/// fork writes more than upstream headers declare.
pub fn grab(win: *mut c_void) {
    release();
    let mut info = [0u8; 512];
    unsafe {
        sdl::SDL_GetVersion(info.as_mut_ptr());
        let ok = sdl::SDL_GetWindowWMInfo(win, info.as_mut_ptr() as *mut c_void);
        let subsystem = i32::from_ne_bytes(info[4..8].try_into().unwrap());
        if ok != 0 && subsystem == SDL_SYSWM_WAYLAND {
            let p = info.as_ptr().add(8) as *const *mut c_void;
            let display = p.read_unaligned();
            let surface = p.add(1).read_unaligned();
            if !display.is_null() && !surface.is_null() {
                DISPLAY.store(display, Ordering::Relaxed);
                SURFACE.store(surface, Ordering::Relaxed);
            }
        }
        // The framebuffer's alpha: without it a clear to alpha 0 is no hole for the video plane.
        let mut alpha: c_int = -1;
        sdl::glGetIntegerv(sdl::GL_ALPHA_BITS, &mut alpha);
        log(&format!(
            "wl: sdl={}.{}.{} wm_ok={ok} subsystem={subsystem} surface={:p} alpha_bits={alpha}",
            info[0],
            info[1],
            info[2],
            SURFACE.load(Ordering::Relaxed)
        ));
        if SURFACE.load(Ordering::Relaxed).is_null() {
            log("wl: NO wl_surface: the UI plane cannot be made transparent; video would play invisibly beneath it");
        }
    }
    clear_opaque_region();
}

/// Dispatches pending default-context GLib work: SDL's registerApp replies (app lifecycle) are
/// delivered here.
pub fn ls2_pump() {
    unsafe {
        let mut guard = 8;
        while guard > 0 && g_main_context_pending(std::ptr::null_mut()) != 0 {
            g_main_context_iteration(std::ptr::null_mut(), 0);
            guard -= 1;
        }
    }
}

fn nothing() {}

/// Our `tv::Port`: the window/surface/bus entries are ours, Home comes from plx's webOS glue, the
/// rest (Plex storage, Starfish feed sink, toasts) stays at the library's no-op defaults.
pub static PORT: plx_platform::tv::Port = plx_platform::tv::Port {
    probe_device: nothing,
    start_capability_probe: nothing,
    repair_sandbox: || Err(plx_platform::tv::sandbox::Failure::Unsupported),
    seal: |_| None,
    open: |_| None,
    remove: |_, _| {},
    system_locale: || plx_platform::tv::LocaleReply::NoPlatform,
    sound_output: || plx_platform::tv::sound::Reply::NoPlatform,
    go_home: plx_platform::webos::go_home,
    poll_home: plx_platform::webos::poll_home,
    deliver_toast: plx_platform::tv::toast::deliver_without_port,
    bind_window: |_| {},
    grab_surface: grab,
    release_surface: release,
    arm_opaque_region: nothing,
    opaque_route: |_| {},
    clear_opaque_region,
    pump_bus: ls2_pump,
    frame_probe_request: nothing,
    frame_probe_prepare_begin: nothing,
    frame_probe_prepare_end: nothing,
    frame_probe_waiting: nothing,
    frame_probe_acquired: nothing,
    frame_probe_fields: String::new,
    sink: &plx_platform::tv::sink::NoSink,
    display_sink: &plx_platform::tv::sink::NoSink,
    uri_media: &plx_platform::tv::uri::NoUriMedia,
};

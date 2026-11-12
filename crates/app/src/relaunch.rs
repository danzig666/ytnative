//! Warm-launch parameters (a link sent while the app already runs).
//!
//! Ported from plx-native (`rust-modules/src/app/boot.rs`, `tap_relaunch_payloads`, GPL-3.0,
//! © Gleb Linnik and contributors). SAM sends a relaunch's parameters only to the `registerApp`
//! subscriber, which is LG's SDL; its handler raises the window and never reads `parameters`. It
//! reads every reply's payload through `HELPER_HLunaServiceMessage`, an exported pointer slot that
//! `SDL_Init` fills from `libhelpers.so.2` and the handler reloads on every call. Wrapping that slot
//! hands a relaunch payload to `plx_platform::tv::launch` and returns it unchanged, so SDL still
//! raises the window. Installed only when the slot holds exactly libhelpers' own
//! `HLunaServiceMessage`; anything else is left alone (webOS 4-6 have no slot: cold links only).

use std::sync::atomic::{AtomicUsize, Ordering};

type LunaMessageFn = unsafe extern "C" fn(*mut std::ffi::c_void) -> *const std::ffi::c_char;
static REAL_MESSAGE: AtomicUsize = AtomicUsize::new(0);

/// Call once, after `SDL_Init`.
pub fn install() {
    unsafe { tap() }
}

unsafe fn tap() {
    let Some(slot) = plx_base::dynlib::Handle::self_handle().sym("HELPER_HLunaServiceMessage").filter(|p| !p.is_null()) else {
        crate::log("relaunch tap: no HELPER_HLunaServiceMessage in this SDL");
        return;
    };
    if (slot as usize) % std::mem::align_of::<usize>() != 0 {
        crate::log("relaunch tap: slot misaligned, not installed");
        return;
    }
    // SDL's Luna thread may read the slot while it is replaced: store atomically, after the
    // function it forwards to is published.
    let slot = &*(slot as *const AtomicUsize);
    let wrapped = wrapped_message as *const () as usize;
    let current = slot.load(Ordering::Acquire);
    if current == 0 || current == wrapped {
        crate::log(if current == 0 { "relaunch tap: slot is empty" } else { "relaunch tap: already installed" });
        return;
    }
    let expected = plx_base::dynlib::Handle::open_loaded(&["libhelpers.so.2"])
        .and_then(|(h, _)| h.sym("HLunaServiceMessage"))
        .filter(|p| !p.is_null())
        .map(|p| p as usize);
    if expected != Some(current) {
        crate::log("relaunch tap: slot does not hold libhelpers' HLunaServiceMessage, not installed");
        return;
    }
    REAL_MESSAGE.store(current, Ordering::Release);
    slot.store(wrapped, Ordering::Release);
    crate::log("relaunch tap: installed");
}

/// SDL's payload read, passed through; a relaunch's payload is also queued for the main loop.
unsafe extern "C" fn wrapped_message(msg: *mut std::ffi::c_void) -> *const std::ffi::c_char {
    let real = REAL_MESSAGE.load(Ordering::Acquire);
    if real == 0 {
        return std::ptr::null();
    }
    let real: LunaMessageFn = std::mem::transmute(real);
    let p = real(msg);
    if !p.is_null() {
        let payload = std::ffi::CStr::from_ptr(p).to_string_lossy();
        if payload.contains("\"relaunch\"") {
            plx_platform::tv::launch::offer(&payload);
            plx_machine::present::wake_from_worker();
        }
    }
    p
}

//! FFI to `csrc/starfish.c` (the StarfishMediaAPIs shim) and the library-thread event entry.
//!
//! Library callbacks arrive on LG's threads through `sf_on_event`, tagged with the epoch the Load
//! was made with. They are routed to the [`Events`] of the live epoch under one mutex held for the
//! whole callback, so teardown (which clears the route) can never race a callback that already
//! validated an old epoch into a new session (plx-native's rule).

use std::ffi::{c_char, c_int, c_uint, CStr};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering::*};
use std::sync::{Arc, Mutex};

/// Sentinel for "no position reported since the last Load/seek".
pub const PRES_NONE: i64 = i64::MIN;

/// Callback types (StarfishMediaAPIs / libpf `PF_EVENT_TYPE_*`). Types above 0x1c are numbered
/// two higher on webOS 5+ than on 4.x; the ones used here are at or below it.
pub const EV_FRAMEREADY: c_int = 0;
pub const EV_ERROR: c_int = 18;
pub const EV_UNLOAD_COMPLETED: c_int = 23;
pub const EV_END_OF_STREAM: c_int = 28;

#[derive(Default)]
pub struct Events {
    /// Fed-timeline PTS (ns) of the last FRAMEREADY (a ~5 Hz position tick, not one per frame).
    pub pres_fed_ns: AtomicI64,
    pub frames: AtomicU64,
    pub eos: AtomicBool,
    pub load_completed: AtomicBool,
    pub error: Mutex<Option<(i64, String)>>,
    /// webOS 4 (ACB): the pipeline's media id, the first callback string's `context` / `mediaId`.
    pub media_id: Mutex<Option<String>>,
    /// webOS 4 (ACB): the first `{"video":…,"context":…}` envelope, verbatim, for setMediaVideoData.
    pub source_info: Mutex<Option<std::ffi::CString>>,
    /// The pipeline's `video.hdrType` ("none", "HDR10", "HLG", ...), taken by the engine.
    pub hdr: Mutex<Option<String>>,
}

impl Events {
    pub fn new() -> Arc<Events> {
        let e = Events::default();
        e.pres_fed_ns.store(PRES_NONE, Relaxed);
        Arc::new(e)
    }
}

static ROUTE: Mutex<Option<(u32, Arc<Events>)>> = Mutex::new(None);
static NEXT_EPOCH: AtomicU32 = AtomicU32::new(1);

/// A fresh epoch, routed to `ev` until [`unroute`].
pub fn route(ev: Arc<Events>) -> u32 {
    let epoch = NEXT_EPOCH.fetch_add(1, Relaxed).max(1);
    *ROUTE.lock().unwrap() = Some((epoch, ev));
    epoch
}

pub fn unroute(epoch: u32) {
    let mut r = ROUTE.lock().unwrap();
    if r.as_ref().is_some_and(|(e, _)| *e == epoch) {
        *r = None;
    }
}

#[no_mangle]
pub extern "C" fn sf_on_event(epoch: c_uint, ty: c_int, num: i64, s: *const c_char) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let text = if s.is_null() { String::new() } else { unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned() };
        let route = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
        let Some((live, ev)) = route.as_ref() else { return };
        if *live != epoch {
            return;
        }
        match ty {
            EV_FRAMEREADY => {
                ev.pres_fed_ns.store(num, Relaxed);
                ev.frames.fetch_add(1, Relaxed);
            }
            EV_ERROR => {
                crate::log(&format!("feed: pipeline error type=18 num={num} {}", short(&text)));
                ev.error.lock().unwrap().get_or_insert((num, text.clone()));
            }
            EV_END_OF_STREAM => {
                crate::log("feed: pipeline end-of-stream");
                ev.eos.store(true, Relaxed);
            }
            EV_UNLOAD_COMPLETED => crate::log("feed: unload completed"),
            _ => {
                if !text.is_empty() {
                    crate::log(&format!("feed: smp_cb type={ty} num={num} {}", short(&text)));
                }
            }
        }
        if text.contains("\"hdrType\"") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(h) = v["video"]["hdrType"].as_str() {
                    *ev.hdr.lock().unwrap() = Some(h.to_string());
                }
            }
        }
        if text.contains("loadCompleted") || text.contains("\"loaded\"") {
            ev.load_completed.store(true, Relaxed);
        }
        // The ACB bind's two inputs, captured once per pipeline as plx-native does
        // (`play-feed/src/sf_event.rs`): no getMediaID, no re-encoding of the envelope.
        {
            let mut mid = ev.media_id.lock().unwrap();
            if mid.is_none() {
                if let Some(id) = between(&text, "\"context\":\"").or_else(|| between(&text, "\"mediaId\":\"")) {
                    crate::log(&format!("feed: SMP context/mediaId={id}"));
                    *mid = Some(id.to_string());
                }
            }
        }
        if text.contains("\"video\":") && text.contains("\"context\":") {
            let mut si = ev.source_info.lock().unwrap();
            if si.is_none() {
                if let Ok(c) = std::ffi::CString::new(text.as_bytes()) {
                    crate::log(&format!("feed: sourceInfo captured ({} bytes)", text.len()));
                    *si = Some(c);
                }
            }
        }
    }));
}

/// The text after `open` up to the next `"` (none when unterminated or empty).
fn between<'a>(s: &'a str, open: &str) -> Option<&'a str> {
    let rest = &s[s.find(open)? + open.len()..];
    Some(&rest[..rest.find('"')?]).filter(|v| !v.is_empty())
}

fn short(s: &str) -> String {
    s.chars().take(600).collect()
}

#[no_mangle]
pub extern "C" fn yt_sf_log(line: *const c_char) {
    if line.is_null() {
        return;
    }
    let s = unsafe { CStr::from_ptr(line) }.to_string_lossy();
    crate::log(&s);
}

#[cfg(target_arch = "arm")]
mod ffi {
    use std::ffi::{c_char, c_int, c_uint};
    extern "C" {
        pub fn sf_available() -> c_int;
        pub fn sf_blocked() -> c_int;
        pub fn sf_ready() -> c_int;
        pub fn sf_load(payload: *const c_char, epoch: c_uint) -> c_int;
        pub fn sf_is_load_completed() -> c_int;
        pub fn sf_play() -> c_int;
        pub fn sf_pause() -> c_int;
        pub fn sf_set_volume(v: c_int) -> c_int;
        pub fn sf_set_play_rate(rate: f64) -> c_int;
        pub fn sf_seek_ms(ms: i64) -> c_int;
        pub fn sf_push_eos() -> c_int;
        pub fn sf_feed(p: *const u8, size: u32, pts: i64, es: c_int) -> c_char;
        pub fn sf_unload();
        pub fn sf_unload_completed() -> c_int;
        pub fn sf_callback_gate_retire() -> c_int;
        pub fn sf_destroy() -> c_int;
        pub fn sf_quarantine();
    }
}

/// Host builds (no TV libraries): every verb refuses.
#[cfg(not(target_arch = "arm"))]
#[allow(clippy::missing_safety_doc)]
mod ffi {
    use std::ffi::{c_char, c_int, c_uint};
    pub unsafe fn sf_available() -> c_int { 0 }
    pub unsafe fn sf_blocked() -> c_int { 0 }
    pub unsafe fn sf_ready() -> c_int { 0 }
    pub unsafe fn sf_load(_: *const c_char, _: c_uint) -> c_int { 0 }
    pub unsafe fn sf_is_load_completed() -> c_int { 0 }
    pub unsafe fn sf_play() -> c_int { 0 }
    pub unsafe fn sf_pause() -> c_int { 0 }
    pub unsafe fn sf_set_volume(_: c_int) -> c_int { 0 }
    pub unsafe fn sf_set_play_rate(_: f64) -> c_int { 0 }
    pub unsafe fn sf_seek_ms(_: i64) -> c_int { 0 }
    pub unsafe fn sf_push_eos() -> c_int { 0 }
    pub unsafe fn sf_feed(_: *const u8, _: u32, _: i64, _: c_int) -> c_char { b'e' as c_char }
    pub unsafe fn sf_unload() {}
    pub unsafe fn sf_unload_completed() -> c_int { 0 }
    pub unsafe fn sf_callback_gate_retire() -> c_int { 0 }
    pub unsafe fn sf_destroy() -> c_int { 0 }
    pub unsafe fn sf_quarantine() {}
}

/// Keeps `starfish.o` (and with it the exported callback-gate interposer) in the link even
/// while nothing calls the shim yet: zig's driver ignores `--whole-archive`.
#[cfg(target_arch = "arm")]
#[used]
static FORCE_LINK: unsafe extern "C" fn() -> std::ffi::c_int = ffi::sf_available;

/// Safe wrappers. All calls for one pipeline are made from the engine thread.
pub fn available() -> bool {
    unsafe { ffi::sf_available() != 0 }
}
pub fn blocked() -> bool {
    unsafe { ffi::sf_blocked() != 0 }
}
pub fn load(payload: &str, epoch: u32) -> bool {
    let Ok(c) = std::ffi::CString::new(payload) else { return false };
    unsafe { ffi::sf_load(c.as_ptr(), epoch) != 0 }
}
pub fn is_load_completed() -> bool {
    unsafe { ffi::sf_is_load_completed() != 0 }
}
pub fn play() -> bool {
    unsafe { ffi::sf_play() != 0 }
}
pub fn pause() -> bool {
    unsafe { ffi::sf_pause() != 0 }
}
pub fn set_volume(v: i32) -> bool {
    unsafe { ffi::sf_set_volume(v) != 0 }
}
pub fn set_play_rate(rate: f32) -> bool {
    unsafe { ffi::sf_set_play_rate(rate as f64) != 0 }
}
pub fn seek_ms(ms: i64) -> bool {
    unsafe { ffi::sf_seek_ms(ms) != 0 }
}
pub fn push_eos() -> bool {
    unsafe { ffi::sf_push_eos() != 0 }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedReply {
    Ok,
    BufferFull,
    Error,
}

/// esData: 1 = video, 2 = audio. `pts_ns` on the fed timeline.
pub fn feed(data: &[u8], pts_ns: i64, es: i32) -> FeedReply {
    match unsafe { ffi::sf_feed(data.as_ptr(), data.len() as u32, pts_ns, es) } as u8 {
        b'O' => FeedReply::Ok,
        b'B' => FeedReply::BufferFull,
        _ => FeedReply::Error,
    }
}

/// Unload and, only with every proof the shim asks for (callback gate drained + intercepted
/// callback + unload-completed), destruct; otherwise the object is quarantined forever (and
/// engine B disabled for the rest of the process). Waits up to `wait` for the unload event.
/// `release_plane` runs after the Unload (and the gate's retirement) and before D1 or the
/// quarantine: the webOS 4 plane's `setState(UNLOADED)` (plx-native: Unload, 23, retire, UNLOADED, D1).
pub fn teardown(wait: std::time::Duration, release_plane: impl FnOnce()) -> bool {
    unsafe {
        if ffi::sf_ready() == 0 {
            release_plane();
            return true; // nothing was constructed
        }
        ffi::sf_unload();
        let t0 = std::time::Instant::now();
        while ffi::sf_unload_completed() == 0 && t0.elapsed() < wait {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if ffi::sf_unload_completed() == 0 {
            crate::log("feed: no unload-completed event; quarantining the Starfish object");
            release_plane();
            ffi::sf_quarantine();
            return false;
        }
        let retired = ffi::sf_callback_gate_retire() != 0;
        release_plane();
        if !retired {
            ffi::sf_quarantine();
            return false;
        }
        ffi::sf_destroy() != 0
    }
}

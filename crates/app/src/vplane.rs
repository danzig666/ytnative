//! The webOS 4 video plane: ACB (App Common Binding, `libAcbAPI.so.1`, dlopen'd by
//! `csrc/video_plane.c`), driven through plx-native's vendored plane owner (`tv::plane`) and its
//! `StarfishSink`. webOS 5+ binds the plane with an SDL exported window instead (player.rs,
//! feed/window.rs) and never reaches the ACB verbs: [`boot`] checks for that window first.
//!
//! Stage 1: only engine B's handle (player type 10, plx's Feed) is created; engine A (the system
//! player) has no plane on webOS 4 and is not used there.
//!
//! The boot and the bind sequence are plx-native's (`src/app/adapters/backends.rs`,
//! `play-feed/src/feed_pump.rs`): one handle per process, created and initialized once and never
//! destroyed; per pipeline `setSinkType(MAIN)` + `setMediaId` + `setState(LOADED)`, after two
//! frame ticks `setMediaVideoData(sourceInfo)` + full-screen window + `setState(PLAYING)`, and
//! `setState(UNLOADED)` before the pipeline's object is destroyed.

use plx_base::task::MainThread;
use plx_platform::tv::plane::{self, BindPolicy, BindVerdict, HandleSeat, Phase, PlaneFacts, PlaneHandle};
use plx_platform::tv::sink::{DisplaySink, VP_ACB};
use std::ffi::{CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

#[cfg(target_arch = "arm")]
fn sink() -> &'static dyn DisplaySink {
    &plx_platform::webos::video_sink::StarfishSink
}

/// Host builds (no TV libraries): no plane.
#[cfg(not(target_arch = "arm"))]
fn sink() -> &'static dyn DisplaySink {
    &plx_platform::tv::sink::NoSink
}

/// Every pub fn here is called on the SDL main thread: the boot, `Player` start/tick and
/// `FeedPlayer` tick/stop. The ACB verbs are never called from a worker (the seam has no locking).
fn mt() -> MainThread {
    unsafe { MainThread::assume() }
}

static BOOTED: AtomicBool = AtomicBool::new(false);
/// This TV's plane is ACB (no SDL exported window, libAcbAPI resolved).
static ACB: AtomicBool = AtomicBool::new(false);
/// Engine B's handle exists and the library confirmed its `AcbAPI_initialize`.
static FEED: AtomicBool = AtomicBool::new(false);
/// What the present gate was last told (`idle::note(VideoPlane)`, edges only).
static PRESENTING: AtomicBool = AtomicBool::new(false);

/// The SDL that loaded exports the webOS 5+ exported-window API.
pub fn exported_window() -> bool {
    !unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"SDL_webOSCreateExportedWindow".as_ptr()) }.is_null()
}

/// Once per process, main thread, after the first frame (so SDL's own registration and the LS2
/// bus are up before `AcbAPI_initialize`, which takes the app id on the hub). Repeat calls do nothing.
pub fn boot() {
    if BOOTED.swap(true, Relaxed) {
        return;
    }
    let t0 = std::time::Instant::now();
    if exported_window() {
        crate::log("vplane: SDL exported window (webOS 5+): ACB not used");
        plane::installed().boot(PlaneFacts::ABSENT);
        return;
    }
    // Our LS2 client first, under its own name: after AcbAPI_initialize the app id is taken.
    if let Err(e) = crate::ls2::Luna::shared(plx_base::paths::app_id()) {
        crate::log(&format!("vplane: LS2 unavailable before the ACB boot: {e}"));
    }
    let mode = sink().window_mode(); // dlopen("libAcbAPI.so.1"), logged by the seam
    if mode != VP_ACB {
        crate::log(&format!("vplane: no SDL exported window and no ACB (mode {mode}): video cannot be shown"));
        plane::installed().boot(PlaneFacts::ABSENT);
        return;
    }
    ACB.store(true, Relaxed);
    let mt = mt();
    let app = CString::new(plx_base::paths::app_id()).unwrap_or_default();
    let pt = plane::DEFAULT_PLAYER_TYPE;
    let id = unsafe { sink().plane_create(&mt, app.as_ptr(), pt) };
    let initialized = id != 0 && unsafe { sink().plane_initialized(&mt) };
    // A created handle whose initialize the library refused gets no bind (plx grants one on
    // `id != 0` alone; its verbs would then run against an uninitialized core).
    plane::installed().boot_handle(PlaneHandle::Feed, PlaneFacts { handle: id != 0 && initialized, player_type: pt }, HandleSeat { id, initialized });
    FEED.store(initialized, Relaxed);
    crate::log(&format!(
        "vplane: acb handle=feed ptype={pt} app={} create={id} initialized={initialized} (boot {} ms)",
        app.to_string_lossy(),
        t0.elapsed().as_millis()
    ));
}

/// This TV's video plane is ACB (webOS 4).
pub fn acb() -> bool {
    ACB.load(Relaxed)
}

/// Engine B can bind the ACB plane (webOS 4, handle initialized).
pub fn feed() -> bool {
    FEED.load(Relaxed)
}

/// Tells the present gate whether a plane is slaved to the UI surface (bound, or its release in
/// flight): every frame presents then, as in plx-native (`idle::VIDEO_PLANE`).
fn publish(phase: Phase) -> Phase {
    let on = matches!(phase, Phase::Bound | Phase::Releasing);
    if PRESENTING.swap(on, Relaxed) != on {
        plx_machine::idle::note(plx_machine::present::PresentEvent::VideoPlane(on));
        if !on {
            plx_machine::idle::invalidate(); // the unbind frame
        }
        crate::log(&format!("vplane: presenting every frame {}", if on { "on (plane bound)" } else { "off" }));
    }
    phase
}

pub fn phase() -> Phase {
    plane::installed().phase()
}

/// Drains the completion mailbox and advances the release fence's clock.
pub fn pump() -> Phase {
    publish(plane::installed().pump(sink(), &mt()))
}

/// Every frame: keeps the plane owner pumped while a binding or its release is outstanding (a
/// release would otherwise settle only at the next bind).
pub fn tick() {
    if feed() && matches!(phase(), Phase::Bound | Phase::Releasing) {
        pump();
    }
}

/// Engine B's bind (`Rebind`: its own handle title after title, over an unproven release once the
/// fence has passed). On `Granted`: `setSinkType(MAIN)` + `setMediaId` + `setState(LOADED)`.
pub fn bind_feed(media_id: &str) -> BindVerdict {
    let Ok(id) = CString::new(media_id) else { return BindVerdict::Refused(plane::Refusal::NoPlane) };
    let mt = mt();
    let verdict = plane::installed().bind(sink(), &mt, PlaneHandle::Feed, BindPolicy::Rebind, media_id);
    if verdict == BindVerdict::Granted {
        unsafe { sink().plane_bind(&mt, id.as_ptr()) };
    }
    publish(phase());
    verdict
}

/// `setMediaVideoData` with the pipeline's own envelope: 1 accepted, -1 refused client-side.
pub fn video_data(source_info: &CStr) -> i32 {
    unsafe { sink().plane_send_video_data(&mt(), source_info.as_ptr()) }
}

/// Full-screen window + `setState(PLAYING)` (Feed's rectangle, `feed_pump.rs`).
pub fn start_full() {
    unsafe { sink().plane_start(&mt(), 0, 0, 1920, 1080) }
}

pub fn pause() {
    unsafe { sink().plane_pause(&mt()) }
}

pub fn resume() {
    unsafe { sink().plane_resume(&mt()) }
}

/// `setState(UNLOADED)` on the bound handle. Does not wait: the fence settles in later pumps.
pub fn unload() -> Phase {
    publish(plane::installed().unload(sink(), &mt()))
}

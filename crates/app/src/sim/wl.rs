//! Windows simulator stand-in for `wl.rs`: there is no Wayland surface, no video plane under the
//! UI plane and no LS2 bus, so every entry is a no-op. The `tv::Port` mirrors the TV one except
//! that Home (BACK at the root) is logged instead of launching the webOS launcher.

use std::os::raw::c_void;

pub fn clear_opaque_region() {}

pub fn release() -> bool {
    false
}

pub fn grab(_win: *mut c_void) {}

pub fn ls2_pump() {}

fn nothing() {}

fn go_home() {
    crate::log("sim: BACK at the root (on the TV this returns to the webOS home screen)");
}

pub static PORT: plx_platform::tv::Port = plx_platform::tv::Port {
    probe_device: nothing,
    start_capability_probe: nothing,
    repair_sandbox: || Err(plx_platform::tv::sandbox::Failure::Unsupported),
    seal: |_| None,
    open: |_| None,
    remove: |_, _| {},
    system_locale: || plx_platform::tv::LocaleReply::NoPlatform,
    sound_output: || plx_platform::tv::sound::Reply::NoPlatform,
    go_home,
    poll_home: nothing,
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

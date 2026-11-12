//! ytnative local change: the Windows simulator's stand-in for the storage module.
//!
//! The real module is the client of plx-native's on-TV storage helper (fd passing, `openat`,
//! `statvfs`, uid checks) and does not build on Windows. ytnative does not use it; the only thing
//! the rest of the crate needs from it is the wire failure vocabulary and the diagnostics sink that
//! `webos::activate_storage_helper` reports into.

#[path = "state.rs"]
pub mod state;
#[path = "../storage_service/wire.rs"]
pub mod wire;

pub mod diagnostics {
    use super::wire::failure::Detail;

    /// No storage helper on the simulator: nothing to record.
    pub fn activation(_detail: Detail, _elapsed_ms: u64) {}
}

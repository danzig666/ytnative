//! Windows desktop simulator support (`cfg(windows)` only; the TV build never compiles this).
//!
//! The same app runs in a 1920x1080 SDL window on desktop OpenGL: `gl` forwards the GLES2 entry
//! points, `player` replaces the webOS media pipeline with a fake clock, `remote` serves the key
//! channel on localhost TCP, `shot` writes screenshots. `ls2`/`wl` (the stand-ins for the TV's
//! luna-service2 and Wayland modules) are mounted in their place from `main.rs`.

pub mod gl;
pub mod player;
pub mod shot;

/// `%LOCALAPPDATA%\ytnative`: the data dir (token, accounts, settings) and runtime root.
pub fn base_dir() -> std::path::PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("ytnative")
}

/// Local time offset from UTC in seconds (the MSVC CRT has no `tm_gmtoff`).
pub fn local_offset_secs() -> i64 {
    extern "C" {
        fn _localtime64_s(out: *mut libc::tm, t: *const i64) -> i32;
        fn _mkgmtime64(t: *mut libc::tm) -> i64;
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if _localtime64_s(&mut tm, &now) != 0 {
            return 0;
        }
        let as_utc = _mkgmtime64(&mut tm);
        if as_utc == -1 {
            0
        } else {
            as_utc - now
        }
    }
}

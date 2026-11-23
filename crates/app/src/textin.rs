//! The TV's own on-screen keyboard (LG's, through SDL text input): every language the TV has,
//! with its input methods (Chinese, Japanese, Korean, Hindi, Thai...), for the search field.
//! A screen asks for it ([`show`] / [`hide`]); the main loop, which owns SDL, starts and stops
//! SDL text input and turns what the keyboard types into `InputKind::Text` for the screen in
//! front: committed text, Backspace, and `"\n"` for its Enter key.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

const NONE: u8 = 0;
const SHOW: u8 = 1;
const HIDE: u8 = 2;

static WANT: AtomicU8 = AtomicU8::new(NONE);
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Ask for the TV's keyboard (from a screen; started on the next frame).
pub fn show() {
    WANT.store(SHOW, Ordering::SeqCst);
}

/// Put it away (the screen left, a search was made).
pub fn hide() {
    WANT.store(HIDE, Ordering::SeqCst);
}

/// The TV's keyboard is up: text and its Backspace / Enter go to the search field.
pub fn active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

/// Main loop: what was asked since the last frame (`Some(true)` start, `Some(false)` stop).
pub fn take_request() -> Option<bool> {
    match WANT.swap(NONE, Ordering::SeqCst) {
        SHOW => Some(true),
        HIDE => Some(false),
        _ => None,
    }
}

/// Main loop: text input started or stopped.
pub fn set_active(on: bool) {
    ACTIVE.store(on, Ordering::SeqCst);
}

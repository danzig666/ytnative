//! SDL key events → the dispatcher's input events. Adapted from plx-native's
//! `app/events.rs::decode_key` and `app/bridge.rs::key_input` (GPL-3.0-or-later).

use plx_machine::machine::{Edge, InputEvent, InputKind, Key, Source, Tick};

pub fn rd_u32(ev: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes(ev[at..at + 4].try_into().unwrap())
}

/// LG's SDL fork shifts `SDL_KeyboardEvent` by four bytes against upstream headers:
/// +16 state (low byte pressed, bit 0x100 auto-repeat), +20 wcode (LG remote code), +24 sym.
#[cfg(not(windows))]
pub fn decode_key(ev: &[u8]) -> (u32, u32, u32) {
    (rd_u32(ev, 16), rd_u32(ev, 20), rd_u32(ev, 24))
}

/// Windows simulator: stock SDL2 layout (state u8@12, repeat u8@13, keysym.sym @20). The PC
/// keyboard stands in for the remote: arrows, Enter = OK, Escape / Backspace = BACK.
#[cfg(windows)]
pub fn decode_key(ev: &[u8]) -> (u32, u32, u32) {
    let state = ev[12] as u32 | if ev[13] != 0 { 0x100 } else { 0 };
    let sym = rd_u32(ev, 20);
    const SDLK_ESCAPE: u32 = 27;
    const SDLK_BACKSPACE: u32 = 8;
    const SDLK_KP_ENTER: u32 = 0x4000_0058;
    match sym {
        SDLK_ESCAPE | SDLK_BACKSPACE => (state, 482, 0),
        SDLK_KP_ENTER => (state, 0, 13),
        _ => (state, 0, sym),
    }
}

pub fn key_input(sym: u32, wcode: u32, state: u32, now: Tick) -> InputEvent<u32> {
    use plx_ui::consts::{classify, Key as K};
    let key = match classify(sym, wcode) {
        K::Up => Key::Up,
        K::Down => Key::Down,
        K::Left { alt: false } => Key::Left,
        K::Right { alt: false } => Key::Right,
        K::Ok => Key::Ok,
        K::Back => Key::Back,
        _ => Key::Other,
    };
    let edge = if (state & 0xff) != 1 {
        Edge::Up
    } else if state & 0x100 != 0 {
        Edge::Repeat
    } else {
        Edge::Down
    };
    InputEvent { at: now, source: Source::Sdl, kind: InputKind::Key { key, sym, wcode, edge, at_edge: false } }
}

//! The SDL2 / GLES2 entry points the shell itself calls (the vendored crates declare their own).
//! No SDL headers: LG's SDL fork differs from upstream in struct layouts, so everything that reads
//! an SDL struct does it by offset (see `input.rs`).
#![allow(non_snake_case, dead_code)]

use std::os::raw::{c_char, c_int, c_void};

pub const SDL_INIT_VIDEO: u32 = 0x20;
pub const SDL_WINDOW_FULLSCREEN: u32 = 0x1;
pub const SDL_WINDOW_OPENGL: u32 = 0x2;

// SDL_GLattr
pub const SDL_GL_RED_SIZE: c_int = 0;
pub const SDL_GL_GREEN_SIZE: c_int = 1;
pub const SDL_GL_BLUE_SIZE: c_int = 2;
pub const SDL_GL_ALPHA_SIZE: c_int = 3;
pub const SDL_GL_BUFFER_SIZE: c_int = 4;
pub const SDL_GL_DEPTH_SIZE: c_int = 6;
pub const SDL_GL_STENCIL_SIZE: c_int = 7;
pub const SDL_GL_CONTEXT_MAJOR_VERSION: c_int = 17;
pub const SDL_GL_CONTEXT_MINOR_VERSION: c_int = 18;
pub const SDL_GL_CONTEXT_PROFILE_MASK: c_int = 21;
pub const SDL_GL_CONTEXT_PROFILE_ES: c_int = 4;

// SDL_EventType
pub const SDL_QUIT: u32 = 0x100;
pub const SDL_APP_WILLENTERBACKGROUND: u32 = 0x103;
pub const SDL_APP_DIDENTERBACKGROUND: u32 = 0x104;
pub const SDL_APP_WILLENTERFOREGROUND: u32 = 0x105;
pub const SDL_APP_DIDENTERFOREGROUND: u32 = 0x106;
pub const SDL_KEYDOWN: u32 = 0x300;
pub const SDL_KEYUP: u32 = 0x301;

extern "C" {
    pub fn SDL_SetMainReady();
    pub fn SDL_Init(flags: u32) -> c_int;
    pub fn SDL_Quit();
    pub fn SDL_GetError() -> *const c_char;
    pub fn SDL_SetHint(name: *const c_char, value: *const c_char) -> c_int;
    pub fn SDL_GL_SetAttribute(attr: c_int, value: c_int) -> c_int;
    pub fn SDL_CreateWindow(title: *const c_char, x: c_int, y: c_int, w: c_int, h: c_int, flags: u32) -> *mut c_void;
    pub fn SDL_GL_CreateContext(win: *mut c_void) -> *mut c_void;
    pub fn SDL_GL_SetSwapInterval(interval: c_int) -> c_int;
    pub fn SDL_GL_SwapWindow(win: *mut c_void);
    pub fn SDL_PollEvent(ev: *mut c_void) -> c_int;
    pub fn SDL_GetTicks() -> u32;
    pub fn SDL_Delay(ms: u32);
    pub fn SDL_GetWindowWMInfo(win: *mut c_void, info: *mut c_void) -> c_int;
    pub fn SDL_GetVersion(ver: *mut u8);

    pub fn glViewport(x: c_int, y: c_int, w: c_int, h: c_int);
}

pub fn error() -> String {
    unsafe { std::ffi::CStr::from_ptr(SDL_GetError()).to_string_lossy().into_owned() }
}

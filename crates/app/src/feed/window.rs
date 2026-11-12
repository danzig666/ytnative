//! The SDL exported window (webOS 5+ video plane), as engine A creates it (`player.rs`): the
//! window id travels in the Load payload as `option.windowId`. Main thread only.

use std::ffi::{c_char, c_int, CStr, CString};

#[repr(C)]
struct SdlRect {
    x: c_int,
    y: c_int,
    w: c_int,
    h: c_int,
}

type CreateFn = unsafe extern "C" fn(c_int) -> *const c_char;
type SetFn = unsafe extern "C" fn(*const c_char, *mut SdlRect, *mut SdlRect) -> c_int;
type DestroyFn = unsafe extern "C" fn(*const c_char);

fn sym<T>(name: &str) -> Option<T> {
    let c = CString::new(name).ok()?;
    let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c.as_ptr()) };
    (!p.is_null()).then(|| unsafe { std::mem::transmute_copy(&p) })
}

pub struct Window {
    id: CString,
}

impl Window {
    pub fn create() -> Option<Window> {
        let create = sym::<CreateFn>("SDL_webOSCreateExportedWindow")?;
        let p = unsafe { create(0) };
        if p.is_null() {
            return None;
        }
        Some(Window { id: unsafe { CStr::from_ptr(p) }.to_owned() })
    }

    pub fn id(&self) -> String {
        self.id.to_string_lossy().into_owned()
    }

    /// Source frame `w`x`h` shown full screen (1920x1080 UI coordinates).
    pub fn place(&self, w: u32, h: u32) -> bool {
        let Some(set) = sym::<SetFn>("SDL_webOSSetExportedWindow") else { return false };
        let (w, h) = if w > 0 && h > 0 { (w as c_int, h as c_int) } else { (1920, 1080) };
        let mut src = SdlRect { x: 0, y: 0, w, h };
        let mut dst = SdlRect { x: 0, y: 0, w: 1920, h: 1080 };
        unsafe { set(self.id.as_ptr(), &mut src, &mut dst) != 0 }
    }

    /// Only after the pipeline that renders into it was unloaded.
    pub fn destroy(self) {
        if let Some(destroy) = sym::<DestroyFn>("SDL_webOSDestroyExportedWindow") {
            unsafe { destroy(self.id.as_ptr()) };
        }
    }
}

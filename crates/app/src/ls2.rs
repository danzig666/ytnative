//! Minimal luna-service2 client over dlopen: register under the app's identity, run a GLib main
//! loop on a background thread, make one-reply calls (blocking) and subscriptions (callback).
//! This is what a sandboxed app must use instead of the `luna-send` tool.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::mpsc;
use std::time::Duration;

#[repr(C)]
struct LSError {
    error_code: c_int,
    message: *mut c_char,
    file: *const c_char,
    line: c_int,
    func: *const c_char,
    padding: *mut c_void,
    magic: usize,
}

type Filter = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> bool;

struct Api {
    error_init: unsafe extern "C" fn(*mut LSError) -> bool,
    error_free: unsafe extern "C" fn(*mut LSError),
    register: unsafe extern "C" fn(*const c_char, *mut *mut c_void, *mut LSError) -> bool,
    gmain_attach: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut LSError) -> bool,
    call: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, Filter, *mut c_void, *mut libc::c_ulong, *mut LSError) -> bool,
    call_one: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, Filter, *mut c_void, *mut libc::c_ulong, *mut LSError) -> bool,
    payload: unsafe extern "C" fn(*mut c_void) -> *const c_char,
}

pub struct Luna {
    api: Api,
    handle: *mut c_void,
}
unsafe impl Send for Luna {}
unsafe impl Sync for Luna {}

unsafe fn open(lib: &str) -> Result<*mut c_void, String> {
    let c = CString::new(lib).unwrap();
    let h = libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
    if h.is_null() {
        Err(format!("dlopen {lib}: {:?}", CStr::from_ptr(libc::dlerror())))
    } else {
        Ok(h)
    }
}

unsafe fn sym<T>(h: *mut c_void, name: &str) -> Result<T, String> {
    let c = CString::new(name).unwrap();
    let p = libc::dlsym(h, c.as_ptr());
    if p.is_null() {
        Err(format!("missing symbol {name}"))
    } else {
        Ok(std::mem::transmute_copy(&p))
    }
}

/// Boxed callback context: for one-reply calls a Sender, for subscriptions a FnMut.
enum Ctx {
    Once(mpsc::Sender<String>),
    Sub(Box<dyn FnMut(&str) + Send>),
}

unsafe extern "C" fn on_reply(_sh: *mut c_void, msg: *mut c_void, ctx: *mut c_void) -> bool {
    let luna = &*(LUNA_PTR as *const Luna);
    let p = (luna.api.payload)(msg);
    let s = if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() };
    match &mut *(ctx as *mut Ctx) {
        Ctx::Once(tx) => {
            let _ = tx.send(s);
        }
        Ctx::Sub(f) => f(&s),
    }
    true
}

static mut LUNA_PTR: usize = 0;

impl Luna {
    /// The process's one bus connection, made on first use (the player and voice share it).
    pub fn shared(app_id: &str) -> Result<&'static Luna, String> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap();
        let p = unsafe { LUNA_PTR };
        if p != 0 {
            return Ok(unsafe { &*(p as *const Luna) });
        }
        Self::connect(app_id)
    }

    /// Registers on the bus. Tries the bare app id first, then "<appId>-<pid>" (SDL may already
    /// hold the bare name for the app's lifecycle registration).
    pub fn connect(app_id: &str) -> Result<&'static Luna, String> {
        unsafe {
            let ls = open("libluna-service2.so.3").or_else(|_| open("libluna-service2.so"))?;
            let glib = open("libglib-2.0.so.0")?;
            let api = Api {
                error_init: sym(ls, "LSErrorInit")?,
                error_free: sym(ls, "LSErrorFree")?,
                register: sym(ls, "LSRegister")?,
                gmain_attach: sym(ls, "LSGmainAttach")?,
                call: sym(ls, "LSCall")?,
                call_one: sym(ls, "LSCallOneReply")?,
                payload: sym(ls, "LSMessageGetPayload")?,
            };
            let loop_new: unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_void = sym(glib, "g_main_loop_new")?;
            let loop_run: unsafe extern "C" fn(*mut c_void) = sym(glib, "g_main_loop_run")?;

            let mut handle = std::ptr::null_mut();
            let mut last_err = String::new();
            // SDL's lifecycle registration already holds the bare app id; the role generated for native
            // apps also allows "com.webos.media.client.*", which may talk to com.webos.media.
            let pid = std::process::id();
            for name in [format!("com.webos.media.client.ytnative{pid}"), app_id.to_string()] {
                let mut err: LSError = std::mem::zeroed();
                (api.error_init)(&mut err);
                let cname = CString::new(name.clone()).unwrap();
                if (api.register)(cname.as_ptr(), &mut handle, &mut err) {
                    crate::log(&format!("LSRegister ok as {name}"));
                    break;
                }
                last_err = if err.message.is_null() { "?".into() } else { CStr::from_ptr(err.message).to_string_lossy().into_owned() };
                crate::log(&format!("LSRegister({name}) failed: {last_err}"));
                (api.error_free)(&mut err);
                handle = std::ptr::null_mut();
            }
            if handle.is_null() {
                return Err(format!("LSRegister failed: {last_err}"));
            }

            // Default main context, run on its own thread; luna callbacks fire there.
            let main_loop = loop_new(std::ptr::null_mut(), 0) as usize;
            let mut err: LSError = std::mem::zeroed();
            (api.error_init)(&mut err);
            if !(api.gmain_attach)(handle, main_loop as *mut c_void, &mut err) {
                return Err("LSGmainAttach failed".into());
            }
            std::thread::spawn(move || loop_run(main_loop as *mut c_void));

            let luna: &'static Luna = Box::leak(Box::new(Luna { api, handle }));
            LUNA_PTR = luna as *const Luna as usize;
            Ok(luna)
        }
    }

    /// One-reply call, blocking up to 8 s.
    pub fn call(&self, uri: &str, payload: &str) -> String {
        let (tx, rx) = mpsc::channel();
        let ctx = Box::into_raw(Box::new(Ctx::Once(tx))) as *mut c_void; // leaked if no reply: fine for a PoC
        let (u, p) = (CString::new(uri).unwrap(), CString::new(payload).unwrap());
        unsafe {
            let mut err: LSError = std::mem::zeroed();
            (self.api.error_init)(&mut err);
            let mut tok: libc::c_ulong = 0;
            if !(self.api.call_one)(self.handle, u.as_ptr(), p.as_ptr(), on_reply, ctx, &mut tok, &mut err) {
                let m = if err.message.is_null() { "?".into() } else { CStr::from_ptr(err.message).to_string_lossy().into_owned() };
                return format!("<LSCallOneReply failed: {m}>");
            }
        }
        rx.recv_timeout(Duration::from_secs(8)).unwrap_or_else(|_| "<timeout>".into())
    }

    /// Subscription: `f` is called (on the luna thread) for every message.
    pub fn subscribe(&self, uri: &str, payload: &str, f: impl FnMut(&str) + Send + 'static) -> bool {
        let ctx = Box::into_raw(Box::new(Ctx::Sub(Box::new(f)))) as *mut c_void;
        let (u, p) = (CString::new(uri).unwrap(), CString::new(payload).unwrap());
        unsafe {
            let mut err: LSError = std::mem::zeroed();
            (self.api.error_init)(&mut err);
            let mut tok: libc::c_ulong = 0;
            (self.api.call)(self.handle, u.as_ptr(), p.as_ptr(), on_reply, ctx, &mut tok, &mut err)
        }
    }
}

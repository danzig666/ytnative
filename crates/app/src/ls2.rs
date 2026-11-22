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
    /// For a borrowed connection ([`Luna::sdl_app`]): the GLib context its owner dispatches it on.
    /// Calls on it are made there, never concurrently with the owner's own (null = our own loop).
    owner: Option<Owner>,
}

struct Owner {
    context: *mut c_void,
    invoke: unsafe extern "C" fn(*mut c_void, unsafe extern "C" fn(*mut c_void) -> c_int, *mut c_void),
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
    // Every connection loads the same library, so any of them reads the payload.
    let payload: unsafe extern "C" fn(*mut c_void) -> *const c_char = std::mem::transmute(PAYLOAD_FN);
    let p = payload(msg);
    let s = if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() };
    match &mut *(ctx as *mut Ctx) {
        Ctx::Once(tx) => {
            let _ = tx.send(s);
            // A one-reply call gets exactly this one callback: its context is done.
            drop(Box::from_raw(ctx as *mut Ctx));
        }
        Ctx::Sub(f) => f(&s),
    }
    true
}

static mut PAYLOAD_FN: usize = 0;
/// The GLib main loop every connection is attached to (one thread runs it).
static mut MAIN_LOOP: usize = 0;

impl Luna {
    /// The process's media connection, made on first use (the player shares it). Its name may only
    /// talk to the media services.
    pub fn shared(app_id: &str) -> Result<&'static Luna, String> {
        static LOCK: std::sync::Mutex<Option<&'static Luna>> = std::sync::Mutex::new(None);
        let mut g = LOCK.lock().unwrap();
        if let Some(l) = *g {
            return Ok(l);
        }
        // SDL's lifecycle registration already holds the bare app id; the role generated for native
        // apps also allows "com.webos.media.client.*", which may talk to com.webos.media.
        let pid = std::process::id();
        let l = Self::connect(&[format!("com.webos.media.client.ytnative{pid}"), app_id.to_string()])?;
        *g = Some(l);
        Ok(l)
    }

    /// A connection that may call any service (Homebrew Channel, voice). The role LG generates for
    /// a native app lets "com.webos.chromium.media.*" send anywhere, and the TV gives that name the
    /// "all" permission group; the media name of `shared` is refused ("Not permitted to send to").
    pub fn wide(app_id: &str) -> Result<&'static Luna, String> {
        static LOCK: std::sync::Mutex<Option<&'static Luna>> = std::sync::Mutex::new(None);
        let mut g = LOCK.lock().unwrap();
        if let Some(l) = *g {
            return Ok(l);
        }
        let pid = std::process::id();
        let l = match Self::connect(&[format!("com.webos.chromium.media.ytnative{pid}")]) {
            Ok(l) => l,
            Err(_) => return Self::shared(app_id),
        };
        *g = Some(l);
        Ok(l)
    }

    /// SDL's own connection, registered under the bare app id (libhelpers' `HLunaServiceBridge`,
    /// the way LG's SDL keeps it; borrowed as the Cobalt port's microphone code does). Some services
    /// match the caller to the app in front by that id (voiceconductor). Calls on it run on its
    /// owner's dispatch context; the connection stays SDL's.
    pub fn sdl_app() -> Result<&'static Luna, String> {
        static LOCK: std::sync::Mutex<Option<&'static Luna>> = std::sync::Mutex::new(None);
        let mut g = LOCK.lock().unwrap();
        if let Some(l) = *g {
            return Ok(l);
        }
        unsafe {
            let ls = open("libluna-service2.so.3").or_else(|_| open("libluna-service2.so"))?;
            let glib = open("libglib-2.0.so.0")?;
            let helpers = open("libhelpers.so.2")?;
            let instance: unsafe extern "C" fn() -> *mut c_void = sym(helpers, "_ZN18HLunaServiceBridge8instanceEv")?;
            let private: unsafe extern "C" fn(*mut c_void) -> *mut c_void = sym(helpers, "_ZN18HLunaServiceBridge13PrivateHandleEv")?;
            let get_context: unsafe extern "C" fn(*mut c_void, *mut LSError) -> *mut c_void = sym(ls, "LSGmainGetContext")?;
            let bridge = instance();
            if bridge.is_null() {
                return Err("no HLunaServiceBridge".into());
            }
            let handle = private(bridge);
            if handle.is_null() {
                return Err("HLunaServiceBridge has no private handle".into());
            }
            let api = Api {
                error_init: sym(ls, "LSErrorInit")?,
                error_free: sym(ls, "LSErrorFree")?,
                register: sym(ls, "LSRegister")?,
                gmain_attach: sym(ls, "LSGmainAttach")?,
                call: sym(ls, "LSCall")?,
                call_one: sym(ls, "LSCallOneReply")?,
                payload: sym(ls, "LSMessageGetPayload")?,
            };
            let mut err: LSError = std::mem::zeroed();
            (api.error_init)(&mut err);
            let context = get_context(handle, &mut err);
            (api.error_free)(&mut err);
            if context.is_null() {
                return Err("SDL's connection is not attached to a main context".into());
            }
            PAYLOAD_FN = api.payload as usize;
            let owner = Owner { context, invoke: sym(glib, "g_main_context_invoke")? };
            let l: &'static Luna = Box::leak(Box::new(Luna { api, handle, owner: Some(owner) }));
            crate::log("LS2: borrowed SDL's app connection");
            *g = Some(l);
            Ok(l)
        }
    }

    /// Runs `f` where calls on this connection may be made: on the owner's context for a borrowed
    /// one (waiting until it ran), else right here.
    fn on_bus<R: Send>(&self, f: impl FnOnce() -> R + Send) -> R {
        let Some(owner) = &self.owner else { return f() };
        struct Task<'a, R> {
            f: Option<Box<dyn FnOnce() -> R + Send + 'a>>,
            tx: mpsc::Sender<R>,
        }
        unsafe extern "C" fn run<R>(data: *mut c_void) -> c_int {
            let task = &mut *(data as *mut Task<R>);
            if let Some(f) = task.f.take() {
                let _ = task.tx.send(f());
            }
            0 // G_SOURCE_REMOVE
        }
        let (tx, rx) = mpsc::channel();
        let mut task = Task { f: Some(Box::new(f)), tx };
        unsafe { (owner.invoke)(owner.context, run::<R>, &mut task as *mut Task<R> as *mut c_void) };
        // `task` lives on this stack until the owner has run it.
        rx.recv().expect("bus task dropped")
    }

    /// Registers on the bus under the first name the hub accepts.
    fn connect(names: &[String]) -> Result<&'static Luna, String> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap();
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
            for name in names {
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
            let first = MAIN_LOOP == 0;
            if first {
                MAIN_LOOP = loop_new(std::ptr::null_mut(), 0) as usize;
            }
            let main_loop = MAIN_LOOP;
            let mut err: LSError = std::mem::zeroed();
            (api.error_init)(&mut err);
            if !(api.gmain_attach)(handle, main_loop as *mut c_void, &mut err) {
                return Err("LSGmainAttach failed".into());
            }
            PAYLOAD_FN = api.payload as usize;
            if first {
                std::thread::spawn(move || loop_run(main_loop as *mut c_void));
            }
            Ok(Box::leak(Box::new(Luna { api, handle, owner: None })))
        }
    }

    /// One-reply call, blocking up to 8 s.
    pub fn call(&self, uri: &str, payload: &str) -> String {
        let (tx, rx) = mpsc::channel();
        // Freed by `on_reply` (or below when the call is refused); leaked only if no reply ever comes.
        let ctx = Box::into_raw(Box::new(Ctx::Once(tx))) as *mut c_void;
        let (u, p) = (CString::new(uri).unwrap(), CString::new(payload).unwrap());
        unsafe {
            let mut err: LSError = std::mem::zeroed();
            (self.api.error_init)(&mut err);
            let mut tok: libc::c_ulong = 0;
            let (h, ctx_addr, err_addr, tok_addr) = (self.handle as usize, ctx as usize, &mut err as *mut LSError as usize, &mut tok as *mut libc::c_ulong as usize);
            let call_one = self.api.call_one;
            let (up, pp) = (u.as_ptr() as usize, p.as_ptr() as usize);
            let ok = self.on_bus(move || {
                call_one(h as _, up as _, pp as _, on_reply, ctx_addr as _, tok_addr as _, err_addr as _)
            });
            if !ok {
                let m = if err.message.is_null() { "?".into() } else { CStr::from_ptr(err.message).to_string_lossy().into_owned() };
                (self.api.error_free)(&mut err);
                drop(Box::from_raw(ctx as *mut Ctx));
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
            let (h, ctx_addr, err_addr, tok_addr) = (self.handle as usize, ctx as usize, &mut err as *mut LSError as usize, &mut tok as *mut libc::c_ulong as usize);
            let call = self.api.call;
            let (up, pp) = (u.as_ptr() as usize, p.as_ptr() as usize);
            let ok = self.on_bus(move || call(h as _, up as _, pp as _, on_reply, ctx_addr as _, tok_addr as _, err_addr as _));
            if !ok {
                let m = if err.message.is_null() { "?".into() } else { CStr::from_ptr(err.message).to_string_lossy().into_owned() };
                crate::log(&format!("LSCall {uri}: {m}"));
                (self.api.error_free)(&mut err);
            }
            ok
        }
    }
}

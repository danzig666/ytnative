//! tvplay - step 2 proof of concept for a native YouTube client on LG webOS 5+.
//!
//! 1. Opens the TV's own libSDL2 at runtime (no link-time dependency on any LG library).
//! 2. Creates a transparent fullscreen SDL window + an SDL "exported window" for the video plane.
//! 3. Loads the URL into the TV's media service (uMediaServer, `luna://com.webos.media`) with that
//!    windowId, so the hardware decoder's output is composited where we say.
//! 4. Logs pipeline events, exits on BACK/ESC, timeout or end of stream, and always unloads.
//!
//! Usage: tvplay <url | @file-with-url-on-line-N[:N]> [seconds]
//! Writes everything it does to /tmp/ytnative/ytnative.log (an app launched by the TV has no stdout).

mod ls2;

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const LOG: &str = "/tmp/ytnative/ytnative.log";

static LOGF: Mutex<Option<std::fs::File>> = Mutex::new(None);

macro_rules! log {
    ($($a:tt)*) => {{
        let line = format!($($a)*);
        eprintln!("{line}");
        if let Some(f) = LOGF.lock().unwrap().as_mut() { let _ = writeln!(f, "{line}"); let _ = f.flush(); }
    }};
}

#[repr(C)]
struct SdlRect {
    x: c_int,
    y: c_int,
    w: c_int,
    h: c_int,
}

struct Sdl {
    init: unsafe extern "C" fn(u32) -> c_int,
    quit: unsafe extern "C" fn(),
    get_error: unsafe extern "C" fn() -> *const c_char,
    create_window: unsafe extern "C" fn(*const c_char, c_int, c_int, c_int, c_int, u32) -> *mut c_void,
    destroy_window: unsafe extern "C" fn(*mut c_void),
    create_renderer: unsafe extern "C" fn(*mut c_void, c_int, u32) -> *mut c_void,
    set_draw_color: unsafe extern "C" fn(*mut c_void, u8, u8, u8, u8) -> c_int,
    set_blend: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    clear: unsafe extern "C" fn(*mut c_void) -> c_int,
    present: unsafe extern "C" fn(*mut c_void),
    poll_event: unsafe extern "C" fn(*mut u8) -> c_int,
    create_exported: Option<unsafe extern "C" fn(c_int) -> *const c_char>,
    set_exported: Option<unsafe extern "C" fn(*const c_char, *mut SdlRect, *mut SdlRect) -> c_int>,
    destroy_exported: Option<unsafe extern "C" fn(*const c_char)>,
}

unsafe fn sym<T>(h: *mut c_void, name: &str) -> Option<T> {
    let c = CString::new(name).unwrap();
    let p = libc::dlsym(h, c.as_ptr());
    if p.is_null() {
        None
    } else {
        Some(std::mem::transmute_copy(&p))
    }
}

impl Sdl {
    unsafe fn load() -> Result<Sdl, String> {
        let name = CString::new("libSDL2-2.0.so.0").unwrap();
        let h = libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
        if h.is_null() {
            return Err(format!("dlopen libSDL2 failed: {:?}", CStr::from_ptr(libc::dlerror())));
        }
        macro_rules! req {
            ($n:expr) => {
                sym(h, $n).ok_or_else(|| format!("missing SDL symbol {}", $n))?
            };
        }
        Ok(Sdl {
            init: req!("SDL_Init"),
            quit: req!("SDL_Quit"),
            get_error: req!("SDL_GetError"),
            create_window: req!("SDL_CreateWindow"),
            destroy_window: req!("SDL_DestroyWindow"),
            create_renderer: req!("SDL_CreateRenderer"),
            set_draw_color: req!("SDL_SetRenderDrawColor"),
            set_blend: req!("SDL_SetRenderDrawBlendMode"),
            clear: req!("SDL_RenderClear"),
            present: req!("SDL_RenderPresent"),
            poll_event: req!("SDL_PollEvent"),
            create_exported: sym(h, "SDL_webOSCreateExportedWindow"),
            set_exported: sym(h, "SDL_webOSSetExportedWindow"),
            destroy_exported: sym(h, "SDL_webOSDestroyExportedWindow"),
        })
    }
    fn err(&self) -> String {
        unsafe { CStr::from_ptr((self.get_error)()).to_string_lossy().into_owned() }
    }
}

static BUS: std::sync::OnceLock<Option<&'static ls2::Luna>> = std::sync::OnceLock::new();

pub fn log_line(s: &str) {
    log!("{s}");
}

/// One-shot luna call: over our own LS2 registration when we have one (required inside the app
/// sandbox), else through the stock `luna-send` tool (works from a root shell).
fn luna(uri: &str, payload: &str) -> String {
    if let Some(bus) = BUS.get().copied().flatten() {
        return bus.call(uri, payload);
    }
    match Command::new("luna-send").args(["-w", "8000", "-n", "1", uri, payload]).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(e) => format!("<luna-send failed: {e}>"),
    }
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let k = format!("\"{key}\"");
    let rest = &json[json.find(&k)? + k.len()..];
    let rest = &rest[rest.find('"')? + 1..];
    Some(&rest[..rest.find('"')?])
}

fn resolve_url(arg: &str) -> String {
    // "@path[:line]" reads the URL from a file (URLs are too long and too quote-heavy for argv
    // round-trips through appinfo/luna params).
    if let Some(spec) = arg.strip_prefix('@') {
        let (path, line) = match spec.rsplit_once(':') {
            Some((p, l)) if l.chars().all(|c| c.is_ascii_digit()) => (p, l.parse().unwrap_or(1)),
            _ => (spec, 1usize),
        };
        let text = std::fs::read_to_string(path).unwrap_or_default();
        return text.lines().nth(line - 1).unwrap_or("").trim().to_string();
    }
    arg.to_string()
}

fn main() {
    let _ = std::fs::create_dir_all("/tmp/ytnative");
    *LOGF.lock().unwrap() = std::fs::OpenOptions::new().create(true).append(true).open(LOG).ok();

    // Arguments come from argv when run from a shell, or from the launch params JSON that the
    // webOS app manager passes as argv[1] for native apps ({"url":"...","secs":N}).
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (url_arg, secs) = match args.first() {
        Some(a) if a.trim_start().starts_with('{') => (
            field(a, "url").unwrap_or("@/tmp/ytnative/last_streams.txt:3").to_string(),
            a.split("\"secs\":").nth(1).and_then(|s| s.trim().split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()).unwrap_or(60u64),
        ),
        Some(a) => (a.clone(), args.get(1).and_then(|s| s.parse().ok()).unwrap_or(60)),
        None => ("@/tmp/ytnative/last_streams.txt:3".to_string(), 60),
    };
    let url = resolve_url(&url_arg);
    let app_id = std::env::var("APPID").unwrap_or_else(|_| "io.github.danzig666.ytnative".into());
    log!("=== tvplay start: appId={app_id} secs={secs} url={}...", &url[..url.len().min(90)]);
    if url.is_empty() {
        log!("no URL");
        return;
    }

    let sdl = match unsafe { Sdl::load() } {
        Ok(s) => s,
        Err(e) => {
            log!("{e}");
            return;
        }
    };

    unsafe {
        if (sdl.init)(0x20 /* SDL_INIT_VIDEO */) != 0 {
            log!("SDL_Init failed: {}", sdl.err());
            return;
        }
        let bus = match ls2::Luna::connect(&app_id) {
            Ok(b) => Some(b),
            Err(e) => {
                log!("LS2 unavailable ({e}); falling back to luna-send");
                None
            }
        };
        let _ = BUS.set(bus);
        let title = CString::new("tvplay").unwrap();
        let win = (sdl.create_window)(title.as_ptr(), 0, 0, 1920, 1080, 0x4 /* SHOWN */ | 0x1 /* FULLSCREEN */);
        if win.is_null() {
            log!("SDL_CreateWindow failed: {}", sdl.err());
            (sdl.quit)();
            return;
        }
        let ren = (sdl.create_renderer)(win, -1, 0x2 /* ACCELERATED */);
        if ren.is_null() {
            log!("SDL_CreateRenderer failed: {}", sdl.err());
        } else {
            (sdl.set_blend)(ren, 0 /* NONE: write alpha=0 straight through */);
        }
        let punch = |sdl: &Sdl| {
            if !ren.is_null() {
                (sdl.set_draw_color)(ren, 0, 0, 0, 0);
                (sdl.clear)(ren);
                (sdl.present)(ren);
            }
        };
        punch(&sdl);

        let Some(create) = sdl.create_exported else {
            log!("this SDL has no SDL_webOSCreateExportedWindow (pre-webOS 5?)");
            return;
        };
        let wid_ptr = create(0 /* video */);
        if wid_ptr.is_null() {
            log!("SDL_webOSCreateExportedWindow returned NULL: {}", sdl.err());
            return;
        }
        let window_id = CStr::from_ptr(wid_ptr).to_string_lossy().into_owned();
        let wid_c = CString::new(window_id.clone()).unwrap();
        log!("exported windowId={window_id}");
        if let Some(set) = sdl.set_exported {
            let mut src = SdlRect { x: 0, y: 0, w: 1920, h: 1080 };
            let mut dst = SdlRect { x: 0, y: 0, w: 1920, h: 1080 };
            log!("setExportedWindow -> {}", set(wid_c.as_ptr(), &mut src, &mut dst));
        }

        let load = format!(
            r#"{{"uri":{},"type":"media","payload":{{"option":{{"appId":{},"windowId":{}}}}}}}"#,
            json_str(&url),
            json_str(&app_id),
            json_str(&window_id)
        );
        let r = luna("luna://com.webos.media/load", &load);
        log!("load -> {r}");
        let Some(mid) = field(&r, "mediaId").map(str::to_string) else {
            log!("no mediaId, giving up");
            return;
        };

        // Event subscription: over LS2 the callback fires per message (no buffering issues).
        let ended = Arc::new(AtomicBool::new(false));
        if let Some(bus) = BUS.get().copied().flatten() {
            let ended = ended.clone();
            let mut last_time_log = Instant::now() - Duration::from_secs(10);
            let ok = bus.subscribe(
                "luna://com.webos.media/subscribe",
                &format!(r#"{{"mediaId":{},"subscribe":true}}"#, json_str(&mid)),
                move |l: &str| {
                    let l = l.trim();
                    if l.is_empty() || l.starts_with("{\"bufferRange") {
                        return;
                    }
                    if l.starts_with("{\"currentTime") {
                        if last_time_log.elapsed() < Duration::from_secs(5) {
                            return;
                        }
                        last_time_log = Instant::now();
                    }
                    log!("event {}", &l[..l.len().min(400)]);
                    if l.contains("\"endOfStream\"") || l.contains("\"error\"") {
                        ended.store(true, Ordering::SeqCst);
                    }
                },
            );
            log!("subscribe -> {ok}");
        } else {
            log!("(no LS2 handle: running without event log)");
        }

        let mid_j = json_str(&mid);
        log!("notifyForeground -> {}", luna("luna://com.webos.media/notifyForeground", &format!(r#"{{"connectionId":{mid_j}}}"#)));
        log!("play -> {}", luna("luna://com.webos.media/play", &format!(r#"{{"mediaId":{mid_j}}}"#)));

        let start = Instant::now();
        let mut ev = [0u8; 64];
        'outer: while start.elapsed() < Duration::from_secs(secs) && !ended.load(Ordering::SeqCst) {
            while (sdl.poll_event)(ev.as_mut_ptr()) != 0 {
                let ty = u32::from_ne_bytes(ev[0..4].try_into().unwrap());
                if ty == 0x100 {
                    log!("SDL_QUIT");
                    break 'outer;
                }
                if ty == 0x300 {
                    let scancode = i32::from_ne_bytes(ev[16..20].try_into().unwrap());
                    let sym = i32::from_ne_bytes(ev[20..24].try_into().unwrap());
                    log!("key scancode={scancode} sym={sym:#x}");
                    if sym == 27 || scancode == 482 || sym == 0x4000_0000 | 482 {
                        break 'outer;
                    }
                }
            }
            punch(&sdl);
            std::thread::sleep(Duration::from_millis(33));
        }

        log!("unload -> {}", luna("luna://com.webos.media/unload", &format!(r#"{{"mediaId":{mid_j}}}"#)));
        std::thread::sleep(Duration::from_millis(300)); // let the last events arrive
        if let Some(d) = sdl.destroy_exported {
            d(wid_c.as_ptr());
        }
        (sdl.destroy_window)(win);
        (sdl.quit)();
        log!("=== tvplay done after {:.1}s", start.elapsed().as_secs_f32());
    }
}

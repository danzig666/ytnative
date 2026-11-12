//! Thumbnails: our `plx_ui::tex::Source`. A draw probes a URL; a miss interns it as a PosterKey
//! and queues a fetch; worker threads download + decode; the main thread hands decoded pixels to
//! the render cache (`tex::accept`) and uploads them under the frame budget (`tex::prepare`).

use plx_machine::machine::{PosterKey, PresentHandle};
use plx_machine::present::Present;
use plx_ui::frame::Budget;
use plx_ui::tex::{self, Decoded, PosterError, PosterReady, Tex, Uploader, Warm};
use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex};

const WORKERS: usize = 3;
/// Thumbnails are scaled down to at most this width before upload (YouTube serves 640x360+).
const MAX_W: u32 = 480;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Queued,
    Ready,
    Failed,
}

#[derive(Default)]
struct State {
    by_url: HashMap<String, (PosterKey, Status)>,
    queue: VecDeque<(PosterKey, String)>,
    done: Vec<PosterReady<PosterKey>>,
    next: u32,
    started: bool,
}

pub struct Thumbs {
    st: Mutex<State>,
    cv: Condvar,
}

pub static THUMBS: std::sync::LazyLock<Thumbs> = std::sync::LazyLock::new(|| Thumbs { st: Mutex::new(State::default()), cv: Condvar::new() });

impl Thumbs {
    fn request(&self, url: &str) -> (PosterKey, Status, bool) {
        let mut st = self.st.lock().unwrap();
        if let Some(&(k, s)) = st.by_url.get(url) {
            return (k, s, false);
        }
        st.next += 1;
        let k = PosterKey(st.next);
        st.by_url.insert(url.to_string(), (k, Status::Queued));
        st.queue.push_back((k, url.to_string()));
        if !st.started {
            st.started = true;
            for i in 0..WORKERS {
                let _ = plx_base::task::spawn(if i == 0 { "thumb0" } else { "thumb" }, || (*THUMBS).work());
            }
        }
        drop(st);
        self.cv.notify_one();
        (k, Status::Queued, true)
    }

    fn work(&'static self) {
        let http = match yt_core::net::builder().timeout(std::time::Duration::from_secs(20)).build() {
            Ok(c) => c,
            Err(_) => return,
        };
        loop {
            let (key, url) = {
                let mut st = self.st.lock().unwrap();
                loop {
                    if let Some(j) = st.queue.pop_front() {
                        break j;
                    }
                    st = self.cv.wait(st).unwrap();
                }
            };
            let result = fetch(&http, &url);
            let mut st = self.st.lock().unwrap();
            let ok = result.is_ok();
            if let Some(e) = st.by_url.get_mut(&url) {
                e.1 = if ok { Status::Ready } else { Status::Failed };
            }
            st.done.push(PosterReady { key, result });
            drop(st);
            plx_machine::present::wake_from_worker();
        }
    }

    /// Main thread, every frame: hand finished decodes to the render cache.
    pub fn deliver(&self) {
        let done = std::mem::take(&mut self.st.lock().unwrap().done);
        for r in done {
            tex::accept(r);
        }
    }
}

fn fetch(http: &reqwest::blocking::Client, url: &str) -> Result<Decoded, PosterError> {
    match fetch_one(http, url) {
        Ok(d) => Ok(d),
        // DeArrow renders crowd thumbnails on demand and often has none yet (204 / 404 / an
        // empty body): fall back to the video's own thumbnail instead of a blank card.
        Err(e) if url.contains("dearrow-thumb") => match url.split("videoID=").nth(1).map(|r| r.split('&').next().unwrap_or(r)) {
            Some(id) if !id.is_empty() => fetch_one(http, &format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")),
            _ => Err(e),
        },
        Err(e) => Err(e),
    }
}

fn fetch_one(http: &reqwest::blocking::Client, url: &str) -> Result<Decoded, PosterError> {
    let bytes = http.get(url).send().and_then(|r| r.error_for_status()).and_then(|r| r.bytes()).map_err(|_| PosterError::Fetch)?;
    let (w, h, rgba) = plx_gfx::img::img_decode_owned(&bytes).ok_or(PosterError::Decode)?;
    // Storyboard sheets are frame grids: keep full resolution so each frame stays sharp.
    let (w, h, rgba) = if url.contains("/sb/") { (w, h, rgba) } else { downscale(w, h, rgba) };
    Ok(Decoded { w: w as u16, h: h as u16, rgba: rgba.into_boxed_slice() })
}

/// Box-filter downscale by an integer factor so the texture is at most MAX_W wide.
fn downscale(w: u32, h: u32, rgba: Vec<u8>) -> (u32, u32, Vec<u8>) {
    let f = w.div_ceil(MAX_W).max(1);
    if f == 1 {
        return (w, h, rgba);
    }
    let (nw, nh) = (w / f, h / f);
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    for y in 0..nh {
        for x in 0..nw {
            let mut acc = [0u32; 4];
            for dy in 0..f {
                for dx in 0..f {
                    let i = (((y * f + dy) * w + (x * f + dx)) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += rgba[i + c] as u32;
                    }
                }
            }
            let o = ((y * nw + x) * 4) as usize;
            for c in 0..4 {
                out[o + c] = (acc[c] / (f * f)) as u8;
            }
        }
    }
    (nw, nh, out)
}

impl tex::Source for Thumbs {
    fn probe(&self, _srv: u16, path: &str, _w: i32, _h: i32, _png: bool) -> Option<PosterKey> {
        if path.is_empty() {
            return None;
        }
        let (k, s, _) = self.request(path);
        (s == Status::Ready).then_some(k)
    }
    fn warm(&self, _srv: u16, path: &str, _w: i32, _h: i32, _png: bool) -> Warm {
        if path.is_empty() {
            return Warm::Known;
        }
        if self.request(path).2 { Warm::Claimed } else { Warm::Known }
    }
    fn logo(&self, _srv: u16, _logo: &str) -> Option<PosterKey> {
        None
    }
    fn logo_warm(&self, _srv: u16, _logo: &str) -> Warm {
        Warm::Known
    }
    fn failed(&self, _srv: u16, path: &str, _w: i32, _h: i32, _png: bool) -> bool {
        self.st.lock().unwrap().by_url.get(path).is_some_and(|e| e.1 == Status::Failed)
    }
    fn unresident(&self, key: PosterKey, _refused: bool) {
        // Forget it so a later draw fetches again.
        self.st.lock().unwrap().by_url.retain(|_, v| v.0 != key);
    }
    fn idle(&self) -> bool {
        let st = self.st.lock().unwrap();
        st.queue.is_empty() && st.done.is_empty()
    }
}

/// The GL half of the cache (plx-native's `GfxUploader`).
pub struct GfxUploader;

impl Uploader for GfxUploader {
    fn upload(&mut self, d: &Decoded) -> Tex {
        let id = plx_gfx::img::img_upload_rgba(d.rgba.as_ptr(), d.w as i32, d.h as i32);
        Tex { id, w: d.w, h: d.h }
    }
    fn warm(&mut self, t: Tex) {
        if t.id != 0 {
            plx_gfx::gfx::warm_tex(t.id);
        }
    }
    fn free(&mut self, t: Tex) {
        if t.id != 0 {
            plx_gfx::gfx::delete_tex(t.id);
        }
    }
}

/// Called from the Rig's per-frame `prepare`.
pub fn prepare(b: &mut Budget, present: &mut Present) {
    tex::prepare(b, &mut GfxUploader, &mut PresentHandle(present), crate::now_us);
}

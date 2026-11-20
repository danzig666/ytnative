//! Ambient mode: a soft glow in the bars around a video that does not fill the screen (a 4:3
//! video, a vertical Short, a 21:9 film), in the colours of the picture's edges, like YouTube's
//! ambient mode.
//!
//! The picture is on the hardware video plane under the UI, so its pixels cannot be read. The
//! storyboard (the seek preview's frame sheets) stands in for it: every sheet is reduced to a small
//! copy when the thumbnail worker decodes it ([`note_sheet`]), and the frame at the playhead gives
//! three colours along each edge of the picture that faces a bar. They are painted as two tiny
//! textures stretched over the bars, so the GPU's filtering draws the gradient: brightest against
//! the picture, gone at the edge of the screen, with no hard edge anywhere. A new frame's colours
//! cross-fade in over a few seconds, the glow breathes slowly, it fades out a minute into a pause
//! (an OLED panel must not hold a still image), and it is dimmed with the rest of the UI over HDR
//! video. Live streams have no storyboard, so they have no glow.

use crate::player::{Phase, Zoom};
use plx_ui::{Painter, Rect};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

pub type Rgb = [f32; 3];
/// Three colours along each of the two edges that face bars, `[side][along the edge]`: left and
/// right (top to bottom) for a pillarboxed picture, top and bottom (left to right) for a
/// letterboxed one.
pub type Palette = [[Rgb; 3]; 2];

/// A bar narrower than this (a 1.85:1 film, say) gets no glow: there is no room for a gradient.
const MIN_BAR: f32 = 24.0;
/// A new frame's colours take this long to fade in.
const FADE_MS: u32 = 3000;
/// Paused this long, the glow fades away (and comes back when playback resumes).
const PAUSE_OFF_MS: u32 = 60_000;
/// Opacity of the glow against the picture, before the slow breathing and the HDR dimming.
/// (0.38 was barely visible on an OLED panel.)
const PEAK: f32 = 0.75;
/// No colour brighter than this: a white frame glows a soft grey, not a white bar.
const MAX_INK: f32 = 0.85;
/// Edge colours are lifted by this power (hue kept): a frame's edge averages come out dark,
/// and a dark glow does not show in the black bars.
const LIFT: f32 = 0.75;
/// Texels across a bar, from the picture's edge to the screen's: the falloff curve, which the
/// GPU's linear filtering smooths between them.
const FALLOFF_TEXELS: usize = 8;
/// The glow starts this far (px) inside the picture's edge, so the TV's scaler rounding never
/// leaves a dark line between the two.
const OVERLAP: f32 = 3.0;
/// A frame edge darker than this (brightest channel, 0..1) is a black border of the video
/// itself; the glow covers it.
const BLACK: f32 = 0.07;
/// At most this share of the frame is taken for a black border, either side.
const MAX_BORDER: f32 = 0.2;
/// Reduced sheets kept (each about 40 KB): enough for the sheets of a long video seeked around.
const SHEETS_KEPT: usize = 48;
/// The reduced copy of a sheet is at most this wide.
const REDUCED_W: u32 = 160;

/// A storyboard sheet, box-filtered down to a few pixels per frame (RGB).
pub struct Sheet {
    w: u32,
    h: u32,
    /// Sheet pixels per reduced pixel.
    f: u32,
    rgb: Vec<u8>,
}

static SHEETS: Mutex<VecDeque<(String, Arc<Sheet>)>> = Mutex::new(VecDeque::new());

/// Thumbnail worker: a storyboard sheet was decoded; keep its reduced copy for the glow.
pub fn note_sheet(url: &str, w: u32, h: u32, rgba: &[u8]) {
    let Some(s) = reduce(w, h, rgba) else { return };
    let mut sheets = SHEETS.lock().unwrap();
    sheets.retain(|(u, _)| u != url);
    if sheets.len() >= SHEETS_KEPT {
        sheets.pop_front();
    }
    sheets.push_back((url.to_string(), Arc::new(s)));
}

fn sheet(url: &str) -> Option<Arc<Sheet>> {
    SHEETS.lock().unwrap().iter().find(|(u, _)| u == url).map(|(_, s)| s.clone())
}

/// Box-filters an RGBA sheet down to at most [`REDUCED_W`] wide.
fn reduce(w: u32, h: u32, rgba: &[u8]) -> Option<Sheet> {
    if w == 0 || h == 0 || rgba.len() < (w * h * 4) as usize {
        return None;
    }
    let f = w.div_ceil(REDUCED_W).max(1);
    let (nw, nh) = (w / f, h / f);
    if nw == 0 || nh == 0 {
        return None;
    }
    let mut rgb = vec![0u8; (nw * nh * 3) as usize];
    for y in 0..nh {
        for x in 0..nw {
            let mut acc = [0u32; 3];
            for dy in 0..f {
                let row = ((y * f + dy) * w + x * f) as usize * 4;
                for dx in 0..f as usize {
                    for c in 0..3 {
                        acc[c] += rgba[row + dx * 4 + c] as u32;
                    }
                }
            }
            let o = ((y * nw + x) * 3) as usize;
            for c in 0..3 {
                rgb[o + c] = (acc[c] / (f * f)) as u8;
            }
        }
    }
    Some(Sheet { w: nw, h: nh, f, rgb })
}

/// Average colour of the reduced pixels `x0..x1` x `y0..y1` (0..1).
fn average(s: &Sheet, x0: u32, x1: u32, y0: u32, y1: u32) -> Rgb {
    let (x1, y1) = (x1.min(s.w).max(x0 + 1), y1.min(s.h).max(y0 + 1));
    let mut acc = [0u32; 3];
    let mut n = 0u32;
    for y in y0..y1.min(s.h) {
        for x in x0..x1.min(s.w) {
            let i = ((y * s.w + x) * 3) as usize;
            for c in 0..3 {
                acc[c] += s.rgb[i + c] as u32;
            }
            n += 1;
        }
    }
    if n == 0 {
        return [0.0; 3];
    }
    acc.map(|v| v as f32 / n as f32 / 255.0)
}

/// Dark colours lifted ([`LIFT`]) and bright ones scaled down so the brightest channel is at
/// most [`MAX_INK`], the hue kept either way.
fn tame(c: Rgb) -> Rgb {
    let m = c[0].max(c[1]).max(c[2]);
    let c = if m > 0.0 && m < 1.0 { c.map(|v| v * m.powf(LIFT) / m) } else { c };
    let m = c[0].max(c[1]).max(c[2]);
    if m > MAX_INK {
        c.map(|v| v * MAX_INK / m)
    } else {
        c
    }
}

/// How much of the frame at `(x, y, w, h)` (sheet pixels) is a black border of the video itself
/// on the two sides that face the bars (left and right when `pillar`, else top and bottom), as
/// shares of the frame's width (or height), each at most [`MAX_BORDER`]: many videos carry black
/// strips in their frames, which would show as a dark line between the picture and the glow.
pub fn black_edges(s: &Sheet, frame: (u32, u32, u32, u32), pillar: bool) -> [f32; 2] {
    let (fx, fy, fw, fh) = frame;
    let (x0, y0) = (fx / s.f, fy / s.f);
    let (x1, y1) = (((fx + fw) / s.f).min(s.w), ((fy + fh) / s.f).min(s.h));
    if x1 < x0 + 4 || y1 < y0 + 4 {
        return [0.0; 2];
    }
    let dark = |c: Rgb| c[0].max(c[1]).max(c[2]) < BLACK;
    let (len, cap) = if pillar { (x1 - x0, ((x1 - x0) as f32 * MAX_BORDER) as u32) } else { (y1 - y0, ((y1 - y0) as f32 * MAX_BORDER) as u32) };
    // Lines (columns or rows) counted from one edge while they are dark.
    let count = |from_far: bool| {
        (0..cap)
            .take_while(|&i| {
                let at = if from_far { len - 1 - i } else { i };
                let line = if pillar { average(s, x0 + at, x0 + at + 1, y0, y1) } else { average(s, x0, x1, y0 + at, y0 + at + 1) };
                dark(line)
            })
            .count() as f32
            / len as f32
    };
    [count(false), count(true)]
}

/// The colours along the two edges of the frame at `(x, y, w, h)` (sheet pixels) that face the
/// bars: the left and right edges when `pillar`, else the top and bottom. Each colour is the
/// average of a band a quarter of the frame deep, a little in from the edge and past any black
/// border ([`black_edges`]), one third of the edge long.
pub fn edge_palette(s: &Sheet, frame: (u32, u32, u32, u32), pillar: bool) -> Option<Palette> {
    let (fx, fy, fw, fh) = frame;
    let (x0, y0) = (fx / s.f, fy / s.f);
    let (x1, y1) = (((fx + fw) / s.f).min(s.w), ((fy + fh) / s.f).min(s.h));
    if x1 < x0 + 4 || y1 < y0 + 4 {
        return None;
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let border = black_edges(s, frame, pillar);
    let past = |share: f32, len: u32| (share * len as f32).ceil() as u32;
    let third = |a: u32, len: u32, i: u32| (a + len * i / 3, a + len * (i + 1) / 3);
    let mut p: Palette = [[[0.0; 3]; 3]; 2];
    if pillar {
        let inset = (w * 3 / 100).max(past(border[0].max(border[1]), w));
        let depth = (w / 4).max(1);
        for i in 0..3 {
            let (a, b) = third(y0, h, i);
            p[0][i as usize] = tame(average(s, x0 + inset, x0 + inset + depth, a, b));
            p[1][i as usize] = tame(average(s, x1 - inset - depth, x1 - inset, a, b));
        }
    } else {
        let inset = (h * 3 / 100).max(past(border[0].max(border[1]), h));
        let depth = (h / 4).max(1);
        for i in 0..3 {
            let (a, b) = third(x0, w, i);
            p[0][i as usize] = tame(average(s, a, b, y0 + inset, y0 + inset + depth));
            p[1][i as usize] = tame(average(s, a, b, y1 - inset - depth, y1 - inset));
        }
    }
    Some(p)
}

/// Where the picture lands on the 1920 x 1080 screen: a `vw` x `vh` video fitted whole, scaled
/// by `zoom` about the centre (it may then run off the screen). `None` without a size.
pub fn content_rect(vw: u32, vh: u32, zoom: f32) -> Option<Rect> {
    if vw == 0 || vh == 0 {
        return None;
    }
    let k = (1920.0 / vw as f32).min(1080.0 / vh as f32) * zoom;
    let (w, h) = (vw as f32 * k, vh as f32 * k);
    Some(Rect::new((1920.0 - w) / 2.0, (1080.0 - h) / 2.0, w, h))
}

/// The bars widened toward the picture over its black borders (`border`, shares of `content`)
/// and [`OVERLAP`], so no dark line stays between the picture and the glow.
pub fn reach_in(rects: [Rect; 2], content: Rect, pillar: bool, border: [f32; 2]) -> [Rect; 2] {
    let [mut a, mut b] = rects;
    if pillar {
        let (l, r) = (border[0] * content.w + OVERLAP, border[1] * content.w + OVERLAP);
        a.w += l;
        b.x -= r;
        b.w += r;
    } else {
        let (t, d) = (border[0] * content.h + OVERLAP, border[1] * content.h + OVERLAP);
        a.h += t;
        b.y -= d;
        b.h += d;
    }
    [a, b]
}

/// The two bars around `content` (the parts of the screen it leaves black), first the left or
/// top one: `Some((true, ..))` for side bars (pillarbox), `Some((false, ..))` for bars above and
/// below (letterbox), `None` when the picture fills the screen or the bars are too thin.
pub fn bars(content: Rect) -> Option<(bool, [Rect; 2])> {
    let side = content.x.max(0.0);
    let top = content.y.max(0.0);
    if side >= MIN_BAR && side >= top {
        let right = (1920.0 - (content.x + content.w)).max(0.0);
        return Some((true, [Rect::new(0.0, 0.0, side, 1080.0), Rect::new(1920.0 - right, 0.0, right, 1080.0)]));
    }
    if top >= MIN_BAR {
        let bottom = (1080.0 - (content.y + content.h)).max(0.0);
        return Some((false, [Rect::new(0.0, 0.0, 1920.0, top), Rect::new(0.0, 1080.0 - bottom, 1920.0, bottom)]));
    }
    None
}

/// The glow's opacity at texel `i` of [`FALLOFF_TEXELS`] counted from the picture's edge: high
/// against the picture, falling to nothing exactly at the screen's edge.
fn falloff(i: usize) -> f32 {
    let d = i as f32 / (FALLOFF_TEXELS - 1) as f32;
    (1.0 - d).max(0.0).powf(1.1)
}

/// The texture of one bar (straight alpha RGBA, in screen orientation): three texels along the
/// picture's edge, so their centres fall on the middles of the three colour regions, and
/// [`FALLOFF_TEXELS`] across it. `side` is 0 for the left / top bar (the picture is to its right /
/// below), 1 for the right / bottom one. Returns (width, height, pixels).
pub fn texels(colours: &[[u8; 3]; 3], pillar: bool, side: usize) -> (i32, i32, Vec<u8>) {
    let n = FALLOFF_TEXELS;
    let (w, h) = if pillar { (n, 3) } else { (3, n) };
    let mut px = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let (along, across) = if pillar { (y, x) } else { (x, y) };
            // Distance from the picture, in texels.
            let from_picture = if side == 0 { n - 1 - across } else { across };
            let o = (y * w + x) * 4;
            px[o..o + 3].copy_from_slice(&colours[along]);
            px[o + 3] = (falloff(from_picture) * 255.0).round() as u8;
        }
    }
    (w as i32, h as i32, px)
}

fn to_codes(p: &Palette) -> [[[u8; 3]; 3]; 2] {
    p.map(|side| side.map(|c| c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)))
}

fn lerp(a: &Palette, b: &Palette, t: f32) -> Palette {
    let mut out = *a;
    for s in 0..2 {
        for i in 0..3 {
            for c in 0..3 {
                out[s][i][c] = a[s][i][c] + (b[s][i][c] - a[s][i][c]) * t;
            }
        }
    }
    out
}

/// The slow brightness swell (0.88..1.0, about every 24 seconds), so even a still picture's glow
/// never stands still on the panel.
fn breath(now_ms: u32) -> f32 {
    let t = (now_ms % 24_000) as f32 / 24_000.0;
    0.94 + 0.06 * (t * std::f32::consts::TAU).sin()
}

/// Should the glow be up: the video playing, or paused for less than [`PAUSE_OFF_MS`] (and
/// loading or buffering in between, so a seek does not blink it away).
pub fn wanted(phase: &Phase, paused_since: Option<u32>, now_ms: u32) -> bool {
    match phase {
        Phase::Playing | Phase::Loading => true,
        Phase::Paused => paused_since.is_none_or(|t| now_ms.wrapping_sub(t) < PAUSE_OFF_MS),
        _ => false,
    }
}

/// The glow's colours and fades between frames: a pure state machine, stepped by [`draw`].
#[derive(Default)]
pub struct Glow {
    video: String,
    pillar: bool,
    /// The storyboard frame the target colours came from (sheet, x, y).
    frame: Option<(u32, u32, u32)>,
    from: Palette,
    to: Palette,
    t0: u32,
    has: bool,
    /// Overall opacity, 0..1, easing toward 1 while [`wanted`] and toward 0 otherwise.
    master: f32,
    last_ms: u32,
    paused_since: Option<u32>,
    tex: [u32; 2],
    uploaded: Option<(bool, [[[u8; 3]; 3]; 2])>,
    /// The video's own black borders on the two sides ([`black_edges`]): the glow covers them.
    border: [f32; 2],
}

impl Glow {
    /// Another video: start over, faded out.
    fn reset(&mut self, video: &str) {
        let tex = self.tex;
        *self = Glow { video: video.to_string(), tex, ..Glow::default() };
    }

    /// Colours as shown at `now`: the cross-fade from the previous frame's to the current one's.
    pub fn shown(&self, now_ms: u32) -> Palette {
        let t = (now_ms.wrapping_sub(self.t0) as f32 / FADE_MS as f32).clamp(0.0, 1.0);
        lerp(&self.from, &self.to, t * t * (3.0 - 2.0 * t))
    }

    /// New target colours (another storyboard frame): fade to them from what is shown now. The
    /// first colours of a video (or after the bars turned around) are taken as they are, since
    /// the glow itself fades in.
    pub fn retarget(&mut self, p: Palette, pillar: bool, now_ms: u32) {
        if !self.has || pillar != self.pillar {
            self.from = p;
        } else {
            self.from = self.shown(now_ms);
        }
        self.to = p;
        self.t0 = now_ms;
        self.pillar = pillar;
        self.has = true;
    }

    /// Eases [`Self::master`] toward `on` over `dt_ms`: in over about a second, out more slowly.
    pub fn step_master(&mut self, on: bool, dt_ms: u32) {
        let (target, tau) = if on && self.has { (1.0, 900.0) } else { (0.0, 1800.0) };
        let a = 1.0 - (-(dt_ms.min(200) as f32) / tau).exp();
        self.master += (target - self.master) * a;
        if (self.master - target).abs() < 0.004 {
            self.master = target;
        }
    }

    /// Still changing: a cross-fade or the overall fade is running.
    pub fn moving(&self, on: bool, now_ms: u32) -> bool {
        let target = if on && self.has { 1.0 } else { 0.0 };
        self.master != target || (self.master > 0.0 && now_ms.wrapping_sub(self.t0) < FADE_MS)
    }
}

thread_local! {
    static GLOW: std::cell::RefCell<Glow> = std::cell::RefCell::new(Glow::default());
}

/// The player screen, every frame, under the HUD and captions (inside its [`crate::ui::HdrDim`]):
/// steps the glow and paints it in the bars. `enabled` is the Ambient mode setting.
pub fn draw(p: Painter, st: &crate::player::Status, enabled: bool, now_ms: u32) {
    GLOW.with(|g| {
        let g = &mut *g.borrow_mut();
        if g.video != st.video_id {
            g.reset(&st.video_id);
            g.last_ms = now_ms;
        }
        let dt = now_ms.wrapping_sub(g.last_ms);
        g.last_ms = now_ms;
        if st.phase == Phase::Paused {
            g.paused_since.get_or_insert(now_ms);
        } else {
            g.paused_since = None;
        }
        let level = yt_core::storyboard::best_level(&st.extras.levels, 320);
        // The picture's size: as the decoder reports it, else the storyboard frame's shape.
        let (vw, vh) = match (st.width, st.height, level) {
            (w, h, _) if w > 0 && h > 0 => (w, h),
            (_, _, Some(l)) => (l.width, l.height),
            _ => (0, 0),
        };
        let zoom = match st.zoom {
            Zoom::Fit => Some(1.0),
            Zoom::Zoom => Some(1.25),
            Zoom::Fill => None,
        };
        // The comments shrink the picture into a window of its own: no bars to light.
        let bars = zoom.filter(|_| enabled && !crate::ui::comments_open()).and_then(|z| content_rect(vw, vh, z)).and_then(bars);
        if let (Some((pillar, _)), Some(level)) = (bars, level) {
            let fr = yt_core::storyboard::frame_at(level, st.position_ms, st.duration_ms);
            let key = (fr.sheet, fr.x, fr.y);
            if g.frame != Some(key) || g.pillar != pillar {
                // Asks for the sheet (once; the thumbnail worker decodes it and calls `note_sheet`).
                plx_ui::tex::warm_on(1, &fr.sheet_url, 0, 0, false);
                if let Some(s) = sheet(&fr.sheet_url) {
                    if let Some(pal) = edge_palette(&s, (fr.x, fr.y, fr.w, fr.h), pillar) {
                        g.retarget(pal, pillar, now_ms);
                        g.border = black_edges(&s, (fr.x, fr.y, fr.w, fr.h), pillar);
                        g.frame = Some(key);
                    }
                }
            }
        }
        let on = bars.is_some() && wanted(&st.phase, g.paused_since, now_ms);
        g.step_master(on, dt);
        if g.moving(on, now_ms) {
            crate::ui::animating();
        }
        let Some((pillar, rects)) = bars else { return };
        if g.master <= 0.0 || pillar != g.pillar {
            return;
        }
        let codes = to_codes(&g.shown(now_ms));
        if g.uploaded != Some((pillar, codes)) {
            for side in 0..2 {
                let (w, h, px) = texels(&codes[side], pillar, side);
                g.tex[side] = plx_gfx::gfx::upload_rgba(g.tex[side], w, h, px.as_ptr());
            }
            g.uploaded = Some((pillar, codes));
        }
        let ink = crate::ui::dimmed([1.0, 1.0, 1.0, g.master * PEAK * breath(now_ms)]);
        let content = content_rect(vw, vh, zoom.unwrap_or(1.0));
        let rects = content.map_or(rects, |c| reach_in(rects, c, pillar, g.border));
        for side in 0..2 {
            if g.tex[side] != 0 && rects[side].w > 0.0 && rects[side].h > 0.0 {
                p.tex_uv(g.tex[side], [0.0, 0.0, 1.0, 1.0], rects[side], 0.0, ink);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 20 x 10 frame (one sheet pixel per reduced pixel): grey, with black columns at the left
    /// (`l`) and right (`r`).
    fn bordered(l: u32, r: u32) -> Sheet {
        let (w, h) = (20u32, 10u32);
        let mut rgb = vec![120u8; (w * h * 3) as usize];
        for y in 0..h {
            for x in (0..l).chain(w - r..w) {
                let i = ((y * w + x) * 3) as usize;
                rgb[i..i + 3].copy_from_slice(&[4, 4, 4]);
            }
        }
        Sheet { w, h, f: 1, rgb }
    }

    #[test]
    fn a_videos_black_strips_are_found() {
        let e = black_edges(&bordered(2, 1), (0, 0, 20, 10), true);
        assert!((e[0] - 0.10).abs() < 1e-6 && (e[1] - 0.05).abs() < 1e-6, "{e:?}");
        assert_eq!(black_edges(&bordered(0, 0), (0, 0, 20, 10), true), [0.0, 0.0]);
        // A black frame is not all border: at most MAX_BORDER either side.
        let all = black_edges(&bordered(20, 0), (0, 0, 20, 10), true);
        assert!(all[0] <= MAX_BORDER + 1e-6, "{all:?}");
    }

    #[test]
    fn the_glow_reaches_over_the_strips_to_the_picture() {
        let c = content_rect(1440, 1080, 1.0).unwrap();
        let (pillar, rects) = bars(c).unwrap();
        let r = reach_in(rects, c, pillar, [0.10, 0.0]);
        assert!((r[0].w - (240.0 + 144.0 + OVERLAP)).abs() < 0.01, "left bar over a tenth of the picture: {:?}", r[0]);
        assert!((r[1].x - (1680.0 - OVERLAP)).abs() < 0.01 && (r[1].x + r[1].w - 1920.0).abs() < 0.01, "{:?}", r[1]);
    }

    /// A sheet of two 40x20 frames side by side: the first red on its left half and blue on its
    /// right, the second green on top and white below.
    fn test_sheet() -> Sheet {
        let (w, h) = (80u32, 20u32);
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let c = if x < 40 {
                    if x < 20 { [255, 0, 0] } else { [0, 0, 255] }
                } else if y < 10 {
                    [0, 255, 0]
                } else {
                    [255, 255, 255]
                };
                let o = ((y * w + x) * 4) as usize;
                px[o..o + 3].copy_from_slice(&c);
                px[o + 3] = 255;
            }
        }
        reduce(w, h, &px).unwrap()
    }

    #[test]
    fn reduce_keeps_small_sheets_and_shrinks_big_ones() {
        let s = test_sheet();
        assert_eq!((s.w, s.h, s.f), (80, 20, 1));
        let big = reduce(960, 540, &vec![128u8; 960 * 540 * 4]).unwrap();
        assert_eq!((big.w, big.h, big.f), (160, 90, 6));
        assert_eq!(&big.rgb[..3], &[128, 128, 128]);
        assert!(reduce(10, 10, &[0u8; 12]).is_none());
    }

    #[test]
    fn pillar_palette_reads_the_left_and_right_edges() {
        let s = test_sheet();
        let p = edge_palette(&s, (0, 0, 40, 20), true).unwrap();
        for i in 0..3 {
            assert_eq!(p[0][i], [MAX_INK, 0.0, 0.0], "left edge is red, tamed");
            assert_eq!(p[1][i], [0.0, 0.0, MAX_INK], "right edge is blue, tamed");
        }
    }

    #[test]
    fn letter_palette_reads_the_top_and_bottom_edges() {
        let s = test_sheet();
        let p = edge_palette(&s, (40, 0, 40, 20), false).unwrap();
        for i in 0..3 {
            assert_eq!(p[0][i], [0.0, MAX_INK, 0.0]);
            assert_eq!(p[1][i], [MAX_INK, MAX_INK, MAX_INK]);
        }
        assert!(edge_palette(&s, (78, 0, 2, 20), false).is_none(), "a frame too small to read");
    }

    #[test]
    fn bars_follow_the_shape_of_the_picture() {
        // 4:3: pillarbox, 240 px each side.
        let (pillar, r) = bars(content_rect(1440, 1080, 1.0).unwrap()).unwrap();
        assert!(pillar);
        assert_eq!((r[0].x, r[0].w, r[1].x, r[1].w), (0.0, 240.0, 1680.0, 240.0));
        // A vertical Short.
        let (pillar, r) = bars(content_rect(1080, 1920, 1.0).unwrap()).unwrap();
        assert!(pillar && (r[0].w - 656.25).abs() < 0.01);
        // 21:9: letterbox.
        let (pillar, r) = bars(content_rect(2560, 1080, 1.0).unwrap()).unwrap();
        assert!(!pillar);
        assert!((r[0].h - 135.0).abs() < 0.01 && (r[1].y - 945.0).abs() < 0.01);
        // 16:9 fills the screen; so does a 4:3 zoomed past its bars' width.
        assert!(bars(content_rect(1920, 1080, 1.0).unwrap()).is_none());
        assert!(bars(content_rect(1440, 1080, 1.4).unwrap()).is_none());
        // 1.85:1 leaves bars too thin to light.
        assert!(bars(content_rect(1998, 1080, 1.0).unwrap()).is_none());
        assert!(content_rect(0, 1080, 1.0).is_none());
    }

    #[test]
    fn texels_are_brightest_against_the_picture() {
        let cols = [[10, 20, 30], [40, 50, 60], [70, 80, 90]];
        // Left bar: the picture is on the right, so alpha grows to the right.
        let (w, h, px) = texels(&cols, true, 0);
        assert_eq!((w, h), (FALLOFF_TEXELS as i32, 3));
        let a = |x: usize, y: usize| px[(y * w as usize + x) * 4 + 3];
        assert!(a(0, 0) < 5 && a(w as usize - 1, 0) > 200);
        assert_eq!(&px[(2 * w as usize) * 4..(2 * w as usize) * 4 + 3], &[70, 80, 90], "bottom row: the bottom colour");
        // Bottom bar: the picture is above, so alpha falls downwards.
        let (w, h, px) = texels(&cols, false, 1);
        assert_eq!((w, h), (3, FALLOFF_TEXELS as i32));
        let a = |x: usize, y: usize| px[(y * w as usize + x) * 4 + 3];
        assert!(a(1, 0) > 200 && a(1, h as usize - 1) < 5);
        assert!((0..FALLOFF_TEXELS - 1).all(|i| falloff(i) > falloff(i + 1)));
    }

    #[test]
    fn colours_cross_fade_to_a_new_frame() {
        let red: Palette = [[[0.8, 0.0, 0.0]; 3]; 2];
        let blue: Palette = [[[0.0, 0.0, 0.8]; 3]; 2];
        let mut g = Glow::default();
        g.retarget(red, true, 1000);
        assert_eq!(g.shown(1000), red, "the first colours come as they are");
        g.retarget(blue, true, 5000);
        assert_eq!(g.shown(5000), red);
        let mid = g.shown(5000 + FADE_MS / 2);
        assert!((mid[0][0][0] - 0.4).abs() < 0.01 && (mid[0][0][2] - 0.4).abs() < 0.01);
        assert_eq!(g.shown(5000 + FADE_MS), blue);
        // A new frame halfway through a fade starts from what is on screen.
        g.retarget(red, true, 5000 + FADE_MS / 2);
        assert_eq!(g.shown(5000 + FADE_MS / 2), mid);
    }

    #[test]
    fn the_glow_fades_in_and_out_and_comes_to_rest() {
        let mut g = Glow::default();
        g.step_master(true, 16);
        assert_eq!(g.master, 0.0, "nothing to show before the first colours");
        g.retarget([[[0.5; 3]; 3]; 2], true, 0);
        for _ in 0..400 {
            g.step_master(true, 16);
        }
        assert_eq!(g.master, 1.0);
        assert!(!g.moving(true, 10_000));
        g.step_master(false, 16);
        assert!(g.master < 1.0 && g.moving(false, 10_000));
        for _ in 0..800 {
            g.step_master(false, 16);
        }
        assert_eq!(g.master, 0.0);
        assert!(!g.moving(false, 30_000));
    }

    #[test]
    fn a_long_pause_turns_the_glow_off() {
        assert!(wanted(&Phase::Playing, None, 0));
        assert!(wanted(&Phase::Paused, Some(1000), 1000 + PAUSE_OFF_MS - 1));
        assert!(!wanted(&Phase::Paused, Some(1000), 1000 + PAUSE_OFF_MS));
        assert!(!wanted(&Phase::Ended, None, 0));
        assert!(!wanted(&Phase::Failed(String::new()), None, 0));
    }

    #[test]
    fn breathing_stays_subtle() {
        for t in (0..48_000).step_by(500) {
            let b = breath(t);
            assert!((0.879..=1.0).contains(&b));
        }
    }
}

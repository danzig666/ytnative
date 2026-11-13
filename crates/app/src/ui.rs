//! Small UI building blocks shared by the screens: text drawing, the card source over the store,
//! and a vertical list (sidebar, menus, settings) that plugs into plx's focus engine.

use crate::host::YtHost;
use crate::store::Card;
use plx_machine::machine::{Cx, EntryId, FocusKey, GroupId};
use plx_ui::card_row::TileLabel;
use plx_ui::cards::CardSource;
use plx_ui::screen::{AxisMask, Dir, EdgeRule, ElemKind, GroupKind, GroupSpec, Placed, Seat, Step};
use plx_ui::widgets::Art;
use plx_ui::{Painter, Rect};
use std::ffi::CString;

pub const ALIGN_LEFT: i32 = 0;
pub const ALIGN_CENTER: i32 = 1;
pub const ALIGN_RIGHT: i32 = 2;

pub const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
pub const DIM: [f32; 4] = [1.0, 1.0, 1.0, 0.62];
pub const FAINT: [f32; 4] = [1.0, 1.0, 1.0, 0.35];
/// The accent colour (default: the app icon's red), chosen in settings.
static ACCENT_IDX: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
const ACCENTS: [[f32; 4]; 5] = [
    [1.0, 0.031, 0.278, 1.0],
    [0.16, 0.47, 1.0, 1.0],
    [0.12, 0.72, 0.38, 1.0],
    [0.56, 0.32, 0.96, 1.0],
    [1.0, 0.55, 0.1, 1.0],
];

pub fn accent() -> [f32; 4] {
    ACCENTS[ACCENT_IDX.load(std::sync::atomic::Ordering::Relaxed) as usize % ACCENTS.len()]
}

static CARD_COLS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(4);

/// Cards per row (3..=5), from the "Card size" setting.
pub fn card_cols() -> usize {
    CARD_COLS.load(std::sync::atomic::Ordering::Relaxed).clamp(3, 5) as usize
}

pub fn set_card_cols(n: u8) {
    CARD_COLS.store(n.clamp(3, 5), std::sync::atomic::Ordering::Relaxed);
}

// Off until the first frame applies the setting (which also sets the row room).
static TITLES_UNDER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Room every row keeps under its cards for the info panel (title on two lines, channel line).
const UNDER_EXTRA: f32 = 128.0;
/// The info panel under a card: gap above it, height, inner padding, corner radius.
const PANEL_GAP: f32 = 6.0;
const PANEL_H: f32 = 114.0;
const PANEL_PAD: f32 = 16.0;
const PANEL_RAD: f32 = 14.0;
/// Panel colours: unfocused (a lifted dark) and focused (light, dark text: SmartTube's focus).
const PANEL_IDLE: [f32; 4] = [1.0, 1.0, 1.0, 0.07];
const PANEL_FOCUS: [f32; 4] = [0.94, 0.94, 0.96, 1.0];
const PANEL_FOCUS_INK: [f32; 4] = [0.09, 0.09, 0.11, 1.0];
const PANEL_FOCUS_DIM: [f32; 4] = [0.09, 0.09, 0.11, 0.66];

/// SmartTube-style titles under every thumbnail (on) or plx's caption under the focused card only.
pub fn titles_under() -> bool {
    TITLES_UNDER.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_titles_under(on: bool) {
    TITLES_UNDER.store(on, std::sync::atomic::Ordering::Relaxed);
    plx_ui::card_row::set_label_band_extra(if on { UNDER_EXTRA } else { 0.0 });
}

thread_local! {
    /// The focused card's caption, deferred to the end of the page draw: (card rect on screen,
    /// title, detail).
    static FOCUSED_CAPTION: std::cell::RefCell<Option<(Rect, String, String)>> = const { std::cell::RefCell::new(None) };
    /// Marquee clock: (title it runs for, start).
    static MARQUEE: std::cell::RefCell<(String, Option<std::time::Instant>)> = const { std::cell::RefCell::new((String::new(), None)) };
}

thread_local! {
    /// Drawn cards per row this frame: (row centre y, height, leftmost x, rightmost edge).
    static ROWS_DRAWN: std::cell::RefCell<Vec<(f32, f32, f32, f32)>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn note_card(r: Rect) {
    ROWS_DRAWN.with(|v| {
        let mut v = v.borrow_mut();
        let cy = r.y + r.h / 2.0;
        match v.iter_mut().find(|e| (e.0 - cy).abs() < r.h / 2.0) {
            Some(e) => {
                e.2 = e.2.min(r.x);
                e.3 = e.3.max(r.x + r.w);
            }
            None => v.push((cy, r.h, r.x, r.x + r.w)),
        }
    });
}

/// Faint arrows at the ends of each card row that runs on past the screen edge (more videos
/// to the left / right). Rows are what the cards drawn this frame span.
pub fn draw_row_arrows(p: Painter, left: f32, right: f32) {
    let rows = ROWS_DRAWN.with(|v| std::mem::take(&mut *v.borrow_mut()));
    for (cy, _h, min_x, max_r) in rows {
        if min_x < left - 160.0 {
            fill(p, Rect::new(0.0, cy - 60.0, 64.0, 120.0), 0.0, [0.0, 0.0, 0.0, 0.35]);
            text(p, "\u{2039}", 32.0, cy - 50.0, 72, [1.0, 1.0, 1.0, 0.55], ALIGN_CENTER, false);
        }
        if max_r > right + 6.0 {
            fill(p, Rect::new(1856.0, cy - 60.0, 64.0, 120.0), 0.0, [0.0, 0.0, 0.0, 0.35]);
            text(p, "\u{203A}", 1888.0, cy - 50.0, 72, [1.0, 1.0, 1.0, 0.55], ALIGN_CENTER, false);
        }
    }
}

/// Forgets a deferred caption (call before a page draws its cards).
pub fn reset_focused_caption() {
    ROWS_DRAWN.with(|v| v.borrow_mut().clear());
    FOCUSED_CAPTION.with(|f| *f.borrow_mut() = None);
}

/// The selected card's caption, drawn after every card: the title on ONE line in the card's own
/// width, gliding in a loop when it does not fit (hold, glide to the end, hold, start over), and
/// the detail line under it. No box and nothing over the neighbours.
pub fn draw_focused_caption(p: Painter, m: &dyn plx_machine::machine::Measure, _min_x: f32) {
    let Some((r, title, detail)) = FOCUSED_CAPTION.with(|f| f.borrow_mut().take()) else { return };
    if !titles_under() {
        return;
    }
    const SIZE: i32 = 24;
    // The focused card's panel turns light (SmartTube's focus): it follows the card's pop.
    let panel = Rect::new(r.x, r.y + r.h + PANEL_GAP, r.w, PANEL_H);
    fill(p, panel, PANEL_RAD, PANEL_FOCUS);
    let (ix, iy, w) = (panel.x + PANEL_PAD, panel.y + 12.0, panel.w - 2.0 * PANEL_PAD);
    let tw = CString::new(title.as_str()).map_or(0.0, |c| m.width(&c, SIZE, true));
    if tw <= w + 0.5 {
        text(p, &title, ix, iy, SIZE, PANEL_FOCUS_INK, ALIGN_LEFT, true);
    } else if let (l1, Some(l2), _) = caption_lines(m, &title, "", w).clone() {
        // Two lines hold it: show them; only a title longer than that glides on one line.
        if CString::new(format!("{l1} {l2}")).map_or(0.0, |c| m.width(&c, SIZE, true)) <= 2.0 * w && !l2.ends_with('\u{2026}') {
            text(p, &l1, ix, iy, SIZE, PANEL_FOCUS_INK, ALIGN_LEFT, true);
            text(p, &l2, ix, iy + 31.0, SIZE, PANEL_FOCUS_INK, ALIGN_LEFT, true);
            if !detail.is_empty() {
                let d = m.fit_line(&detail, w, 20, false).to_string_lossy().into_owned();
                text(p, &d, ix, iy + 67.0, 20, PANEL_FOCUS_DIM, ALIGN_LEFT, false);
            }
            return;
        }
        marquee(p, m, &title, ix, iy, w, SIZE);
    } else {
        marquee(p, m, &title, ix, iy, w, SIZE);
    }
    if !detail.is_empty() {
        // Under a one-line title the channel line may take two lines.
        let (d1, d2) = wrap2(m, &detail, w, 20);
        text(p, &d1, ix, iy + 36.0, 20, PANEL_FOCUS_DIM, ALIGN_LEFT, false);
        if let Some(d2) = d2 {
            text(p, &d2, ix, iy + 62.0, 20, PANEL_FOCUS_DIM, ALIGN_LEFT, false);
        }
    }
}

/// A title that does not fit, gliding on one line (hold, glide to the end, hold, start over).
fn marquee(p: Painter, m: &dyn plx_machine::machine::Measure, title: &str, ix: f32, iy: f32, w: f32, size: i32) {
    let tw = CString::new(title).map_or(0.0, |c| m.width(&c, size, true));
    {
        const HOLD: f32 = 1.5;
        const SPEED: f32 = 70.0;
        let dist = tw - w + 24.0;
        let t = MARQUEE.with(|mq| {
            let mut mq = mq.borrow_mut();
            if mq.0 != title || mq.1.is_none() {
                *mq = (title.to_string(), Some(std::time::Instant::now()));
            }
            mq.1.unwrap().elapsed().as_secs_f32()
        });
        let cycle = 2.0 * HOLD + dist / SPEED;
        let off = ((t % cycle - HOLD) * SPEED).clamp(0.0, dist);
        {
            let _scope = plx_ui::screen::ClipScope::open_in(p, Rect::new(ix, iy - 4.0, w, 40.0));
            text(p, title, ix - off, iy, size, PANEL_FOCUS_INK, ALIGN_LEFT, true);
        }
        animating();
    }
}

type CaptionLines = (String, Option<String>, String);

/// [`wrap2`] of the title and the fitted detail line, cached by (title, detail, width).
fn caption_lines(m: &dyn plx_machine::machine::Measure, title: &str, detail: &str, w: f32) -> CaptionLines {
    thread_local! {
        static CACHE: std::cell::RefCell<std::collections::HashMap<(String, String, u32), CaptionLines>> = Default::default();
    }
    let key = (title.to_string(), detail.to_string(), w.round() as u32);
    if let Some(v) = CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return v;
    }
    let (l1, l2) = wrap2(m, title, w, 24);
    let d = if detail.is_empty() { String::new() } else { m.fit_line(detail, w, 20, false).to_string_lossy().into_owned() };
    let v = (l1, l2, d);
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 4000 {
            c.clear();
        }
        c.insert(key, v.clone());
    });
    v
}

/// `s` split into at most two lines of `w` px (word wrap; the second line ellipsised).
fn wrap2(m: &dyn plx_machine::machine::Measure, s: &str, w: f32, size: i32) -> (String, Option<String>) {
    let width = |t: &str| CString::new(t).map_or(0.0, |c| m.width(&c, size, false));
    if width(s) <= w {
        return (s.to_string(), None);
    }
    let mut first = String::new();
    let mut rest = s;
    for (i, word) in s.split_inclusive(' ').enumerate() {
        let trial = format!("{first}{word}");
        if i > 0 && width(trial.trim_end()) > w {
            break;
        }
        first = trial;
        rest = &s[first.len()..];
    }
    if first.is_empty() || rest.is_empty() {
        // One long word: hard-cut it.
        let cut: String = m.fit_line(s, w, size, false).to_string_lossy().into_owned();
        return (cut, None);
    }
    let second = m.fit_line(rest.trim_start(), w, size, false).to_string_lossy().into_owned();
    (first.trim_end().to_string(), Some(second))
}

/// A 16:9 card style that fits `cols` cards (with `gap`) into `width`.
pub fn card_style(cols: usize, width: f32, gap: f32) -> plx_ui::card_row::RowStyle {
    let w = ((width - (cols as f32 - 1.0) * gap) / cols as f32).floor();
    plx_ui::card_row::RowStyle { w, h: (w * 9.0 / 16.0).floor(), gap, ..plx_ui::card_row::RowStyle::HOME }
}

/// Comments panels open over the player (the player hides its controls, shrinks its captions).
pub static COMMENTS_OPEN: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

pub fn comments_open() -> bool {
    COMMENTS_OPEN.load(std::sync::atomic::Ordering::Relaxed) > 0
}

static ANIMATING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A time-driven animation (marquee, sliding sidebar, comments panel) needs another frame.
pub fn animating() {
    ANIMATING.store(true, std::sync::atomic::Ordering::Relaxed);
    plx_machine::idle::note_motion();
}

/// Taken by the main loop once per frame: present the next frame too.
pub fn take_animating() -> bool {
    ANIMATING.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// Set by the browse screen while it shows shelf rows: its page scrolls on `InputKind::Wheel`
/// (plx's Stack) instead of by focus.
pub static PAGE_SCROLLS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How a wheel notch / a pointer resting on the top or bottom edge reaches `screen`: as a page
/// scroll (`Wheel`), or as Up/Down focus moves (lists, grids).
/// Option menus open (over the player or a page): the wheel / pointer edges step their rows.
pub static MENUS_OPEN: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

pub fn menu_open() -> bool {
    MENUS_OPEN.load(std::sync::atomic::Ordering::Relaxed) > 0
}

pub fn wheel_scrolls_page(screen: Option<&str>) -> bool {
    if menu_open() {
        return false;
    }
    match screen {
        Some("player") | Some("comments") => true,
        Some("browse") => PAGE_SCROLLS.load(std::sync::atomic::Ordering::Relaxed),
        _ => false,
    }
}

static BLACK_BG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The app's ground: a dark slate (SmartTube draws its lists on black; a hair of lift keeps
/// cards and panels readable on LCDs too).
pub const GROUND: (f32, f32, f32) = (0.063, 0.066, 0.078);

pub fn set_black_background(on: bool) {
    BLACK_BG.store(on, std::sync::atomic::Ordering::Relaxed);
    plx_gfx::gfx::tokens::set_clear_black(on);
    plx_gfx::gfx::tokens::set_clear_app(GROUND);
}

/// The page background: the app's ground, or true black (OLED setting).
pub fn bg_rgb() -> (f32, f32, f32) {
    if BLACK_BG.load(std::sync::atomic::Ordering::Relaxed) {
        (0.0, 0.0, 0.0)
    } else {
        GROUND
    }
}

pub fn set_accent(name: &str) {
    let i = match name {
        "blue" => 1,
        "green" => 2,
        "purple" => 3,
        "orange" => 4,
        _ => 0,
    };
    ACCENT_IDX.store(i, std::sync::atomic::Ordering::Relaxed);
}
pub const PANEL: [f32; 4] = [0.06, 0.06, 0.08, 0.92];
pub const ROW_FOCUS: [f32; 4] = [1.0, 1.0, 1.0, 0.16];
pub const SCRIM: [f32; 4] = [0.0, 0.0, 0.0, 0.62];

thread_local! {
    /// Brightness of UI ink while it is drawn over HDR video (1 = normal).
    static UI_DIM: std::cell::Cell<f32> = const { std::cell::Cell::new(1.0) };
}

/// While alive, [`text`], [`fill`] and [`icon::draw`] draw darker: the panel runs at HDR
/// brightness, so the OSD and the comments over HDR video would glare otherwise.
pub struct HdrDim(f32);

impl HdrDim {
    /// Dims when `hdr` (the pipeline reports an HDR type) is on.
    pub fn begin(hdr: bool) -> HdrDim {
        let prev = UI_DIM.with(|d| d.replace(if hdr { 0.55 } else { 1.0 }));
        HdrDim(prev)
    }
}

impl Drop for HdrDim {
    fn drop(&mut self) {
        UI_DIM.with(|d| d.set(self.0));
    }
}

/// The playing video is HDR (as the pipeline reported it).
pub fn hdr_playing(st: &crate::player::Status) -> bool {
    !st.hdr.is_empty() && !st.hdr.eq_ignore_ascii_case("none")
}

/// `col` as drawn under the current [`HdrDim`]: bright ink is scaled down, dark scrims get a
/// little more opaque.
pub(crate) fn dimmed(col: [f32; 4]) -> [f32; 4] {
    let k = UI_DIM.with(|d| d.get());
    if k >= 1.0 {
        return col;
    }
    if col[0].max(col[1]).max(col[2]) < 0.12 {
        return [col[0], col[1], col[2], (col[3] * 1.3).min(1.0)];
    }
    [col[0] * k, col[1] * k, col[2] * k, col[3]]
}

/// Draws `s` with its top-left (or anchor edge per `align`) at x,y. Returns the text width.
pub fn text(p: Painter, s: &str, x: f32, y: f32, size: i32, col: [f32; 4], align: i32, bold: bool) -> f32 {
    let col = dimmed(col);
    let s = crate::i18n::tr(s);
    let Ok(c) = CString::new(s.replace('\0', "")) else { return 0.0 };
    p.text(c.as_ptr(), x, y, size, col, align, bold as i32)
}

/// Truncates to roughly `max` characters with an ellipsis (cheap, measure-free).
/// [`text`] in an extra face (1..=8, the subtitle fonts: 1-4 bold, 5-8 regular).
pub fn text_face(p: Painter, s: &str, x: f32, y: f32, size: i32, col: [f32; 4], align: i32, face: i32) -> f32 {
    let col = dimmed(col);
    let Ok(c) = CString::new(s.replace('\0', "")) else { return 0.0 };
    p.text(c.as_ptr(), x, y, size, col, align, (face.clamp(0, 8) << 4) | (face <= 4) as i32)
}

pub fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('\u{2026}');
        t
    }
}

pub fn fill(p: Painter, r: Rect, rad: f32, col: [f32; 4]) {
    let col = dimmed(col);
    p.rect(r, rad, col, col, 0.0);
}

pub fn fmt_time(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Cards of a feed for plx's Grid/Shelf.
pub struct CardsSrc<'a> {
    pub cards: &'a [Card],
    pub more: bool,
}

impl CardSource<YtHost> for CardsSrc<'_> {
    fn len(&self) -> usize {
        self.cards.len()
    }
    fn elem(&self, i: usize) -> u32 {
        self.cards[i].elem
    }
    fn index_of(&self, e: &u32) -> Option<usize> {
        self.cards.iter().position(|c| c.elem == *e)
    }
    fn art(&self, i: usize) -> Art<'_> {
        Art::Thumb { sid: 0, key: &self.cards[i].thumb, res: (480, 270) }
    }
    fn label(&self, i: usize) -> TileLabel {
        if titles_under() {
            // Every card carries its own caption (`overlay`); plx's focused block would repeat it.
            return TileLabel::from_lines([None, None]);
        }
        TileLabel::from_lines([Some(self.cards[i].title.clone()), Some(self.cards[i].detail.clone())])
    }
    fn progress(&self, i: usize) -> Option<f32> {
        self.cards[i].item.progress.map(|p| p as f32 / 100.0)
    }
    fn more(&self) -> bool {
        self.more
    }
    fn overlay(&self, p: Painter, i: usize, tile: &plx_ui::cards::Tile, m: &dyn plx_machine::machine::Measure) {
        if tile.scale <= 1.01 {
            note_card(p.to_screen(tile.rect).0);
        }
        if titles_under() {
            // Title (two lines) and channel / views / age under every thumbnail, as SmartTube does.
            let (r, c) = (tile.rect, &self.cards[i]);
            if tile.focused {
                // Drawn last, over the neighbours' captions (`draw_focused_caption`).
                let screen = p.to_screen(r).0;
                FOCUSED_CAPTION.with(|f| *f.borrow_mut() = Some((screen, c.title.clone(), c.detail.clone())));
                return self.badge(p, i, tile);
            }
            let panel = Rect::new(r.x, r.y + r.h + PANEL_GAP, r.w, PANEL_H);
            fill(p, panel, PANEL_RAD, PANEL_IDLE);
            let (x, y, w) = (panel.x + PANEL_PAD, panel.y + 12.0, panel.w - 2.0 * PANEL_PAD);
            // Wrapping measures a title word by word; done once per (card, width), not per frame
            // (the page is drawn several times a frame, and a scroll draws every frame).
            let (l1, l2, d) = caption_lines(m, &c.title, &c.detail, w);
            let ink = [1.0, 1.0, 1.0, 0.92];
            text(p, &l1, x, y, 24, ink, ALIGN_LEFT, true);
            if let Some(l2) = &l2 {
                text(p, l2, x, y + 31.0, 24, ink, ALIGN_LEFT, true);
            }
            if !d.is_empty() {
                text(p, &d, x, y + 67.0, 20, DIM, ALIGN_LEFT, false);
            }
        }
        self.badge(p, i, tile);
    }
}

impl CardsSrc<'_> {
    /// Duration / LIVE badge in the bottom-right corner of the thumbnail.
    fn badge(&self, p: Painter, i: usize, tile: &plx_ui::cards::Tile) {
        if let Some(b) = &self.cards[i].item.badge {
            let r = tile.rect;
            let w = 14.0 * b.chars().count() as f32 + 20.0;
            let badge = Rect::new(r.x + r.w - w - 10.0, r.y + r.h - 44.0, w, 34.0);
            let live = matches!(self.cards[i].item.kind, yt_core::model::ItemKind::Live);
            fill(p, badge, 6.0, if live { accent() } else { [0.0, 0.0, 0.0, 0.78] });
            text(p, b, badge.x + w / 2.0, badge.y + 4.0, 22, WHITE, ALIGN_CENTER, true);
        }
    }
}

/// One pointer stop: hovering focuses it, a click presses it (OK).
pub fn stop(f: &mut plx_ui::screen::DrawFrame<'_, '_, YtHost>, key: FocusKey<u32>, rect: Rect, clip: Rect) {
    use plx_ui::screen::{Activate, Hover, Stop};
    let p = f.painter;
    f.stop(p, Stop { key, rect, rest_rect: rect, clip, hover: Hover::Focus, activate: Activate::Press });
}

/// A page header's ground: the page background over `r`, fading out over its last 28 px, so
/// content scrolled up under the title is hidden instead of drawn over it.
pub fn header_band(p: Painter, r: Rect) {
    let c = bg_rgb();
    let bg = [c.0, c.1, c.2, 1.0];
    let fade = 28.0;
    p.rect(Rect::new(r.x, r.y, r.w, r.h - fade), 0.0, bg, bg, 0.0);
    p.rect(Rect::new(r.x, r.y + r.h - fade, r.w, fade), 0.0, bg, [c.0, c.1, c.2, 0.0], 0.0);
}

/// One row of a vertical list.
#[derive(Clone, Debug, Default)]
pub struct Row {
    pub label: String,
    pub value: Option<String>,
    /// Marks the active choice (current section, selected option).
    pub active: bool,
    /// An icon before the label (the sidebar).
    pub icon: Option<icon::Kind>,
}

impl Row {
    pub fn new(label: impl Into<String>) -> Row {
        Row { label: label.into(), ..Default::default() }
    }
    pub fn value(mut self, v: impl Into<String>) -> Row {
        self.value = Some(v.into());
        self
    }
    pub fn active(mut self, on: bool) -> Row {
        self.active = on;
        self
    }
    pub fn icon(mut self, k: icon::Kind) -> Row {
        self.icon = Some(k);
        self
    }
}

/// A focusable vertical list. Elements are `base + index`.
#[derive(Clone, Copy, Debug)]
pub struct VList {
    pub entry: EntryId,
    pub group: GroupId,
    pub base: u32,
    pub rect: Rect,
    pub row_h: f32,
    pub text_size: i32,
}

impl VList {
    pub fn rows_visible(&self) -> usize {
        (self.rect.h / self.row_h).floor().max(1.0) as usize
    }
    pub fn elem(&self, i: usize) -> u32 {
        self.base + i as u32
    }
    pub fn index(&self, e: u32, len: usize) -> Option<usize> {
        e.checked_sub(self.base).map(|i| i as usize).filter(|&i| i < len)
    }
    pub fn key(&self, i: usize) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: self.elem(i) }
    }
    /// First visible row so that `focus` stays on screen.
    pub fn top_for(&self, focus: Option<usize>, len: usize) -> usize {
        let vis = self.rows_visible();
        match focus {
            Some(f) if f >= vis => (f + 1 - vis).min(len.saturating_sub(vis)),
            _ => 0,
        }
    }
    pub fn row_rect(&self, i: usize, top: usize) -> Rect {
        Rect::new(self.rect.x, self.rect.y + (i as f32 - top as f32) * self.row_h, self.rect.w, self.row_h - 6.0)
    }
    pub fn spec(&self, len: usize) -> GroupSpec {
        GroupSpec {
            id: self.group,
            kind: GroupKind::Column,
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent: self.rect,
            len,
            elem: ElemKind::Control,
        }
    }
    pub fn place(&self, e: u32, len: usize, focus: Option<usize>) -> Option<Placed> {
        let i = self.index(e, len)?;
        let r = self.row_rect(i, self.top_for(focus, len));
        Some(Placed { rect: r, rest_rect: r, clip: self.rect, index: Some(i as u32) })
    }
    /// [`place`](Self::place) with the list scrolled to `top` (a list scrolled by the wheel).
    pub fn place_top(&self, e: u32, len: usize, top: usize) -> Option<Placed> {
        let i = self.index(e, len)?;
        let r = self.row_rect(i, top);
        Some(Placed { rect: r, rest_rect: r, clip: self.rect, index: Some(i as u32) })
    }
    pub fn neighbour(&self, key: FocusKey<u32>, dir: Dir, len: usize) -> Step<u32> {
        let Some(i) = self.index(key.elem, len) else { return Step::Edge };
        match dir {
            Dir::Up if i > 0 => Step::Move(self.key(i - 1)),
            Dir::Down if i + 1 < len => Step::Move(self.key(i + 1)),
            _ => Step::Edge,
        }
    }
    pub fn focused(&self, cx: &Cx<'_, YtHost>, len: usize) -> Option<usize> {
        cx.focus.current.filter(|k| k.entry == self.entry).and_then(|k| self.index(k.elem, len))
    }
    /// Registers the visible rows with the pointer hit map (Magic Remote hover / click).
    pub fn stops(&self, f: &mut plx_ui::screen::DrawFrame<'_, '_, YtHost>, len: usize) {
        let focus = self.focused(f.cx, len);
        let top = self.top_for(focus, len);
        for i in (top..len).take(self.rows_visible()) {
            stop(f, FocusKey { entry: self.entry, elem: self.elem(i) }, self.row_rect(i, top), self.rect);
        }
    }

    pub fn draw(&self, p: Painter, rows: &[Row], focus: Option<usize>) {
        let top = self.top_for(focus, rows.len());
        let vis = self.rows_visible();
        for (i, row) in rows.iter().enumerate().skip(top).take(vis) {
            let r = self.row_rect(i, top);
            let focused = focus == Some(i);
            if focused {
                fill(p, r, 12.0, ROW_FOCUS);
            }
            if row.active {
                fill(p, Rect::new(r.x, r.y + 12.0, 6.0, r.h - 24.0), 3.0, accent());
            }
            let ty = r.y + (r.h - self.text_size as f32 * 1.3) / 2.0;
            let col = if focused || row.active { WHITE } else { DIM };
            let mut lx = r.x + 28.0;
            if let Some(k) = row.icon {
                let s = self.text_size as f32 * 1.15;
                icon::draw(p, k, Rect::new(lx, r.y + (r.h - s) / 2.0, s, s), col, false);
                lx += s + 20.0;
            }
            text(p, &row.label, lx, ty, self.text_size, col, ALIGN_LEFT, focused || row.active);
            if let Some(v) = &row.value {
                text(p, v, r.x + r.w - 24.0, ty, self.text_size, if focused { WHITE } else { DIM }, ALIGN_RIGHT, false);
            }
        }
        if top > 0 {
            text(p, "\u{25B2}", self.rect.x + self.rect.w / 2.0, self.rect.y - 34.0, 22, FAINT, ALIGN_CENTER, false);
        }
        if top + vis < rows.len() {
            text(p, "\u{25BC}", self.rect.x + self.rect.w / 2.0, self.rect.y + self.rect.h, 22, FAINT, ALIGN_CENTER, false);
        }
    }
}

/// A drop-down list of an option's values (a combo box): opens beside the option's row, the
/// current value ticked; OK picks one, BACK closes. The screen routes focus to it while open.
pub struct ChoiceBox {
    /// The option row it belongs to.
    pub row: usize,
    /// Values as the option reports them (translated when drawn).
    pub options: Vec<String>,
    pub current: usize,
    pub list: VList,
}

pub const CHOICE_BASE: u32 = 0x7f00_0000;
pub const CHOICE_GROUP: GroupId = GroupId(77);

impl ChoiceBox {
    /// `anchor`: the option row's rect; the box opens to its right edge, below it when there is
    /// room, else above.
    pub fn new(entry: EntryId, row: usize, options: Vec<String>, current: usize, anchor: Rect) -> ChoiceBox {
        const ROW_H: f32 = 52.0;
        let n = options.len().min(12);
        let h = n as f32 * ROW_H + 16.0;
        let w = 440.0;
        let x = (anchor.x + anchor.w - w).clamp(20.0, 1900.0 - w);
        let y = if anchor.y + anchor.h + h <= 1060.0 { anchor.y + anchor.h } else { (anchor.y - h).max(20.0) };
        ChoiceBox {
            row,
            options,
            current,
            list: VList { entry, group: CHOICE_GROUP, base: CHOICE_BASE, rect: Rect::new(x + 8.0, y + 8.0, w - 16.0, h - 16.0), row_h: ROW_H, text_size: 26 },
        }
    }
    pub fn owns(&self, e: u32) -> bool {
        self.list.index(e, self.options.len()).is_some()
    }
    pub fn draw(&self, f: &mut plx_ui::screen::DrawFrame<'_, '_, YtHost>) {
        let p = f.painter;
        let r = self.list.rect;
        fill(p, Rect::new(r.x - 8.0, r.y - 8.0, r.w + 16.0, r.h + 16.0), 14.0, [0.13, 0.13, 0.16, 1.0]);
        let rows: Vec<Row> = self
            .options
            .iter()
            .enumerate()
            .map(|(i, o)| Row::new(crate::i18n::tr(o).to_string()).value(if i == self.current { "\u{2713}" } else { "" }).active(i == self.current))
            .collect();
        self.list.draw(p, &rows, self.list.focused(f.cx, rows.len()));
        self.list.stops(f, rows.len());
    }
}

/// Line icons (shapes after Lucide, ISC licence), drawn through plx's SVG mask renderer and tinted.
pub mod icon {
    use plx_ui::{Painter, Rect};
    const HEAD: &str = r##"<svg viewBox="0 0 24 24" fill="none" stroke="#ffffff" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">"##;
    const THUMB_UP: &str = r##"<path d="M7 10v12"/><path d="M15 5.88 14 10h5.83a2 2 0 0 1 1.92 2.56l-2.33 8A2 2 0 0 1 17.5 22H4a2 2 0 0 1-2-2v-8a2 2 0 0 1 2-2h2.76a2 2 0 0 0 1.79-1.11L12 2a3.13 3.13 0 0 1 3 3.88Z"/>"##;
    const THUMB_DOWN: &str = r##"<path d="M17 14V2"/><path d="M9 18.12 10 14H4.17a2 2 0 0 1-1.92-2.56l2.33-8A2 2 0 0 1 6.5 2H20a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-2.76a2 2 0 0 0-1.79 1.11L12 22a3.13 3.13 0 0 1-3-3.88Z"/>"##;
    const CHAT: &str = r##"<path d="M7.9 20A9 9 0 1 0 4 16.1L2 22Z"/>"##;
    const GEAR: &str = r##"<path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"/><circle cx="12" cy="12" r="3"/>"##;

    const PLAY: &str = r##"<path d="M6 3l14 9-14 9z"/>"##;
    const PAUSE: &str = r##"<path d="M6 4h4v16H6z"/><path d="M14 4h4v16h-4z"/>"##;
    const PREV: &str = r##"<path d="M19 20 9 12l10-8z"/><path d="M5 19V5"/>"##;
    const NEXT: &str = r##"<path d="M5 4l10 8-10 8z"/><path d="M19 5v14"/>"##;
    const REPEAT: &str = r##"<path d="m17 2 4 4-4 4"/><path d="M3 11v-1a4 4 0 0 1 4-4h14"/><path d="m7 22-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/>"##;
    const REPEAT1: &str = r##"<path d="m17 2 4 4-4 4"/><path d="M3 11v-1a4 4 0 0 1 4-4h14"/><path d="m7 22-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/><path d="M11 10h1v4"/>"##;
    const AUTOPLAY: &str = r##"<path d="M5 5l9 7-9 7z"/><path d="M14 5l9 7-9 7"/>"##;
    const PAUSE_END: &str = r##"<circle cx="12" cy="12" r="10"/><path d="M10 15V9"/><path d="M14 15V9"/>"##;
    const GAUGE: &str = r##"<path d="m12 14 4-4"/><path d="M3.34 19a10 10 0 1 1 17.32 0"/>"##;
    const LIST_PLUS: &str = r##"<path d="M11 12H3"/><path d="M16 6H3"/><path d="M16 18H3"/><path d="M18 9v6"/><path d="M21 12h-6"/>"##;
    const BELL: &str = r##"<path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9"/><path d="M10.3 21a1.94 1.94 0 0 0 3.4 0"/>"##;
    const INFO: &str = r##"<circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/>"##;
    const SHIELD: &str = r##"<path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z"/><path d="M10 9l5 3-5 3z"/>"##;
    const LIST: &str = r##"<path d="M3 6h.01"/><path d="M3 12h.01"/><path d="M3 18h.01"/><path d="M8 6h13"/><path d="M8 12h13"/><path d="M8 18h13"/>"##;

    const HOME: &str = r##"<path d="M15 21v-8a1 1 0 0 0-1-1h-4a1 1 0 0 0-1 1v8"/><path d="M3 10a2 2 0 0 1 .709-1.528l7-5.999a2 2 0 0 1 2.582 0l7 5.999A2 2 0 0 1 21 10v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>"##;
    const FLAME: &str = r##"<path d="M8.5 14.5A2.5 2.5 0 0 0 11 12c0-1.38-.5-2-1-3-1.072-2.143-.224-4.054 2-6 .5 2.5 2 4.9 4 6.5 2 1.6 3 3.5 3 5.5a7 7 0 1 1-14 0c0-1.153.433-2.294 1-3a2.5 2.5 0 0 0 2.5 2.5z"/>"##;
    const SUBS: &str = r##"<path d="M4 7h16a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V9a2 2 0 0 1 2-2z"/><path d="M6 3h12"/><path d="M10 11l5 3-5 3z"/>"##;
    const USERS: &str = r##"<path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M22 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.13a4 4 0 0 1 0 7.75"/>"##;
    const HISTORY: &str = r##"<path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/><path d="M3 3v5h5"/><path d="M12 7v5l4 2"/>"##;
    const LIBRARY: &str = r##"<path d="m16 6 4 14"/><path d="M12 6v14"/><path d="M8 8v12"/><path d="M4 4v16"/>"##;
    const MUSIC: &str = r##"<path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/>"##;
    const GAMEPAD: &str = r##"<path d="M6 12h4"/><path d="M8 10v4"/><path d="M15 13h.01"/><path d="M18 11h.01"/><path d="M4 6h16a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2z"/>"##;
    const NEWS: &str = r##"<path d="M4 22h16a2 2 0 0 0 2-2V4a2 2 0 0 0-2-2H8a2 2 0 0 0-2 2v16a2 2 0 0 1-2 2Zm0 0a2 2 0 0 1-2-2v-9c0-1.1.9-2 2-2h2"/><path d="M18 14h-8"/><path d="M15 18h-5"/><path d="M10 6h8v4h-8V6Z"/>"##;
    const MIC: &str = r##"<path d="M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><path d="M12 19v3"/>"##;
    const LIVE: &str = r##"<path d="M4.9 19.1C1 15.2 1 8.8 4.9 4.9"/><path d="M7.8 16.2c-2.3-2.3-2.3-6.1 0-8.5"/><circle cx="12" cy="12" r="2"/><path d="M16.2 7.8c2.3 2.3 2.3 6.1 0 8.5"/><path d="M19.1 4.9C23 8.8 23 15.1 19.1 19"/>"##;
    const FILM: &str = r##"<path d="M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z"/><path d="M7 3v18"/><path d="M3 7.5h4"/><path d="M3 12h18"/><path d="M3 16.5h4"/><path d="M17 3v18"/><path d="M17 7.5h4"/><path d="M17 16.5h4"/>"##;
    const TROPHY: &str = r##"<path d="M6 9H4.5a2.5 2.5 0 0 1 0-5H6"/><path d="M18 9h1.5a2.5 2.5 0 0 0 0-5H18"/><path d="M4 22h16"/><path d="M10 14.66V17c0 .55-.47.98-.97 1.21C7.85 18.75 7 20.24 7 22"/><path d="M14 14.66V17c0 .55.47.98.97 1.21C16.15 18.75 17 20.24 17 22"/><path d="M18 2H6v7a6 6 0 0 0 12 0V2Z"/>"##;
    const TRENDING: &str = r##"<path d="M22 7 13.5 15.5 8.5 10.5 2 17"/><path d="M16 7h6v6"/>"##;
    const SEARCH: &str = r##"<circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/>"##;
    const USER: &str = r##"<path d="M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2"/><circle cx="12" cy="7" r="4"/>"##;
    const STAR: &str = r##"<path d="M12 2l3.09 6.26L22 9.27l-5 4.87 1.18 6.88L12 17.77l-6.18 3.25L7 14.14 2 9.27l6.91-1.01L12 2z"/>"##;
    const FOLDER: &str = r##"<path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/>"##;

    #[derive(Clone, Copy, PartialEq, Debug)]
    pub enum Kind {
        ThumbUp,
        ThumbDown,
        Chat,
        Gear,
        Cc,
        Play,
        Pause,
        Prev,
        Next,
        List,
        Home,
        Flame,
        Subs,
        Users,
        History,
        Library,
        Music,
        Gamepad,
        News,
        Mic,
        Live,
        Film,
        Trophy,
        Trending,
        Search,
        User,
        Star,
        Folder,
        Repeat,
        Repeat1,
        Autoplay,
        PauseEnd,
        Gauge,
        ListPlus,
        Bell,
        Info,
        Shield,
    }

    fn svg(k: Kind, filled: bool) -> &'static str {
        use std::sync::OnceLock;
        static S: OnceLock<Vec<String>> = OnceLock::new();
        let all = S.get_or_init(|| {
            let mut v = Vec::new();
            for body in [
                THUMB_UP, THUMB_DOWN, CHAT, GEAR, PLAY, PAUSE, PREV, NEXT, LIST, HOME, FLAME, SUBS, USERS, HISTORY, LIBRARY, MUSIC, GAMEPAD,
                NEWS, MIC, LIVE, FILM, TROPHY, TRENDING, SEARCH, USER, STAR, FOLDER, REPEAT, REPEAT1, AUTOPLAY, PAUSE_END, GAUGE,
                LIST_PLUS, BELL, INFO, SHIELD,
            ] {
                v.push(format!("{HEAD}{body}</svg>"));
                v.push(format!("{}{body}</svg>", HEAD.replace(r##"fill="none""##, r##"fill="#ffffff""##)));
            }
            v
        });
        let i = match k {
            Kind::ThumbUp => 0,
            Kind::ThumbDown => 2,
            Kind::Chat => 4,
            Kind::Gear | Kind::Cc => 6,
            Kind::Play => 8,
            Kind::Pause => 10,
            Kind::Prev => 12,
            Kind::Next => 14,
            Kind::List => 16,
            Kind::Home => 18,
            Kind::Flame => 20,
            Kind::Subs => 22,
            Kind::Users => 24,
            Kind::History => 26,
            Kind::Library => 28,
            Kind::Music => 30,
            Kind::Gamepad => 32,
            Kind::News => 34,
            Kind::Mic => 36,
            Kind::Live => 38,
            Kind::Film => 40,
            Kind::Trophy => 42,
            Kind::Trending => 44,
            Kind::Search => 46,
            Kind::User => 48,
            Kind::Star => 50,
            Kind::Folder => 52,
            Kind::Repeat => 54,
            Kind::Repeat1 => 56,
            Kind::Autoplay => 58,
            Kind::PauseEnd => 60,
            Kind::Gauge => 62,
            Kind::ListPlus => 64,
            Kind::Bell => 66,
            Kind::Info => 68,
            Kind::Shield => 70,
        };
        &all[i + filled as usize]
    }

    /// Draws icon `k` in the square `r`, `filled` for an active state.
    pub fn draw(p: Painter, k: Kind, r: Rect, tint: [f32; 4], filled: bool) {
        let tint = super::dimmed(tint);
        if k == Kind::Cc {
            plx_ui::icons::draw(p, plx_ui::icons::Icon::Cc, r, tint);
            return;
        }
        plx_ui::icons::draw_svg(p, plx_ui::icons::Svg::new(svg(k, filled)), r, tint);
    }
}

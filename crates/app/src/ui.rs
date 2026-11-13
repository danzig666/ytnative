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
/// Room every row keeps under its cards for the title (two lines) and the channel line.
const UNDER_EXTRA: f32 = 84.0;

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
    let tw = CString::new(title.as_str()).map_or(0.0, |c| m.width(&c, SIZE, true));
    let (ix, iy, w) = (r.x, r.y + r.h + 12.0, r.w);
    if tw <= w + 0.5 {
        text(p, &title, ix, iy, SIZE, WHITE, ALIGN_LEFT, true);
    } else {
        const HOLD: f32 = 1.5;
        const SPEED: f32 = 70.0;
        let dist = tw - w + 24.0;
        let t = MARQUEE.with(|mq| {
            let mut mq = mq.borrow_mut();
            if mq.0 != title || mq.1.is_none() {
                *mq = (title.clone(), Some(std::time::Instant::now()));
            }
            mq.1.unwrap().elapsed().as_secs_f32()
        });
        let cycle = 2.0 * HOLD + dist / SPEED;
        let off = ((t % cycle - HOLD) * SPEED).clamp(0.0, dist);
        {
            let _scope = plx_ui::screen::ClipScope::open_in(p, Rect::new(ix, iy - 4.0, w, 40.0));
            text(p, &title, ix - off, iy, SIZE, WHITE, ALIGN_LEFT, true);
        }
        animating();
    }
    if !detail.is_empty() {
        let d = m.fit_line(&detail, w, 20, false).to_string_lossy().into_owned();
        text(p, &d, ix, iy + 33.0, 20, DIM, ALIGN_LEFT, false);
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

pub fn set_black_background(on: bool) {
    BLACK_BG.store(on, std::sync::atomic::Ordering::Relaxed);
    plx_gfx::gfx::tokens::set_clear_black(on);
}

/// The page background: plx's ground, or true black (OLED setting).
pub fn bg_rgb() -> (f32, f32, f32) {
    if BLACK_BG.load(std::sync::atomic::Ordering::Relaxed) {
        (0.0, 0.0, 0.0)
    } else {
        plx_ui::theme::CLEAR_RGB
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
            let y = r.y + r.h + 12.0;
            let ink = if tile.focused { WHITE } else { [1.0, 1.0, 1.0, 0.86] };
            // Wrapping measures a title word by word; done once per (card, width), not per frame
            // (the page is drawn several times a frame, and a scroll draws every frame).
            let (l1, l2, d) = caption_lines(m, &c.title, &c.detail, r.w);
            text(p, &l1, r.x, y, 24, ink, ALIGN_LEFT, tile.focused);
            let mut dy = 31.0;
            if let Some(l2) = &l2 {
                text(p, l2, r.x, y + dy, 24, ink, ALIGN_LEFT, tile.focused);
                dy += 31.0;
            }
            if !d.is_empty() {
                text(p, &d, r.x, y + dy + 2.0, 20, DIM, ALIGN_LEFT, false);
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
            text(p, &row.label, r.x + 28.0, ty, self.text_size, col, ALIGN_LEFT, focused || row.active);
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

    #[derive(Clone, Copy, PartialEq)]
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
    }

    fn svg(k: Kind, filled: bool) -> &'static str {
        use std::sync::OnceLock;
        static S: OnceLock<Vec<String>> = OnceLock::new();
        let all = S.get_or_init(|| {
            let mut v = Vec::new();
            for body in [THUMB_UP, THUMB_DOWN, CHAT, GEAR, PLAY, PAUSE, PREV, NEXT] {
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

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

/// Draws `s` with its top-left (or anchor edge per `align`) at x,y. Returns the text width.
pub fn text(p: Painter, s: &str, x: f32, y: f32, size: i32, col: [f32; 4], align: i32, bold: bool) -> f32 {
    let Ok(c) = CString::new(s.replace('\0', "")) else { return 0.0 };
    p.text(c.as_ptr(), x, y, size, col, align, bold as i32)
}

/// Truncates to roughly `max` characters with an ellipsis (cheap, measure-free).
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
        TileLabel::from_lines([Some(self.cards[i].title.clone()), Some(self.cards[i].detail.clone())])
    }
    fn progress(&self, i: usize) -> Option<f32> {
        self.cards[i].item.progress.map(|p| p as f32 / 100.0)
    }
    fn more(&self) -> bool {
        self.more
    }
    fn overlay(&self, p: Painter, i: usize, tile: &plx_ui::cards::Tile, _m: &dyn plx_machine::machine::Measure) {
        // Duration / LIVE badge in the bottom-right corner of the thumbnail.
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
    fn row_rect(&self, i: usize, top: usize) -> Rect {
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

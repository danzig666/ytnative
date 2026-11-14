//! Search: an on-screen keyboard, the query line and live suggestions. OK on a suggestion or on
//! the Search key opens the results page.
//!
//! The same keyboard types names ([`SearchScreen::name`]): a playlist on this TV is named with
//! it; the Save key keeps the name and goes back.

use crate::host::{LocalOp, NamePurpose, YtArg, YtFx, YtHost, YtMsg};
use crate::store::FeedKey;
use crate::ui::{self, Row, VList};
use plx_machine::machine::{Canon, Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, LogicalState, Machine, NavOp, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{
    AxisMask, At, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Placed, RenderStrategy, Screen, Seat, Step,
};
use plx_ui::Rect;
use std::borrow::Cow;

const G_KEYS: GroupId = GroupId(1);
const G_SUG: GroupId = GroupId(2);
const KEY_BASE: u32 = 0x1000_0000;
const SUG_BASE: u32 = 0x2000_0000;

const COLS: usize = 8;
const KEY_W: f32 = 92.0;
const KEY_H: f32 = 84.0;
const KEYS_X: f32 = 100.0;
const KEYS_Y: f32 = 300.0;

#[derive(Clone, Copy, PartialEq)]
enum K {
    Ch(char),
    Space,
    Back,
    Clear,
    /// Capital letters on / off (names only).
    Shift,
    /// The accented letters in place of the plain ones, and back.
    Accents,
    Go,
}

/// The plain letters.
const LETTERS: &str = "abcdefghijklmnopqrstuvwxyz";
/// The accented letters (Hungarian, German, Spanish, French and a few more), in the plain
/// letters' places.
const ACCENTED: &str = "áéíóöőúüűäßñçèêàâîôûëïœæãõ";

/// The keys: the letters (plain or accented), digits, marks and the controls. Names also get a
/// Shift key for capital letters.
fn keys_for(naming: bool, accents: bool) -> Vec<K> {
    let mut v: Vec<K> = if accents { ACCENTED } else { LETTERS }.chars().chain("1234567890".chars()).map(K::Ch).collect();
    v.extend([K::Ch('-'), K::Ch('\''), K::Space, K::Back, K::Clear, K::Accents]);
    if naming {
        v.push(K::Shift);
    }
    v.push(K::Go);
    v
}

/// A letter as a capital; "ß" stays as it is (it never starts a word, and "SS" is two letters).
fn capital(c: char) -> String {
    if c == 'ß' { c.to_string() } else { c.to_uppercase().collect() }
}

fn key_label(k: K, naming: bool, upper: bool, accents: bool) -> String {
    match k {
        K::Ch(c) if upper => capital(c),
        K::Ch(c) => c.to_string(),
        K::Space => "Space".into(),
        K::Back => "\u{232B}".into(),
        K::Clear => "Clear".into(),
        // Shows what the letters become.
        K::Shift if upper => "abc".into(),
        K::Shift => "ABC".into(),
        K::Accents if accents => "a\u{2013}z".into(),
        K::Accents => "\u{E1}\u{E9}".into(),
        K::Go if naming => "Save".into(),
        K::Go => "Search".into(),
    }
}

struct State(String);
impl LogicalState for State {
    fn write(&self, c: &mut Canon) {
        c.str(&self.0);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&self.0);
    }
}

pub struct SearchScreen {
    entry: EntryId,
    /// Set when the keyboard types a name instead of a search.
    naming: Option<NamePurpose>,
    /// The letters are typed as capitals.
    upper: bool,
    /// The accented letters are shown in place of the plain ones.
    accents: bool,
    query: String,
    recent: Vec<String>,
    sug: VList,
    state: State,
}

impl SearchScreen {
    pub fn new(entry: EntryId) -> Self {
        SearchScreen {
            entry,
            naming: None,
            upper: false,
            accents: false,
            query: String::new(),
            recent: yt_core::search_history::SearchHistory::load().list().iter().take(10).cloned().collect(),
            sug: VList { entry, group: G_SUG, base: SUG_BASE, rect: Rect::new(1000.0, 300.0, 820.0, 700.0), row_h: 70.0, text_size: 30 },
            state: State(String::new()),
        }
    }
    /// The keyboard typing a name, the field starting with the current name (when renaming).
    pub fn name(entry: EntryId, purpose: NamePurpose) -> Self {
        let mut s = Self::new(entry);
        s.recent.clear();
        s.query = match &purpose {
            NamePurpose::RenameLocal(_, t) => t.clone(),
            NamePurpose::NewLocal(_) => String::new(),
        };
        // A new name starts with a capital letter.
        s.upper = s.query.is_empty();
        s.state = State(s.query.clone());
        s.naming = Some(purpose);
        s
    }
    fn keys(&self) -> Vec<K> {
        keys_for(self.naming.is_some(), self.accents)
    }
    fn key_rect(i: usize) -> Rect {
        Rect::new(KEYS_X + (i % COLS) as f32 * (KEY_W + 12.0), KEYS_Y + (i / COLS) as f32 * (KEY_H + 12.0), KEY_W, KEY_H)
    }
    fn kkey(&self, i: usize) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: KEY_BASE + i as u32 }
    }
    /// Recent searches while nothing is typed, live suggestions after.
    fn suggestions<'a>(&'a self, cx: &'a Cx<'_, YtHost>) -> &'a [String] {
        if self.naming.is_some() {
            &[]
        } else if self.query.is_empty() {
            &self.recent
        } else {
            &cx.views.store.suggestions
        }
    }
    fn search(&self, q: &str, record: bool, fx: &mut Effects<'_, YtHost>) {
        if !q.trim().is_empty() {
            if record {
                let _ = yt_core::search_history::record(q.trim());
            }
            fx.push(Fx::Nav(NavOp::Push(YtArg::Feed(FeedKey::Search { query: q.trim().to_string(), params: None }))));
        }
    }
    /// Save: the typed name goes to its purpose and the keyboard closes. A blank name does
    /// nothing but say so.
    fn save_name(&self, fx: &mut Effects<'_, YtHost>) {
        let Some(purpose) = &self.naming else { return };
        let title = self.query.trim().to_string();
        if title.is_empty() {
            fx.push(Fx::App(YtFx::Toast("Type a name first".into())));
            return;
        }
        let op = match purpose {
            NamePurpose::NewLocal(videos) => LocalOp::Create { title, videos: videos.clone() },
            NamePurpose::RenameLocal(id, _) => LocalOp::Rename { list_id: id.clone(), title },
        };
        fx.push(Fx::App(YtFx::Local(op)));
        fx.push(Fx::Nav(NavOp::Pop));
    }
    fn press(&mut self, k: K, record: bool, fx: &mut Effects<'_, YtHost>) {
        match k {
            K::Ch(c) if self.upper && self.naming.is_some() => {
                self.query.push_str(&capital(c));
                // As on a phone keyboard: one capital letter, then small letters again.
                self.upper = false;
            }
            K::Ch(c) => self.query.push(c),
            K::Shift => self.upper = !self.upper,
            K::Accents => self.accents = !self.accents,
            K::Go if self.naming.is_some() => {
                self.save_name(fx);
                return;
            }
            K::Space => self.query.push(' '),
            K::Back => {
                self.query.pop();
            }
            K::Clear => self.query.clear(),
            K::Go => {
                let q = self.query.clone();
                self.search(&q, record, fx);
                return;
            }
        }
        self.state = State(self.query.clone());
        if !self.query.is_empty() && self.naming.is_none() {
            fx.push(Fx::App(YtFx::Suggest(self.query.clone())));
        }
        fx.invalidate(plx_machine::present::Provenance::Input);
    }
}

impl Machine<YtHost> for SearchScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        if let Some(a) = super::common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        match ev {
            ScreenEvent::Activate(e) if (KEY_BASE..SUG_BASE).contains(e) => {
                if let Some(k) = self.keys().get((e - KEY_BASE) as usize).copied() {
                    self.press(k, cx.views.store.settings.search_history, fx);
                }
                Handled::Yes
            }
            ScreenEvent::Activate(e) if *e >= SUG_BASE => {
                if let Some(q) = self.suggestions(cx).get((e - SUG_BASE) as usize).cloned() {
                    self.search(&q, cx.views.store.settings.search_history, fx);
                }
                Handled::Yes
            }
            ScreenEvent::Async(_, YtMsg::Suggestions) => {
                fx.invalidate(plx_machine::present::Provenance::Lifecycle);
                Handled::Yes
            }
            ScreenEvent::Input(_) => Handled::No,
            _ => Handled::Yes,
        }
    }
}

impl Focusable<YtHost> for SearchScreen {
    fn groups(&self, cx: &Cx<'_, YtHost>, out: &mut Vec<GroupSpec>) {
        let n = self.keys().len();
        out.push(GroupSpec {
            id: G_KEYS,
            kind: GroupKind::Grid { cols: COLS, holes: &[] },
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent: Rect::new(KEYS_X, KEYS_Y, COLS as f32 * (KEY_W + 12.0), (n.div_ceil(COLS)) as f32 * (KEY_H + 12.0)),
            len: n,
            elem: ElemKind::Control,
        });
        let s = self.suggestions(cx).len();
        if s > 0 {
            out.push(self.sug.spec(s));
        }
    }
    fn group_of(&self, key: &u32, _cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        Some(if *key >= SUG_BASE { G_SUG } else { G_KEYS })
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, YtHost>) -> Step<u32> {
        let sugs = self.suggestions(cx).len();
        if key.elem >= SUG_BASE {
            if dir == Dir::Left {
                return Step::Move(self.kkey(COLS - 1));
            }
            return self.sug.neighbour(key, dir, sugs);
        }
        let n = self.keys().len();
        let i = (key.elem - KEY_BASE) as usize;
        let (r, c) = (i / COLS, i % COLS);
        let to = match dir {
            Dir::Left if c > 0 => Some(i - 1),
            Dir::Right if c + 1 < COLS && i + 1 < n => Some(i + 1),
            Dir::Right if sugs > 0 => return Step::Move(self.sug.key(0)),
            Dir::Up if r > 0 => Some(i - COLS),
            Dir::Down if i + COLS < n => Some(i + COLS),
            Dir::Down if r + 1 < n.div_ceil(COLS) => Some(n - 1),
            _ => None,
        };
        to.map_or(Step::Edge, |j| Step::Move(self.kkey(j)))
    }
    fn place(&self, key: &u32, cx: &Cx<'_, YtHost>, _at: At) -> Option<Placed> {
        if *key >= SUG_BASE {
            let n = self.suggestions(cx).len();
            return self.sug.place(*key, n, self.sug.focused(cx, n));
        }
        let i = key.checked_sub(KEY_BASE)? as usize;
        (i < self.keys().len()).then(|| {
            let r = Self::key_rect(i);
            Placed { rect: r, rest_rect: r, clip: Rect::new(0.0, 0.0, 1920.0, 1080.0), index: Some(i as u32) }
        })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        if want.elem >= SUG_BASE && self.sug.index(want.elem, self.suggestions(cx).len()).is_none() {
            return self.kkey(COLS - 1);
        }
        if want.elem < KEY_BASE {
            return self.kkey(0);
        }
        want
    }
    fn seat(&self, g: GroupId, _from: Placed, _cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        if g == G_SUG {
            self.sug.key(0)
        } else {
            self.kkey(0)
        }
    }
}

impl Screen<YtHost> for SearchScreen {
    fn name(&self) -> &'static str {
        "search"
    }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, YtHost>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, YtHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, YtHost>) {
        let p = f.painter;
        let cx = f.cx;
        for i in 0..self.keys().len() {
            ui::stop(f, self.kkey(i), Self::key_rect(i), Rect::new(0.0, 0.0, 1920.0, 1080.0));
        }
        let heading = match &self.naming {
            None => "Search",
            Some(NamePurpose::NewLocal(_)) => "Name of the new playlist on this TV",
            Some(NamePurpose::RenameLocal(..)) => "New name of the playlist",
        };
        ui::text(p, heading, 100.0, 60.0, 46, ui::WHITE, ui::ALIGN_LEFT, true);
        // Query field
        let field = Rect::new(100.0, 150.0, 1720.0, 100.0);
        ui::fill(p, field, 18.0, [1.0, 1.0, 1.0, 0.08]);
        // The query as typed (its end, with the cursor, when it is longer than the field).
        let room = field.w - 72.0;
        let shown = if self.query.is_empty() && self.naming.is_some() {
            crate::i18n::tr("Type the name with the keyboard below\u{2026}").to_string()
        } else if self.query.is_empty() {
            crate::i18n::tr("Type with the keyboard below\u{2026}").to_string()
        } else {
            let mut q: String = format!("{}|", self.query);
            while f.measure.width_str(&q, 40, false) > room && q.chars().count() > 2 {
                q = format!("\u{2026}{}", q.chars().skip(2).collect::<String>());
            }
            q
        };
        ui::text_raw(p, &shown, field.x + 36.0, field.y + 26.0, 40, if self.query.is_empty() { ui::FAINT } else { ui::WHITE }, ui::ALIGN_LEFT, false);
        // Keyboard
        let focused = cx.focus.current.filter(|k| k.entry == self.entry).map(|k| k.elem);
        let naming = self.naming.is_some();
        for (i, k) in self.keys().into_iter().enumerate() {
            let r = Self::key_rect(i);
            let is_focus = focused == Some(KEY_BASE + i as u32);
            let bg = if is_focus { ui::WHITE } else if k == K::Go { ui::accent() } else { [1.0, 1.0, 1.0, 0.10] };
            ui::fill(p, r, 14.0, bg);
            let size = if matches!(k, K::Ch(_)) { 36 } else { 22 };
            let ink = if is_focus { [0.0, 0.0, 0.0, 1.0] } else { ui::WHITE };
            ui::text(p, &key_label(k, naming, self.upper, self.accents), r.x + r.w / 2.0, r.y + (r.h - size as f32 * 1.25) / 2.0, size, ink, ui::ALIGN_CENTER, true);
        }
        // Suggestions
        let sugs = self.suggestions(cx);
        if !sugs.is_empty() {
            let rows: Vec<Row> = sugs.iter().map(|s| Row::new(s.clone()).icon(ui::icon::Kind::Search).raw()).collect();
            self.sug.draw(p, f.measure, &rows, self.sug.focused(cx, sugs.len()));
            self.sug.stops(f, sugs.len());
        } else if !naming {
            ui::text(p, "Suggestions appear here as you type.", 1000.0, 310.0, 28, ui::FAINT, ui::ALIGN_LEFT, false);
        }
        if self.query.is_empty() && !self.recent.is_empty() {
            ui::text(p, "Recent searches", 1000.0, 250.0, 26, ui::FAINT, ui::ALIGN_LEFT, false);
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accented_layout_keeps_the_grid() {
        assert_eq!(ACCENTED.chars().count(), LETTERS.chars().count());
        assert_eq!(keys_for(true, true).len(), keys_for(true, false).len());
        assert_eq!(keys_for(false, true).len(), keys_for(false, false).len());
        let caps: String = "áéíóöőúüűß".chars().map(capital).collect();
        assert_eq!(caps, "ÁÉÍÓÖŐÚÜŰß");
        for c in "áéíóöőúüűäßñçèêà".chars() {
            assert!(ACCENTED.contains(c), "{c} missing");
        }
    }
}

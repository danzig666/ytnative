//! Search: an on-screen keyboard, the query line and live suggestions. OK on a suggestion or on
//! the Search key opens the results page.
//!
//! The same keyboard types names ([`SearchScreen::name`]): a playlist on this TV is named with
//! it; the Save key keeps the name and goes back. It also types the words looked for among the
//! settings ([`NamePurpose::SettingsSearch`]): its Search key leaves them for the settings screen
//! and goes back.

use crate::host::{LocalOp, NamePurpose, YtArg, YtFx, YtHost, YtMsg};
use crate::store::FeedKey;
use crate::ui::{self, Row, VList};
use plx_machine::machine::{Canon, Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, LogicalState, Machine, NavOp, ScreenEvent, TextEdit};
use plx_ui::frame::Budget;
use plx_ui::screen::{
    Activate, AxisMask, At, Hover, Stop, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Placed, RenderStrategy, Screen, Seat, Step,
};
use plx_ui::Rect;
use std::borrow::Cow;

const G_KEYS: GroupId = GroupId(1);
const G_SUG: GroupId = GroupId(2);
const KEY_BASE: u32 = 0x1000_0000;
const SUG_BASE: u32 = 0x2000_0000;

const COLS: usize = 8;
const KEY_W: f32 = 92.0;

/// Columns of keys: three wide keys when the TV's keyboard types (`crate::textin::exclusive`).
fn cols() -> usize {
    if crate::textin::exclusive() { 1 } else { COLS }
}

/// A key's width (wide ones beside the TV's keyboard).
fn key_w() -> f32 {
    if crate::textin::exclusive() { FIELD.w } else { KEY_W }
}

/// The search field.
const FIELD: Rect = Rect::new(100.0, 150.0, 1720.0, 100.0);
const KEY_H: f32 = 84.0;
/// The space between two keys.
const KEY_GAP: f32 = 12.0;
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
    /// The next set of letters: plain, the interface language's own alphabet, accented.
    Accents,
    /// The search field itself, when the TV's keyboard types: focused or pressed, it opens that
    /// keyboard.
    Field,
    Go,
}

/// The plain letters.
const LETTERS: &str = "abcdefghijklmnopqrstuvwxyz";
/// The accented letters (Hungarian, German, Spanish, French and a few more), in the plain
/// letters' places.
const ACCENTED: &str = "áéíóöőúüűäßñçèêàâîôûëïœæãõ";

/// The keyboard's sets of letters: the plain ones, the interface language's own alphabet (Cyrillic,
/// Greek, Hebrew, Arabic, Armenian, Georgian), the accented Latin ones.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Layout {
    Latin,
    Native,
    Accented,
}

/// The alphabet of interface language `code` when it is not written in Latin letters and a key
/// per letter fits the keyboard. (Indic, Thai, Chinese, Japanese and Korean need an input method;
/// voice search types them.)
fn native_alphabet(code: &str) -> Option<&'static str> {
    Some(match code {
        "ru" => "абвгдеёжзийклмнопрстуфхцчшщъыьэюя",
        "uk" => "абвгґдеєжзиіїйклмнопрстуфхцчшщьюя",
        "bg" => "абвгдежзийклмнопрстуфхцчшщъьюя",
        "sr" => "абвгдђежзијклљмнњопрстћуфхцчџш",
        "el" => "αβγδεζηθικλμνξοπρστυφχψωάέήίόύώ",
        "he" => "אבגדהוזחטיכךלמםנןסעפףצץקרשת",
        "ar" => "ابتثجحخدذرزسشصضطظعغفقكلمنهويءآأؤإئةى",
        "fa" => "ابپتثجچحخدذرزژسشصضطظعغفقکگلمنوهیآ",
        "hy" => "աբգդեզէըթժիլխծկհձղճմյնշոչպջռսվտրցւփքօֆ",
        "ka" => "აბგდევზთიკლმნოპჟრსტუფქღყშჩცძწჭხჯჰ",
        _ => return None,
    })
}

/// The layout after `l` (the interface language's own alphabet only when it has one).
fn next_layout(l: Layout, native: bool) -> Layout {
    match l {
        Layout::Latin if native => Layout::Native,
        Layout::Latin | Layout::Native => Layout::Accented,
        Layout::Accented => Layout::Latin,
    }
}

fn letters(l: Layout) -> &'static str {
    match l {
        Layout::Latin => LETTERS,
        Layout::Accented => ACCENTED,
        Layout::Native => native_alphabet(crate::i18n::current()).unwrap_or(LETTERS),
    }
}

/// The keys: the letters (of the layout), digits, marks and the controls. Names also get a
/// Shift key for capital letters.
fn keys_for(naming: bool, layout: Layout) -> Vec<K> {
    // The TV's keyboard types: the field is the one thing to focus (its Enter searches / saves).
    if crate::textin::exclusive() {
        return vec![K::Field];
    }
    let mut v: Vec<K> = letters(layout).chars().chain("1234567890".chars()).map(K::Ch).collect();
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

fn key_label(k: K, naming: bool, upper: bool, layout: Layout) -> String {
    match k {
        K::Ch(c) if upper => capital(c),
        K::Ch(c) => c.to_string(),
        K::Space => "Space".into(),
        K::Back => "\u{232B}".into(),
        K::Clear => "Clear".into(),
        // Shows what the letters become.
        K::Shift if upper => "abc".into(),
        K::Shift => "ABC".into(),
        // Shows what the letters become.
        K::Accents => match next_layout(layout, native_alphabet(crate::i18n::current()).is_some()) {
            Layout::Latin => "a\u{2013}z".into(),
            Layout::Accented => "\u{E1}\u{E9}".into(),
            Layout::Native => letters(Layout::Native).chars().take(3).collect(),
        },
        K::Field => String::new(),
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
    /// The set of letters shown.
    layout: Layout,
    /// The key the focus goes to on the next frame (the layout key, after the keys changed).
    focus_req: std::cell::Cell<Option<u32>>,
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
            layout: Layout::Latin,
            focus_req: std::cell::Cell::new(None),
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
            NamePurpose::SettingsSearch(q) => q.clone(),
            NamePurpose::RenameSection(_, n) => n.clone(),
        };
        // A new name starts with a capital letter (the settings are searched in any case).
        s.upper = s.query.is_empty() && !matches!(purpose, NamePurpose::SettingsSearch(_));
        s.state = State(s.query.clone());
        s.naming = Some(purpose);
        s
    }
    /// The keyboard types a name (not words to look for).
    fn names(&self) -> bool {
        self.naming.as_ref().is_some_and(|p| !matches!(p, NamePurpose::SettingsSearch(_)))
    }
    fn keys(&self) -> Vec<K> {
        keys_for(self.names(), self.layout)
    }
    fn key_rect(i: usize) -> Rect {
        if crate::textin::exclusive() {
            return FIELD;
        }
        Rect::new(KEYS_X + (i % cols()) as f32 * (key_w() + KEY_GAP), KEYS_Y + (i / cols()) as f32 * (KEY_H + KEY_GAP), key_w(), KEY_H)
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
        // The TV's keyboard goes with the search (a suggestion picked as well as its Enter).
        crate::textin::hide();
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
        crate::textin::hide();
        let Some(purpose) = &self.naming else { return };
        if matches!(purpose, NamePurpose::SettingsSearch(_)) {
            super::settings_search::submit(&self.query);
            fx.push(Fx::Nav(NavOp::Pop));
            return;
        }
        let title = self.query.trim().to_string();
        if title.is_empty() {
            fx.push(Fx::App(YtFx::Toast("Type a name first".into())));
            return;
        }
        let op = match purpose {
            NamePurpose::NewLocal(videos) => LocalOp::Create { title, videos: videos.clone() },
            NamePurpose::RenameLocal(id, _) => LocalOp::Rename { list_id: id.clone(), title },
            NamePurpose::SettingsSearch(_) => return,
            NamePurpose::RenameSection(key, _) => {
                let key = key.clone();
                fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| {
                    s.section_names.retain(|(k, _)| *k != key);
                    s.section_names.push((key, title));
                }))));
                fx.push(Fx::Nav(NavOp::Pop));
                return;
            }
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
            K::Field => crate::textin::show(),
            K::Accents => {
                self.layout = next_layout(self.layout, native_alphabet(crate::i18n::current()).is_some());
                // The letters' count changed: the focus stays on this key in its new place.
                if let Some(i) = self.keys().iter().position(|k| *k == K::Accents) {
                    self.focus_req.set(Some(KEY_BASE + i as u32));
                }
            }
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
            // The TV's keyboard: its text, Backspace and Enter, as if typed on the keys here.
            ScreenEvent::Input(InputEvent { kind: InputKind::Text(edit), .. }) => {
                let record = cx.views.store.settings.search_history;
                match edit {
                    TextEdit::Commit(t) if &**t == "\n" => {
                        crate::textin::hide();
                        self.press(K::Go, record, fx);
                    }
                    TextEdit::Commit(t) => {
                        for c in t.chars().filter(|c| !c.is_control()) {
                            self.press(if c == ' ' { K::Space } else { K::Ch(c) }, record, fx);
                        }
                    }
                    TextEdit::Backspace => self.press(K::Back, record, fx),
                    TextEdit::Clear => self.press(K::Clear, record, fx),
                    _ => {}
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } if crate::textin::exclusive() && to.entry == self.entry && to.elem == KEY_BASE => {
                crate::textin::show();
                fx.invalidate(plx_machine::present::Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Mount if crate::textin::exclusive() => {
                crate::textin::show();
                Handled::Yes
            }
            // Left, or covered by the results: the TV's keyboard goes.
            ScreenEvent::Unmount | ScreenEvent::Cover => {
                crate::textin::hide();
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
            kind: GroupKind::Grid { cols: cols(), holes: &[] },
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent: Rect::new(KEYS_X, KEYS_Y, cols() as f32 * (key_w() + KEY_GAP), (n.div_ceil(cols())) as f32 * (KEY_H + KEY_GAP)),
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
                return Step::Move(self.kkey((cols() - 1).min(self.keys().len() - 1)));
            }
            // Up from the first suggestion: the field.
            if crate::textin::exclusive() && dir == Dir::Up && self.sug.index(key.elem, sugs) == Some(0) {
                return Step::Move(self.kkey(0));
            }
            return self.sug.neighbour(key, dir, sugs);
        }
        if crate::textin::exclusive() {
            return if matches!(dir, Dir::Right | Dir::Down) && sugs > 0 { Step::Move(self.sug.key(0)) } else { Step::Edge };
        }
        let n = self.keys().len();
        let i = (key.elem - KEY_BASE) as usize;
        let (r, c) = (i / cols(), i % cols());
        let to = match dir {
            Dir::Left if c > 0 => Some(i - 1),
            Dir::Right if c + 1 < cols() && i + 1 < n => Some(i + 1),
            Dir::Right if sugs > 0 => return Step::Move(self.sug.key(0)),
            Dir::Up if r > 0 => Some(i - cols()),
            Dir::Down if i + cols() < n => Some(i + cols()),
            Dir::Down if r + 1 < n.div_ceil(cols()) => Some(n - 1),
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
        if let Some(e) = self.focus_req.take() {
            return FocusKey { entry: self.entry, elem: e };
        }
        // Fewer keys than before (another layout): the last one.
        let n = self.keys().len();
        if (KEY_BASE..SUG_BASE.min(KEY_BASE + 10_000)).contains(&want.elem) && (want.elem - KEY_BASE) as usize >= n {
            return self.kkey(n - 1);
        }
        if want.elem >= SUG_BASE && self.sug.index(want.elem, self.suggestions(cx).len()).is_none() {
            return self.kkey((cols() - 1).min(n - 1));
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
        // Each key answers the pointer over half the gap around it too, so the keys tile the
        // keyboard with no gaps. A click types the key at once, as the button goes down: the Magic
        // Remote's pointer jiggles as it is pressed, and a press held until the button came up was
        // cancelled whenever the pointer slipped onto another key meanwhile (the key never typed).
        for i in 0..self.keys().len() {
            let r = Self::key_rect(i);
            let half = KEY_GAP / 2.0;
            let hit = Rect::new(r.x - half, r.y - half, r.w + KEY_GAP, r.h + KEY_GAP);
            let p = f.painter;
            f.stop(p, Stop { key: self.kkey(i), rect: hit, rest_rect: hit, clip: Rect::new(0.0, 0.0, 1920.0, 1080.0), hover: Hover::Focus, activate: Activate::Direct });
        }
        let heading = match &self.naming {
            None => "Search",
            Some(NamePurpose::NewLocal(_)) => "Name of the new playlist on this TV",
            Some(NamePurpose::RenameLocal(..)) => "New name of the playlist",
            Some(NamePurpose::SettingsSearch(_)) => "Search settings",
            Some(NamePurpose::RenameSection(..)) => "New name of the section",
        };
        ui::text(p, heading, 100.0, 60.0, 46, ui::WHITE, ui::ALIGN_LEFT, true);
        // Query field
        let field = FIELD;
        let field_focus = crate::textin::exclusive() && cx.focus.current.is_some_and(|k| k.entry == self.entry && k.elem == KEY_BASE);
        if field_focus {
            ui::fill(p, Rect::new(field.x - 4.0, field.y - 4.0, field.w + 8.0, field.h + 8.0), 22.0, ui::WHITE);
            ui::fill(p, field, 18.0, [0.13, 0.13, 0.15, 1.0]);
        } else {
            ui::fill(p, field, 18.0, [1.0, 1.0, 1.0, 0.08]);
        }
        // The query as typed (its end, with the cursor, when it is longer than the field).
        let room = field.w - 72.0;
        if self.query.is_empty() {
            let hint = crate::i18n::tr(if crate::textin::exclusive() {
                "Press OK to type"
            } else if self.names() {
                "Type the name with the keyboard below\u{2026}"
            } else {
                "Type with the keyboard below\u{2026}"
            });
            ui::text_cut(p, f.measure, hint, field.x + 36.0, field.y + 26.0, room, 40, ui::FAINT, ui::ALIGN_LEFT, false);
        } else {
            // Longer than the field: its start fades out at the left edge.
            ui::text_cut_start(p, f.measure, &format!("{}|", self.query), field.x + 36.0, field.y + 26.0, room, 40, ui::WHITE, false);
        }
        // Keyboard
        let focused = cx.focus.current.filter(|k| k.entry == self.entry).map(|k| k.elem);
        let naming = self.names();
        for (i, k) in self.keys().into_iter().enumerate() {
            if k == K::Field {
                continue; // drawn above, as the field
            }
            let r = Self::key_rect(i);
            let is_focus = focused == Some(KEY_BASE + i as u32);
            let bg = if is_focus { ui::WHITE } else if k == K::Go { ui::accent() } else { [1.0, 1.0, 1.0, 0.10] };
            ui::fill(p, r, 14.0, bg);
            let mut size = if matches!(k, K::Ch(_)) { 36 } else { 22 };
            let ink = if is_focus { [0.0, 0.0, 0.0, 1.0] } else { ui::WHITE };
            // A long word ("Очистить", "Leerzeichen") smaller until it fits its key.
            let label = key_label(k, naming, self.upper, self.layout);
            let label = crate::i18n::tr(&label);
            while size > 14 && ui::text_width(f.measure, label, size, true) > r.w - 12.0 {
                size -= 1;
            }
            ui::text_raw(p, label, r.x + r.w / 2.0, r.y + (r.h - size as f32 * 1.25) / 2.0, size, ink, ui::ALIGN_CENTER, true);
        }
        // Suggestions
        let sugs = self.suggestions(cx);
        if !sugs.is_empty() {
            let rows: Vec<Row> = sugs.iter().map(|s| Row::new(s.clone()).icon(ui::icon::Kind::Search).raw()).collect();
            self.sug.draw(p, f.measure, &rows, self.sug.focused(cx, sugs.len()));
            self.sug.stops(f, sugs.len());
        } else if self.naming.is_none() {
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
        assert_eq!(keys_for(true, Layout::Accented).len(), keys_for(true, Layout::Latin).len());
        assert_eq!(keys_for(false, Layout::Accented).len(), keys_for(false, Layout::Latin).len());
        let caps: String = "áéíóöőúüűß".chars().map(capital).collect();
        assert_eq!(caps, "ÁÉÍÓÖŐÚÜŰß");
        for c in "áéíóöőúüűäßñçèêà".chars() {
            assert!(ACCENTED.contains(c), "{c} missing");
        }
    }

    #[test]
    fn native_alphabets_fit_seven_rows() {
        // The most keys (a name: Shift too) still end on the screen.
        let others = keys_for(true, Layout::Latin).len() - LETTERS.chars().count();
        for code in ["ru", "uk", "bg", "sr", "el", "he", "ar", "fa", "hy", "ka"] {
            let n = native_alphabet(code).unwrap().chars().count() + others;
            let bottom = SearchScreen::key_rect(n - 1);
            assert!(bottom.y + bottom.h <= 1070.0, "{code}: {n} keys end at {}", bottom.y + bottom.h);
        }
        assert_eq!(native_alphabet("ru").unwrap().chars().count(), 33);
        assert_eq!(next_layout(Layout::Latin, true), Layout::Native);
        assert_eq!(next_layout(Layout::Latin, false), Layout::Accented);
        assert_eq!(next_layout(Layout::Native, true), Layout::Accented);
        assert_eq!(next_layout(Layout::Accented, true), Layout::Latin);
        assert_eq!(capital('ж'), "Ж");
    }
}

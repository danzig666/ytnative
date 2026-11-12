//! PIN entry: on-screen keypad (and the remote's number keys). Used to set / check the settings
//! PIN (child mode, locked settings) and per-account passwords.

use crate::host::{PinPurpose, SettingsPage, UserAction, YtArg, YtFx, YtHost};
use crate::ui;
use plx_machine::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, LogicalState, Machine, NavOp, ScreenEvent,
};
use plx_ui::frame::Budget;
use plx_ui::screen::{
    AxisMask, At, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Placed, RenderStrategy, Screen, Seat, Step,
};
use plx_ui::Rect;
use std::borrow::Cow;

const G: GroupId = GroupId(1);
const BASE: u32 = 0x7a00_0000;
const KEYS: [&str; 12] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "\u{232B}", "0", "OK"];
const KX: f32 = 760.0;
const KY: f32 = 420.0;
const KW: f32 = 120.0;
const KH: f32 = 100.0;

struct State;
impl LogicalState for State {
    fn write(&self, c: &mut Canon) {
        c.u32(15);
    }
    fn probe(&self, out: &mut String) {
        out.push_str("pin");
    }
}

pub struct PinScreen {
    entry: EntryId,
    purpose: PinPurpose,
    digits: String,
    /// When setting a new PIN: the first entry, awaiting confirmation.
    first: Option<String>,
    error: Option<&'static str>,
}

impl PinScreen {
    pub fn new(entry: EntryId, purpose: PinPurpose) -> Self {
        PinScreen { entry, purpose, digits: String::new(), first: None, error: None }
    }
    fn key_rect(i: usize) -> Rect {
        Rect::new(KX + (i % 3) as f32 * (KW + 16.0), KY + (i / 3) as f32 * (KH + 16.0), KW, KH)
    }
    fn k(&self, i: usize) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: BASE + i as u32 }
    }

    fn submit(&mut self, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        let pin = std::mem::take(&mut self.digits);
        if pin.len() < 4 {
            self.error = Some("Use at least 4 digits");
            return;
        }
        match &self.purpose {
            PinPurpose::Set => match self.first.take() {
                None => self.first = Some(pin),
                Some(f) if f == pin => {
                    let hash = yt_core::accounts::hash_password(&pin);
                    fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| s.settings_pin = hash))));
                    fx.push(Fx::App(YtFx::Toast("PIN set".into())));
                    fx.push(Fx::Nav(NavOp::Pop));
                }
                Some(_) => self.error = Some("The PINs didn't match, try again"),
            },
            PinPurpose::OpenSettings => {
                if yt_core::accounts::verify_password(&cx.views.store.settings.settings_pin, &pin) {
                    fx.push(Fx::Nav(NavOp::Replace(YtArg::Settings(SettingsPage::Root))));
                } else {
                    self.error = Some("Wrong PIN");
                }
            }
            PinPurpose::SwitchAccount(id, stored) => {
                if yt_core::accounts::verify_password(stored, &pin) {
                    fx.push(Fx::App(YtFx::Act(UserAction::SwitchAccount(id.clone()))));
                    fx.push(Fx::Nav(NavOp::Pop));
                } else {
                    self.error = Some("Wrong password");
                }
            }
        }
    }

    fn press(&mut self, key: &str, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        self.error = None;
        match key {
            "\u{232B}" => {
                self.digits.pop();
            }
            "OK" => self.submit(cx, fx),
            d if self.digits.len() < 8 => self.digits.push_str(d),
            _ => {}
        }
        fx.invalidate(plx_machine::present::Provenance::Input);
    }
}

impl Machine<YtHost> for PinScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        if let Some(a) = super::common::press_commit_as_activate(ev, cx, |_| true) {
            return self.step(&a, cx, fx);
        }
        match ev {
            ScreenEvent::Activate(e) => {
                if let Some(k) = e.checked_sub(BASE).and_then(|i| KEYS.get(i as usize)) {
                    self.press(k, cx, fx);
                }
                Handled::Yes
            }
            // The remote's number keys.
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { sym, edge: Edge::Down, .. }, .. }) if (48..=57).contains(sym) => {
                let d = ((*sym - 48) as u8 + b'0') as char;
                self.press(&d.to_string(), cx, fx);
                Handled::Yes
            }
            ScreenEvent::Input(_) => Handled::No,
            _ => Handled::Yes,
        }
    }
}

impl Focusable<YtHost> for PinScreen {
    fn groups(&self, _cx: &Cx<'_, YtHost>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: G,
            kind: GroupKind::Grid { cols: 3, holes: &[] },
            seat: Seat::First,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4],
            extent: Rect::new(KX, KY, 3.0 * (KW + 16.0), 4.0 * (KH + 16.0)),
            len: KEYS.len(),
            elem: ElemKind::Control,
        });
    }
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        Some(G)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, YtHost>) -> Step<u32> {
        let i = (key.elem - BASE) as usize;
        let (r, c) = (i / 3, i % 3);
        let to = match dir {
            Dir::Left if c > 0 => i - 1,
            Dir::Right if c < 2 => i + 1,
            Dir::Up if r > 0 => i - 3,
            Dir::Down if r < 3 => i + 3,
            _ => return Step::Edge,
        };
        Step::Move(self.k(to))
    }
    fn place(&self, key: &u32, _cx: &Cx<'_, YtHost>, _at: At) -> Option<Placed> {
        let i = key.checked_sub(BASE)? as usize;
        (i < KEYS.len()).then(|| {
            let r = Self::key_rect(i);
            Placed { rect: r, rest_rect: r, clip: Rect::new(0.0, 0.0, 1920.0, 1080.0), index: Some(i as u32) }
        })
    }
    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        if want.elem >= BASE && want.elem < BASE + KEYS.len() as u32 { want } else { self.k(0) }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        self.k(0)
    }
}

impl Screen<YtHost> for PinScreen {
    fn name(&self) -> &'static str {
        "pin"
    }
    fn state(&self) -> &dyn LogicalState {
        &State
    }
    fn crumb(&self, _cx: &Cx<'_, YtHost>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, YtHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, YtHost>) {
        let p = f.painter;
        let cx = f.cx;
        let title = match (&self.purpose, &self.first) {
            (PinPurpose::Set, None) => "Choose a PIN",
            (PinPurpose::Set, Some(_)) => "Enter the PIN again",
            (PinPurpose::OpenSettings, _) => "Enter the PIN to open Settings",
            (PinPurpose::SwitchAccount(..), _) => "Enter this account's password",
        };
        ui::text(p, title, 960.0, 150.0, 46, ui::WHITE, ui::ALIGN_CENTER, true);
        let dots: String = self.digits.chars().map(|_| "\u{25CF} ").collect();
        ui::fill(p, Rect::new(760.0, 250.0, 400.0, 100.0), 18.0, [1.0, 1.0, 1.0, 0.08]);
        ui::text(p, if dots.is_empty() { "\u{2014}" } else { &dots }, 960.0, 278.0, 40, ui::WHITE, ui::ALIGN_CENTER, false);
        if let Some(e) = self.error {
            ui::text(p, e, 960.0, 370.0, 26, ui::accent(), ui::ALIGN_CENTER, false);
        }
        let focus = cx.focus.current.filter(|k| k.entry == self.entry).map(|k| k.elem);
        for (i, k) in KEYS.iter().enumerate() {
            let r = Self::key_rect(i);
            let on = focus == Some(BASE + i as u32);
            ui::fill(p, r, 16.0, if on { ui::WHITE } else { [1.0, 1.0, 1.0, 0.10] });
            ui::text(p, k, r.x + r.w / 2.0, r.y + 28.0, 38, if on { [0.0, 0.0, 0.0, 1.0] } else { ui::WHITE }, ui::ALIGN_CENTER, true);
        }
        ui::text(p, "The remote's number keys work too.", 960.0, 900.0, 24, ui::FAINT, ui::ALIGN_CENTER, false);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

//! Sign in with a code: shows the code to enter at the verification URL plus a QR code for the
//! phone, polls in the background, and closes itself when the account is linked.

use crate::host::{SignIn, YtFx, YtHost, YtMsg};
use crate::ui;
use plx_machine::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, Key, LogicalState, Machine, NavOp, ScreenEvent,
};
use plx_ui::frame::Budget;
use plx_ui::qr::QrCode;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use std::borrow::Cow;

const QR_FRAME: Rect = Rect::new(1180.0, 300.0, 460.0, 460.0);

struct State(String);
impl LogicalState for State {
    fn write(&self, c: &mut Canon) {
        c.str(&self.0);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&self.0);
    }
}

pub struct SignInScreen {
    entry: EntryId,
    qr: Option<(String, QrCode)>,
    started: bool,
    /// The sign-in this screen started finished and the screen asked to close (once).
    left: bool,
    state: State,
}

impl SignInScreen {
    pub fn new(entry: EntryId) -> Self {
        SignInScreen { entry, qr: None, started: false, left: false, state: State("signin".into()) }
    }

    /// The sign-in this screen started has finished: say so and go back to where it was opened
    /// from, without waiting for a key.
    fn leave_when_done(&mut self, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) {
        if self.left || !self.started {
            return;
        }
        if let SignIn::Done(name) = cx.views.signin {
            self.left = true;
            fx.push(Fx::App(YtFx::Toast(crate::i18n::tr("Signed in as {}").replace("{}", name))));
            fx.push(Fx::Nav(NavOp::Pop));
        }
    }
}

impl Machine<YtHost> for SignInScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                if !cx.views.store.signed_in {
                    self.started = true;
                    fx.push(Fx::App(YtFx::SignInStart));
                }
                Handled::Yes
            }
            ScreenEvent::Unmount => {
                fx.push(Fx::App(YtFx::SignInCancel));
                Handled::Yes
            }
            ScreenEvent::Async(_, YtMsg::SignIn) => {
                self.leave_when_done(cx, fx);
                fx.invalidate(plx_machine::present::Provenance::Lifecycle);
                Handled::Yes
            }
            // The worker's "done" reply is not always delivered here: the frame it wakes checks too.
            ScreenEvent::Tick(_) => {
                self.leave_when_done(cx, fx);
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Ok, edge: Edge::Down, .. }, .. }) => {
                // Already signed in: OK links another account; after a failure, OK retries.
                if !self.started || matches!(cx.views.signin, SignIn::Failed(_)) {
                    self.started = true;
                    fx.push(Fx::App(YtFx::SignInStart));
                    fx.invalidate(plx_machine::present::Provenance::Input);
                }
                Handled::Yes
            }
            ScreenEvent::Input(_) => Handled::No,
            _ => Handled::Yes,
        }
    }
}

impl Focusable<YtHost> for SignInScreen {
    fn groups(&self, _cx: &Cx<'_, YtHost>, _out: &mut Vec<GroupSpec>) {}
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        None
    }
    fn neighbour(&self, _key: FocusKey<u32>, _dir: Dir, _cx: &Cx<'_, YtHost>) -> Step<u32> {
        Step::Edge
    }
    fn place(&self, _key: &u32, _cx: &Cx<'_, YtHost>, _at: At) -> Option<Placed> {
        None
    }
    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        want
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: 0 }
    }
}

impl Screen<YtHost> for SignInScreen {
    fn name(&self) -> &'static str {
        "signin"
    }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, YtHost>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, cx: &Cx<'_, YtHost>) {
        if let SignIn::Code { user_code, url } = cx.views.signin {
            let link = format!("{url}?user_code={}", user_code.replace(' ', ""));
            if self.qr.as_ref().map(|q| q.0 != link).unwrap_or(true) {
                self.qr = QrCode::new(&link).ok().map(|mut q| {
                    q.prepare(QR_FRAME);
                    (link, q)
                });
            }
        }
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, YtHost>) {
        let p = f.painter;
        let cx = f.cx;
        super::common::logo(p, 120.0, 90.0);
        ui::text(p, "Sign in to YouTube", 120.0, 200.0, 56, ui::WHITE, ui::ALIGN_LEFT, true);
        // Opened while signed in, before OK started a sign-in of its own: whatever state an
        // earlier sign-in left behind ("Done", "Failed") is not this screen's.
        if !self.started && cx.views.store.signed_in {
            let who = cx.views.store.account.clone().filter(|a| !a.trim().is_empty());
            let line = match who {
                Some(who) => crate::i18n::tr("You are signed in as {}.").replace("{}", &who),
                None => crate::i18n::tr("You are signed in.").to_string(),
            };
            ui::text_raw(p, &line, 120.0, 330.0, 36, ui::DIM, ui::ALIGN_LEFT, false);
            ui::text(p, "Press OK to link another account, BACK to return.", 120.0, 400.0, 30, ui::FAINT, ui::ALIGN_LEFT, false);
            return;
        }
        match cx.views.signin {
            SignIn::Idle | SignIn::Starting => {
                ui::text(p, "Getting a code\u{2026}", 120.0, 330.0, 36, ui::DIM, ui::ALIGN_LEFT, false);
            }
            SignIn::Code { user_code, url } => {
                ui::text(p, "1.  On your phone or computer, go to", 120.0, 320.0, 34, ui::DIM, ui::ALIGN_LEFT, false);
                ui::text_raw(p, &url.replace("https://", ""), 180.0, 375.0, 44, ui::WHITE, ui::ALIGN_LEFT, true);
                ui::text(p, "2.  Enter this code", 120.0, 470.0, 34, ui::DIM, ui::ALIGN_LEFT, false);
                let box_ = Rect::new(170.0, 530.0, 760.0, 150.0);
                ui::fill(p, box_, 22.0, [1.0, 1.0, 1.0, 0.10]);
                ui::text_raw(p, user_code, box_.x + box_.w / 2.0, box_.y + 34.0, 72, ui::WHITE, ui::ALIGN_CENTER, true);
                ui::text(p, "Or scan the code with your phone \u{2192}", 120.0, 740.0, 30, ui::FAINT, ui::ALIGN_LEFT, false);
                ui::text(p, "Waiting for you to approve\u{2026}", 120.0, 800.0, 30, ui::FAINT, ui::ALIGN_LEFT, false);
                if let Some((_, qr)) = &self.qr {
                    ui::fill(p, Rect::new(QR_FRAME.x - 20.0, QR_FRAME.y - 20.0, QR_FRAME.w + 40.0, QR_FRAME.h + 40.0), 20.0, ui::WHITE);
                    qr.draw(p, QR_FRAME);
                }
            }
            SignIn::Done(name) => {
                ui::text_raw(p, &crate::i18n::tr("Signed in as {}.").replace("{}", name), 120.0, 330.0, 40, ui::WHITE, ui::ALIGN_LEFT, false);
            }
            SignIn::Failed(e) => {
                ui::text(p, "Sign-in didn't work.", 120.0, 330.0, 40, ui::WHITE, ui::ALIGN_LEFT, false);
                ui::text_raw(p, &f.measure.fit_line(e, 1680.0, 28, false).to_string_lossy(), 120.0, 395.0, 28, ui::FAINT, ui::ALIGN_LEFT, false);
                ui::text(p, "Press OK to try again.", 120.0, 460.0, 30, ui::DIM, ui::ALIGN_LEFT, false);
            }
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

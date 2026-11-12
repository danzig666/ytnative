//! Accounts: switch between signed-in accounts, add one, or remove one.

use crate::host::{UserAction, YtArg, YtFx, YtHost, YtMsg};
use crate::ui::{self, Row, VList};
use plx_machine::machine::{Canon, Cx, Effects, EntryId, FocusKey, Fx, GroupId, Handled, LogicalState, Machine, NavOp, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use std::borrow::Cow;

const G: GroupId = GroupId(1);
const BASE: u32 = 0x6000_0000;

#[derive(Clone)]
enum RowAct {
    Switch(String),
    Add,
    Remove(String, String),
}

fn rows_of(cx: &Cx<'_, YtHost>) -> Vec<(Row, RowAct)> {
    let mut v: Vec<(Row, RowAct)> = cx
        .views
        .store
        .accounts
        .iter()
        .map(|a| (Row::new(a.name.clone()).value(if a.current { "current" } else { "switch" }).active(a.current), RowAct::Switch(a.id.clone())))
        .collect();
    v.push((Row::new("+  Add an account"), RowAct::Add));
    for a in &cx.views.store.accounts {
        v.push((Row::new(format!("Remove {}", a.name)), RowAct::Remove(a.id.clone(), a.name.clone())));
    }
    v
}

struct State;
impl LogicalState for State {
    fn write(&self, c: &mut Canon) {
        c.u32(9);
    }
    fn probe(&self, out: &mut String) {
        out.push_str("accounts");
    }
}

pub struct AccountsScreen {
    list: VList,
    confirm_remove: Option<String>,
}

impl AccountsScreen {
    pub fn new(entry: EntryId) -> Self {
        AccountsScreen {
            list: VList { entry, group: G, base: BASE, rect: Rect::new(360.0, 200.0, 1200.0, 800.0), row_h: 80.0, text_size: 32 },
            confirm_remove: None,
        }
    }
}

impl Machine<YtHost> for AccountsScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        let rows = rows_of(cx);
        match ev {
            ScreenEvent::Activate(e) => {
                if let Some(i) = self.list.index(*e, rows.len()) {
                    match rows[i].1.clone() {
                        RowAct::Switch(id) => fx.push(Fx::App(YtFx::Act(UserAction::SwitchAccount(id)))),
                        RowAct::Add => fx.push(Fx::Nav(NavOp::Push(YtArg::SignIn))),
                        RowAct::Remove(id, name) => {
                            // Press twice to confirm.
                            if self.confirm_remove.as_deref() == Some(id.as_str()) {
                                self.confirm_remove = None;
                                fx.push(Fx::App(YtFx::Act(UserAction::RemoveAccount(id))));
                            } else {
                                self.confirm_remove = Some(id);
                                fx.push(Fx::App(YtFx::Toast(format!("Press OK again to remove {name}"))));
                            }
                        }
                    }
                }
                Handled::Yes
            }
            ScreenEvent::Async(_, YtMsg::Acted) | ScreenEvent::Uncover => {
                fx.invalidate(plx_machine::present::Provenance::Lifecycle);
                Handled::Yes
            }
            ScreenEvent::Input(_) => Handled::No,
            _ => Handled::Yes,
        }
    }
}

impl Focusable<YtHost> for AccountsScreen {
    fn groups(&self, cx: &Cx<'_, YtHost>, out: &mut Vec<GroupSpec>) {
        out.push(self.list.spec(rows_of(cx).len()));
    }
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        Some(G)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, YtHost>) -> Step<u32> {
        self.list.neighbour(key, dir, rows_of(cx).len())
    }
    fn place(&self, key: &u32, cx: &Cx<'_, YtHost>, _at: At) -> Option<Placed> {
        let n = rows_of(cx).len();
        self.list.place(*key, n, self.list.focused(cx, n))
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        if self.list.index(want.elem, rows_of(cx).len()).is_some() { want } else { self.list.key(0) }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        self.list.key(0)
    }
}

impl Screen<YtHost> for AccountsScreen {
    fn name(&self) -> &'static str {
        "accounts"
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
        ui::text(p, "Accounts", 360.0, 80.0, 48, ui::WHITE, ui::ALIGN_LEFT, true);
        let rows: Vec<Row> = rows_of(cx).into_iter().map(|r| r.0).collect();
        self.list.draw(p, &rows, self.list.focused(cx, rows.len()));
        super::common::toast(p, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

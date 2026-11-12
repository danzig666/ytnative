//! The app's `machine::Host`: argument, effect, message and view types, plus the `Rig` the
//! dispatcher drives (screens mounting, worker effects, per-frame preparation).

use crate::screens;
use plx_machine::machine::{
    Addr, Canon, Chrome, Cx, Effects, EntryId, Handled, Host, InputOwner, InstanceId, LogicalState, MachineId, RequestId,
    ScreenId, TimerId,
};
use plx_machine::present::Present;
use plx_ui::dispatch::{CxParts, Rig, Split};
use plx_ui::frame::Budget;
use plx_ui::screen::{Mounter, ReturnState, Screen, ScreenArg};
use std::sync::{Arc, Mutex};

pub struct YtHost;

/// What a navigation entry shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum YtArg {
    Home,
    Video(u32),
}

impl ScreenArg for YtArg {
    fn chrome(&self) -> Chrome {
        Chrome::None
    }
    fn id(&self) -> ScreenId {
        ScreenId(match self {
            YtArg::Home => 1,
            YtArg::Video(_) => 2,
        })
    }
    fn title(&self) -> Option<&str> {
        None
    }
    fn same_instance(&self, other: &Self) -> bool {
        self == other
    }
}

impl LogicalState for YtArg {
    fn write(&self, c: &mut Canon) {
        match self {
            YtArg::Home => {
                c.u32(0);
            }
            YtArg::Video(i) => {
                c.u32(1).u32(*i);
            }
        }
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("{self:?}"));
    }
}

pub enum YtFx {}
pub enum YtMsg {}

/// One card's data as the screens read it.
#[derive(Clone, Debug, Default)]
pub struct Card {
    pub elem: u32,
    pub title: String,
    pub detail: String,
    pub thumb: String,
}

#[derive(Clone, Copy)]
pub struct Views<'a> {
    pub home: &'a [Card],
}

#[derive(Default)]
pub struct Init;
impl LogicalState for Init {
    fn write(&self, _: &mut Canon) {}
    fn probe(&self, _: &mut String) {}
}

impl Host for YtHost {
    type Arg = YtArg;
    type Fx = YtFx;
    type Msg = YtMsg;
    type Elem = u32;
    type Views<'a> = Views<'a>;
    type Init = Init;
    type Memory = ();
}

struct Mount;

impl Mounter<YtHost> for Mount {
    fn mount(
        &mut self,
        _id: InstanceId,
        arg: &YtArg,
        _ret: &ReturnState<u32, ()>,
        cx: &Cx<'_, YtHost>,
        _fx: &mut Effects<'_, YtHost>,
    ) -> Box<dyn Screen<YtHost>> {
        let entry = match cx.owner {
            InputOwner::Entry(e) => e,
            _ => EntryId(0),
        };
        match arg {
            YtArg::Home => Box::new(screens::home::HomeScreen::new(entry)),
            YtArg::Video(i) => Box::new(screens::home::HomeScreen::detail(entry, *i)),
        }
    }
}

pub struct AppRig {
    mounter: Mount,
    pub home: Vec<Card>,
    measure: plx_gfx::text::TtfMeasure,
    pub inbox: Arc<Mutex<Vec<(Addr, YtMsg)>>>,
    next_req: u32,
}

impl AppRig {
    pub fn new(home: Vec<Card>) -> Self {
        AppRig { mounter: Mount, home, measure: plx_gfx::text::TtfMeasure, inbox: Default::default(), next_req: 0 }
    }
    #[allow(dead_code)]
    fn next_addr(&mut self, to: MachineId) -> Addr {
        self.next_req += 1;
        Addr { to, req: RequestId(self.next_req) }
    }
}

impl Rig<YtHost> for AppRig {
    fn split(&mut self) -> Split<'_, YtHost> {
        Split { mounter: &mut self.mounter, views: Views { home: &self.home }, measure: &self.measure }
    }
    fn deliver(&mut self, _to: MachineId, _msg: &YtMsg, _p: &CxParts<u32>, _fx: &mut Effects<'_, YtHost>) -> Handled {
        Handled::No
    }
    fn timer(&mut self, _o: MachineId, _id: TimerId, _p: &CxParts<u32>, _fx: &mut Effects<'_, YtHost>) {}
    fn app_fx(&mut self, _from: MachineId, fx: YtFx, _p: &CxParts<u32>, _out: &mut Effects<'_, YtHost>) {
        match fx {}
    }
    fn log(&mut self, line: &str) {
        plx_base::eventlog::log(line)
    }
    fn prepare(&mut self, _b: &mut Budget, _present: &mut Present) {}
    fn ls2_pump(&mut self) {
        crate::wl::ls2_pump()
    }
    fn opaque_route(&mut self, _video: bool) {}
    fn clear_opaque_region(&mut self) {
        crate::wl::clear_opaque_region()
    }
    fn now_us(&self) -> u64 {
        crate::now_us()
    }
    fn back_at_root(&mut self) {
        plx_platform::tv::home::go_home()
    }
}

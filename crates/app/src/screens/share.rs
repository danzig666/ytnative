//! "Share" (SmartTube's QR link): a video's, channel's or playlist's YouTube address as a QR code,
//! so a phone opens it in YouTube. BACK closes it.

use crate::host::YtHost;
use crate::ui;
use plx_machine::machine::{Canon, Cx, Effects, EntryId, FocusKey, GroupId, Handled, LogicalState, Machine, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::qr::QrCode;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use std::borrow::Cow;

const QR_FRAME: Rect = Rect::new(735.0, 250.0, 450.0, 450.0);

/// The YouTube address of a card's video, channel or playlist.
pub fn link_of(item: &yt_core::model::Item, video_id: Option<&str>) -> Option<String> {
    use yt_core::model::Endpoint;
    if let Some(v) = video_id {
        return Some(format!("https://youtu.be/{v}"));
    }
    match &item.endpoint {
        Some(Endpoint::Browse { browse_id, .. }) if browse_id.starts_with("UC") => Some(format!("https://www.youtube.com/channel/{browse_id}")),
        Some(Endpoint::Browse { browse_id, .. }) if browse_id.starts_with("VL") && !yt_core::local_playlists::is_local_id(browse_id) => {
            Some(format!("https://www.youtube.com/playlist?list={}", &browse_id[2..]))
        }
        _ => None,
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

pub struct ShareScreen {
    entry: EntryId,
    url: String,
    title: String,
    qr: Option<QrCode>,
    state: State,
}

impl ShareScreen {
    pub fn new(entry: EntryId, url: String, title: String) -> Self {
        let qr = QrCode::new(&url).ok().map(|mut q| {
            q.prepare(QR_FRAME);
            q
        });
        ShareScreen { entry, state: State(url.clone()), url, title, qr }
    }
}

impl Machine<YtHost> for ShareScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, YtHost>, _fx: &mut Effects<'_, YtHost>) -> Handled {
        match ev {
            ScreenEvent::Input(_) => Handled::No,
            _ => Handled::Yes,
        }
    }
}

impl Focusable<YtHost> for ShareScreen {
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

impl Screen<YtHost> for ShareScreen {
    fn name(&self) -> &'static str {
        "share"
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
        let m = f.measure;
        ui::fill(p, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.0, 0.0, 0.0, 0.92]);
        ui::text(p, "Share", 960.0, 70.0, 48, ui::WHITE, ui::ALIGN_CENTER, true);
        ui::text_cut(p, m, &self.title, 960.0, 150.0, 1500.0, 30, ui::DIM, ui::ALIGN_CENTER, false);
        if let Some(qr) = &self.qr {
            ui::fill(p, Rect::new(QR_FRAME.x - 24.0, QR_FRAME.y - 24.0, QR_FRAME.w + 48.0, QR_FRAME.h + 48.0), 20.0, ui::WHITE);
            qr.draw(p, QR_FRAME);
        }
        ui::text_raw(p, &self.url, 960.0, QR_FRAME.y + QR_FRAME.h + 60.0, 30, ui::WHITE, ui::ALIGN_CENTER, true);
        ui::text(p, "Scan the code with your phone to open it in YouTube.", 960.0, QR_FRAME.y + QR_FRAME.h + 115.0, 26, ui::DIM, ui::ALIGN_CENTER, false);
        ui::text(p, "BACK closes this screen.", 960.0, 1010.0, 24, ui::FAINT, ui::ALIGN_CENTER, false);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

#[cfg(test)]
mod tests {
    use yt_core::model::{Endpoint, Item};

    #[test]
    fn links() {
        let it = |id: &str| Item { endpoint: Some(Endpoint::Browse { browse_id: id.into(), params: None }), ..Default::default() };
        assert_eq!(super::link_of(&Item::default(), Some("abc")).as_deref(), Some("https://youtu.be/abc"));
        assert_eq!(super::link_of(&it("UCx"), None).as_deref(), Some("https://www.youtube.com/channel/UCx"));
        assert_eq!(super::link_of(&it("VLPLx"), None).as_deref(), Some("https://www.youtube.com/playlist?list=PLx"));
        assert_eq!(super::link_of(&it("FEwhat_to_watch"), None), None);
    }
}

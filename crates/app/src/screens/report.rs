//! Problem report (Settings > Account & about): the app version, the TV and the end of the app's
//! logs, scrolled with Up / Down or the wheel, and a QR code a phone on the same network scans to
//! download the same report as a text file. The report is served only while this screen is open
//! (`crate::problem_report`).

use crate::host::YtHost;
use crate::problem_report::{Report, Unavailable};
use crate::ui;
use plx_machine::machine::{Canon, Cx, Edge, Effects, EntryId, FocusKey, GroupId, Handled, InputEvent, InputKind, Key, LogicalState, Machine, ScreenEvent};
use plx_ui::frame::Budget;
use plx_ui::qr::QrCode;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use std::borrow::Cow;

/// The log column, and where its text starts.
const LOG_RECT: Rect = Rect { x: 80.0, y: 300.0, w: 1160.0, h: 700.0 };
const LINE_H: f32 = 28.0;
const LOG_SIZE: i32 = 20;
/// Lines moved by one key press / one wheel notch.
const KEY_LINES: f32 = 8.0;
const WHEEL_LINES: f32 = 4.0;
/// A log line longer than this many screen lines is cut (the download has it whole).
const MAX_WRAP: usize = 6;
const QR_FRAME: Rect = Rect::new(1400.0, 150.0, 400.0, 400.0);
const COL_X: f32 = 1340.0;
const COL_W: f32 = 520.0;

struct State;
impl LogicalState for State {
    fn write(&self, c: &mut Canon) {
        c.str("problem report");
    }
    fn probe(&self, out: &mut String) {
        out.push_str("problem report");
    }
}

/// The scroll position after moving `by` lines, kept within the text.
fn scrolled(at: f32, by: f32, total: usize, visible: usize) -> f32 {
    (at + by).clamp(0.0, total.saturating_sub(visible) as f32)
}

/// Log lines that fit in the column at once.
fn visible_lines() -> usize {
    (LOG_RECT.h / LINE_H).floor() as usize
}

pub struct ReportScreen {
    entry: EntryId,
    report: Option<Report>,
    qr: Option<QrCode>,
    /// The log lines wrapped to the column (made on the first draw).
    wrapped: Option<Vec<String>>,
    /// First line shown.
    scroll: f32,
}

impl ReportScreen {
    pub fn new(entry: EntryId) -> Self {
        ReportScreen { entry, report: None, qr: None, wrapped: None, scroll: 0.0 }
    }

    fn total(&self) -> usize {
        self.wrapped.as_ref().map_or(0, Vec::len)
    }

    fn scroll_by(&mut self, lines: f32, fx: &mut Effects<'_, YtHost>) {
        self.scroll = scrolled(self.scroll, lines, self.total(), visible_lines());
        fx.invalidate(plx_machine::present::Provenance::Input);
    }
}

impl Drop for ReportScreen {
    fn drop(&mut self) {
        if self.report.is_some() {
            crate::problem_report::close();
        }
    }
}

impl Machine<YtHost> for ReportScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                let report = crate::problem_report::open();
                self.qr = report.link.as_ref().ok().and_then(|l| QrCode::new(l).ok()).map(|mut q| {
                    q.prepare(QR_FRAME);
                    q
                });
                self.report = Some(report);
                self.wrapped = None;
                self.scroll = 0.0;
                fx.invalidate(plx_machine::present::Provenance::Lifecycle);
                Handled::Yes
            }
            ScreenEvent::Unmount => {
                crate::problem_report::close();
                self.report = None;
                self.qr = None;
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Wheel { dy }, .. }) => {
                self.scroll_by(-*dy * WHEEL_LINES, fx);
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: k @ (Key::Up | Key::Down), edge: Edge::Down | Edge::Repeat, .. }, .. }) => {
                self.scroll_by(if *k == Key::Down { KEY_LINES } else { -KEY_LINES }, fx);
                Handled::Yes
            }
            ScreenEvent::Input(_) => Handled::No,
            _ => Handled::Yes,
        }
    }
}

impl Focusable<YtHost> for ReportScreen {
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

impl Screen<YtHost> for ReportScreen {
    fn name(&self) -> &'static str {
        "problem report"
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
        let m = f.measure;
        ui::text(p, "Problem report", 80.0, 60.0, 52, ui::WHITE, ui::ALIGN_LEFT, true);
        let Some(r) = &self.report else { return };
        let tr = crate::i18n::tr;
        let or_unknown = |s: &str| if s.is_empty() { tr("unknown").to_string() } else { s.to_string() };
        let facts = [
            tr("App version: {}").replace("{}", &r.version),
            tr("TV model: {}").replace("{}", &or_unknown(&r.model)),
            tr("webOS version: {}").replace("{}", &or_unknown(&r.webos)),
        ];
        for (i, line) in facts.iter().enumerate() {
            ui::text_cut(p, m, line, 80.0, 150.0 + i as f32 * 40.0, LOG_RECT.w, 28, ui::DIM, ui::ALIGN_LEFT, false);
        }

        // The logs, wrapped once.
        if self.wrapped.is_none() {
            let lines = r.lines.iter().flat_map(|l| if l.trim().is_empty() { vec![String::new()] } else { ui::wrap_lines(m, l, LOG_RECT.w - 24.0, LOG_SIZE, false, MAX_WRAP) }).collect();
            self.wrapped = Some(lines);
        }
        let (total, vis) = (self.total(), visible_lines());
        self.scroll = scrolled(self.scroll, 0.0, total, vis);
        ui::fill(p, Rect::new(LOG_RECT.x - 16.0, LOG_RECT.y - 12.0, LOG_RECT.w + 16.0, LOG_RECT.h + 16.0), 12.0, [1.0, 1.0, 1.0, 0.05]);
        match &self.wrapped {
            Some(lines) if !lines.is_empty() => {
                for (i, line) in lines.iter().skip(self.scroll.floor() as usize).take(vis).enumerate() {
                    let col = if line.starts_with("--- ") { ui::WHITE } else { ui::DIM };
                    ui::text_cut(p, m, line, LOG_RECT.x, LOG_RECT.y + i as f32 * LINE_H, LOG_RECT.w - 24.0, LOG_SIZE, col, ui::ALIGN_LEFT, false);
                }
            }
            _ => {
                ui::text(p, "No log files were found.", LOG_RECT.x, LOG_RECT.y, 28, ui::FAINT, ui::ALIGN_LEFT, false);
            }
        }
        if total > vis {
            // Where the visible part is in the whole.
            let track = Rect::new(LOG_RECT.x + LOG_RECT.w - 10.0, LOG_RECT.y, 6.0, vis as f32 * LINE_H);
            ui::fill(p, track, 3.0, [1.0, 1.0, 1.0, 0.12]);
            let h = (track.h * vis as f32 / total as f32).max(40.0);
            let y = track.y + (track.h - h) * (self.scroll / (total - vis) as f32);
            ui::fill(p, Rect::new(track.x, y, track.w, h), 3.0, ui::WHITE);
        }
        ui::text(p, "Up and Down scroll the logs. BACK closes this screen.", 80.0, 1030.0, 24, ui::FAINT, ui::ALIGN_LEFT, false);

        // The download for a phone.
        match &r.link {
            Ok(link) => {
                if let Some(qr) = &self.qr {
                    ui::fill(p, Rect::new(QR_FRAME.x - 20.0, QR_FRAME.y - 20.0, QR_FRAME.w + 40.0, QR_FRAME.h + 40.0), 20.0, ui::WHITE);
                    qr.draw(p, QR_FRAME);
                }
                let mut y = QR_FRAME.y + QR_FRAME.h + 50.0;
                for line in ui::wrap_lines(m, tr("Scan the code with a phone on the same Wi-Fi network to download the report."), COL_W, 26, false, 4) {
                    ui::text_raw(p, &line, COL_X, y, 26, ui::DIM, ui::ALIGN_LEFT, false);
                    y += 36.0;
                }
                y += 10.0;
                for line in ui::wrap_lines(m, link, COL_W, 26, true, 3) {
                    ui::text_raw(p, &line, COL_X, y, 26, ui::WHITE, ui::ALIGN_LEFT, true);
                    y += 36.0;
                }
                y += 10.0;
                for line in ui::wrap_lines(m, tr("The report is shared only while this screen is open. Stream addresses, sign-in tokens and e-mail addresses are removed from it."), COL_W, 22, false, 5) {
                    ui::text_raw(p, &line, COL_X, y, 22, ui::FAINT, ui::ALIGN_LEFT, false);
                    y += 30.0;
                }
            }
            Err(why) => {
                let msg = match why {
                    Unavailable::NoAddress => tr("This TV has no network address, so the report cannot be downloaded.").to_string(),
                    Unavailable::NoServer(e) => tr("The report cannot be shared: {}").replace("{}", e),
                };
                let mut y = QR_FRAME.y;
                for line in ui::wrap_lines(m, &msg, COL_W, 26, false, 8) {
                    ui::text_raw(p, &line, COL_X, y, 26, ui::DIM, ui::ALIGN_LEFT, false);
                    y += 36.0;
                }
            }
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
    fn scrolling_stays_within_the_text() {
        assert_eq!(scrolled(0.0, -8.0, 100, 25), 0.0);
        assert_eq!(scrolled(0.0, 8.0, 100, 25), 8.0);
        assert_eq!(scrolled(70.0, 8.0, 100, 25), 75.0);
        // Text shorter than the column does not scroll.
        assert_eq!(scrolled(0.0, 8.0, 10, 25), 0.0);
        assert_eq!(visible_lines(), 25);
    }
}

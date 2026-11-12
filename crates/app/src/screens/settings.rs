//! Settings: a root page of categories and one page per category. Each option row shows its
//! value; OK or Right steps forward, Left steps back. Changes are saved immediately.

use crate::host::{SettingsPage, YtArg, YtFx, YtHost};
use crate::settings::{Codec, Repeat, SbAction, Settings};
use crate::store::Section;
use crate::ui::{self, Row, VList};
use plx_machine::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, Key, LogicalState, Machine, NavOp, ScreenEvent,
};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, RenderStrategy, Screen, Step};
use plx_ui::Rect;
use std::borrow::Cow;

const G: GroupId = GroupId(1);
const BASE: u32 = 0x3000_0000;

type Get = fn(&Settings) -> String;
type Change = fn(&mut Settings, i32);

enum Item {
    Page(&'static str, SettingsPage),
    Opt(&'static str, Get, Change),
    Action(&'static str, fn(&mut Effects<'_, YtHost>)),
    Info(&'static str, fn(&Cx<'_, YtHost>) -> String),
}

fn on_off(b: bool) -> String {
    if b { "On".into() } else { "Off".into() }
}

fn cycle<T: Copy + PartialEq>(list: &[T], cur: T, dir: i32) -> T {
    let i = list.iter().position(|x| *x == cur).unwrap_or(0) as i32;
    let n = list.len() as i32;
    list[((i + dir).rem_euclid(n)) as usize]
}

fn sb(a: SbAction) -> String {
    match a {
        SbAction::Off => "Off",
        SbAction::Skip => "Skip",
        SbAction::SkipWithToast => "Skip + notify",
        SbAction::Ask => "Ask",
    }
    .into()
}
const SB: &[SbAction] = &[SbAction::Off, SbAction::Skip, SbAction::SkipWithToast, SbAction::Ask];

fn items(page: SettingsPage) -> Vec<Item> {
    use Item::*;
    match page {
        SettingsPage::Root => vec![
            Page("Playback", SettingsPage::Playback),
            Page("Subtitles", SettingsPage::Subtitles),
            Page("SponsorBlock & DeArrow", SettingsPage::SponsorBlock),
            Page("Content filters", SettingsPage::Content),
            Page("Interface", SettingsPage::Interface),
            Page("Remote control", SettingsPage::Remote),
            Page("Account", SettingsPage::About),
        ],
        SettingsPage::Playback => vec![
            Opt("Maximum quality", |s| format!("{}p", s.max_height), |s, d| s.max_height = cycle(&[480, 720, 1080, 1440, 2160], s.max_height, d)),
            Opt("Start quality (minimum)", |s| format!("{}p", s.min_height), |s, d| s.min_height = cycle(&[144, 360, 480, 720, 1080], s.min_height, d)),
            Opt(
                "Preferred codec",
                |s| match s.codec {
                    Codec::Avc => "H.264 (AVC)".into(),
                    Codec::Vp9 => "VP9".into(),
                    Codec::Av1 => "AV1".into(),
                },
                |s, d| s.codec = cycle(&[Codec::Avc, Codec::Vp9, Codec::Av1], s.codec, d),
            ),
            Opt("Default speed", |s| format!("{}x", s.speed), |s, d| s.speed = cycle(&[0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0], s.speed, d)),
            Opt("Seek step", |s| format!("{} s", s.seek_step_secs), |s, d| s.seek_step_secs = cycle(&[5, 10, 15, 30, 60], s.seek_step_secs, d)),
            Opt(
                "When a video ends",
                |s| match s.repeat {
                    Repeat::Autoplay => "Play next".into(),
                    Repeat::Off => "Stop".into(),
                    Repeat::One => "Repeat video".into(),
                    Repeat::List => "Repeat list".into(),
                },
                |s, d| s.repeat = cycle(&[Repeat::Autoplay, Repeat::Off, Repeat::One, Repeat::List], s.repeat, d),
            ),
            Opt("Resume where you left off", |s| on_off(s.resume_playback), |s, _| s.resume_playback = !s.resume_playback),
            Opt("Hide controls after", |s| format!("{} s", s.hud_timeout_secs), |s, d| s.hud_timeout_secs = cycle(&[2, 3, 4, 6, 8, 12], s.hud_timeout_secs, d)),
            Opt("Clock in player", |s| on_off(s.show_clock), |s, _| s.show_clock = !s.show_clock),
            Opt("Show remaining time", |s| on_off(s.show_ending_time), |s, _| s.show_ending_time = !s.show_ending_time),
        ],
        SettingsPage::Subtitles => vec![
            Opt("Subtitles", |s| on_off(s.subtitles), |s, _| s.subtitles = !s.subtitles),
            Opt(
                "Language",
                |s| s.subtitle_language.clone(),
                |s, d| {
                    const L: &[&str] = &["en", "hu", "de", "fr", "es", "it", "pt", "ru", "uk", "pl", "ro", "ja", "ko", "zh-Hans"];
                    let i = L.iter().position(|l| *l == s.subtitle_language).unwrap_or(0) as i32;
                    s.subtitle_language = L[((i + d).rem_euclid(L.len() as i32)) as usize].into();
                },
            ),
            Opt("Auto-translate when missing", |s| on_off(s.subtitle_auto_translate), |s, _| s.subtitle_auto_translate = !s.subtitle_auto_translate),
            Opt("Size", |s| s.subtitle_size.to_string(), |s, d| s.subtitle_size = cycle(&[32, 38, 44, 52, 60, 72], s.subtitle_size, d)),
            Opt("Position from bottom", |s| format!("{} px", s.subtitle_bottom_offset), |s, d| s.subtitle_bottom_offset = cycle(&[40, 90, 140, 200], s.subtitle_bottom_offset, d)),
        ],
        SettingsPage::SponsorBlock => vec![
            Opt("SponsorBlock", |s| on_off(s.sponsorblock), |s, _| s.sponsorblock = !s.sponsorblock),
            Opt("Sponsor", |s| sb(s.sb_sponsor), |s, d| s.sb_sponsor = cycle(SB, s.sb_sponsor, d)),
            Opt("Self-promotion", |s| sb(s.sb_selfpromo), |s, d| s.sb_selfpromo = cycle(SB, s.sb_selfpromo, d)),
            Opt("Interaction reminder", |s| sb(s.sb_interaction), |s, d| s.sb_interaction = cycle(SB, s.sb_interaction, d)),
            Opt("Intro", |s| sb(s.sb_intro), |s, d| s.sb_intro = cycle(SB, s.sb_intro, d)),
            Opt("Outro / endcards", |s| sb(s.sb_outro), |s, d| s.sb_outro = cycle(SB, s.sb_outro, d)),
            Opt("Preview / recap", |s| sb(s.sb_preview), |s, d| s.sb_preview = cycle(SB, s.sb_preview, d)),
            Opt("Non-music section", |s| sb(s.sb_music_offtopic), |s, d| s.sb_music_offtopic = cycle(SB, s.sb_music_offtopic, d)),
            Opt("Filler", |s| sb(s.sb_filler), |s, d| s.sb_filler = cycle(SB, s.sb_filler, d)),
            Opt("Skip each segment only once", |s| on_off(s.sb_skip_once), |s, _| s.sb_skip_once = !s.sb_skip_once),
            Opt("Segment markers on the seek bar", |s| on_off(s.sb_markers), |s, _| s.sb_markers = !s.sb_markers),
            Opt("DeArrow titles", |s| on_off(s.dearrow_titles), |s, _| s.dearrow_titles = !s.dearrow_titles),
            Opt("DeArrow thumbnails", |s| on_off(s.dearrow_thumbnails), |s, _| s.dearrow_thumbnails = !s.dearrow_thumbnails),
            Opt("Return YouTube Dislike", |s| on_off(s.return_dislikes), |s, _| s.return_dislikes = !s.return_dislikes),
        ],
        SettingsPage::Content => vec![
            Opt("Hide Shorts on Home", |s| on_off(s.hide_shorts_home), |s, _| s.hide_shorts_home = !s.hide_shorts_home),
            Opt("Hide Shorts in Subscriptions", |s| on_off(s.hide_shorts_subscriptions), |s, _| s.hide_shorts_subscriptions = !s.hide_shorts_subscriptions),
            Opt("Hide Shorts in Search", |s| on_off(s.hide_shorts_search), |s, _| s.hide_shorts_search = !s.hide_shorts_search),
            Opt("Hide watched videos", |s| on_off(s.hide_watched), |s, _| s.hide_watched = !s.hide_watched),
            Opt("Hide upcoming streams", |s| on_off(s.hide_upcoming), |s, _| s.hide_upcoming = !s.hide_upcoming),
            Opt("Hide mixes", |s| on_off(s.hide_mixes), |s, _| s.hide_mixes = !s.hide_mixes),
            Opt("Blocked channels", |s| s.blocked_channels.len().to_string(), |s, _| s.blocked_channels.clear()),
            Opt("Child mode", |s| on_off(s.child_mode), |s, _| s.child_mode = !s.child_mode),
        ],
        SettingsPage::Interface => {
            let mut v = vec![
                Opt("Start in", |s| Section::from_key(&s.boot_section).unwrap_or(Section::Home).title().into(), |s, d| {
                    let cur = Section::from_key(&s.boot_section).unwrap_or(Section::Home);
                    s.boot_section = cycle(Section::ALL, cur, d).key().into();
                }),
                Opt("Blank screen when idle (OLED)", |s| format!("{} min", s.idle_blank_secs / 60), |s, d| {
                    s.idle_blank_secs = cycle(&[60, 120, 300, 600], s.idle_blank_secs, d)
                }),
                Opt("24-hour clock", |s| on_off(s.time_24h), |s, _| s.time_24h = !s.time_24h),
            ];
            // One toggle per sidebar section.
            macro_rules! section_toggle {
                ($label:expr, $sec:expr) => {
                    Opt($label, |s| on_off(s.sections.iter().any(|k| k == $sec.key())), |s, _| {
                        let k = $sec.key().to_string();
                        if let Some(i) = s.sections.iter().position(|x| *x == k) {
                            s.sections.remove(i);
                        } else {
                            let order: Vec<&str> = Section::ALL.iter().map(|x| x.key()).collect();
                            s.sections.push(k);
                            s.sections.sort_by_key(|x| order.iter().position(|o| o == x).unwrap_or(99));
                        }
                    })
                };
            }
            v.extend([
                section_toggle!("Sidebar: Home", Section::Home),
                section_toggle!("Sidebar: Subscriptions", Section::Subscriptions),
                section_toggle!("Sidebar: History", Section::History),
                section_toggle!("Sidebar: Library", Section::Library),
                section_toggle!("Sidebar: Music", Section::Music),
                section_toggle!("Sidebar: Gaming", Section::Gaming),
                section_toggle!("Sidebar: News", Section::News),
                section_toggle!("Sidebar: Podcasts", Section::Podcasts),
                section_toggle!("Sidebar: Live", Section::Live),
                section_toggle!("Sidebar: Movies", Section::Movies),
                section_toggle!("Sidebar: Sports", Section::Sports),
                section_toggle!("Sidebar: Trending", Section::Trending),
            ]);
            v
        }
        SettingsPage::Remote => vec![
            Opt("Link with TV code", |s| on_off(s.lounge_enabled), |s, _| s.lounge_enabled = !s.lounge_enabled),
            Info("TV code", |cx| match &cx.views.store.lounge_code {
                Some(c) => c.clone(),
                None if cx.views.store.settings.lounge_enabled => "getting a code\u{2026}".into(),
                None => "off".into(),
            }),
            Info("On your phone", |_| "YouTube \u{203A} Cast \u{203A} Link with TV code".into()),
        ],
        SettingsPage::About => vec![
            Action("Sign in / switch account", |fx| fx.push(Fx::Nav(NavOp::Push(YtArg::SignIn)))),
            Action("Sign out", |fx| {
                fx.push(Fx::App(YtFx::SignOut));
                fx.push(Fx::App(YtFx::Toast("Signed out".into())));
            }),
            Info("Version", |_| env!("CARGO_PKG_VERSION").into()),
            Info("Source", |_| "github.com/danzig666/ytnative".into()),
        ],
    }
}

fn title(page: SettingsPage) -> &'static str {
    match page {
        SettingsPage::Root => "Settings",
        SettingsPage::Playback => "Playback",
        SettingsPage::Subtitles => "Subtitles",
        SettingsPage::SponsorBlock => "SponsorBlock & DeArrow",
        SettingsPage::Content => "Content filters",
        SettingsPage::Interface => "Interface",
        SettingsPage::Remote => "Remote control",
        SettingsPage::About => "Account & about",
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

pub struct SettingsScreen {
    page: SettingsPage,
    list: VList,
    state: State,
}

impl SettingsScreen {
    pub fn new(entry: EntryId, page: SettingsPage) -> Self {
        SettingsScreen {
            page,
            list: VList { entry, group: G, base: BASE, rect: Rect::new(360.0, 170.0, 1200.0, 840.0), row_h: 76.0, text_size: 32 },
            state: State(format!("{page:?}")),
        }
    }

    fn change(&self, i: usize, dir: i32, fx: &mut Effects<'_, YtHost>) {
        let list = items(self.page);
        match list.get(i) {
            Some(Item::Page(_, p)) if dir >= 0 => fx.push(Fx::Nav(NavOp::Push(YtArg::Settings(*p)))),
            Some(Item::Opt(_, _, change)) => {
                let change = *change;
                fx.push(Fx::App(YtFx::EditSettings(Box::new(move |s| change(s, dir)))));
                fx.invalidate(plx_machine::present::Provenance::Input);
            }
            Some(Item::Action(_, act)) if dir >= 0 => act(fx),
            _ => {}
        }
    }
}

impl Machine<YtHost> for SettingsScreen {
    type Ev = ScreenEvent<YtHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, YtHost>, fx: &mut Effects<'_, YtHost>) -> Handled {
        let n = items(self.page).len();
        match ev {
            ScreenEvent::Activate(e) => {
                if let Some(i) = self.list.index(*e, n) {
                    self.change(i, 1, fx);
                }
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: key @ (Key::Left | Key::Right), edge: Edge::Down | Edge::Repeat, .. }, .. }) => {
                if let Some(i) = self.list.focused(cx, n) {
                    if matches!(items(self.page).get(i), Some(Item::Opt(..))) {
                        self.change(i, if *key == Key::Left { -1 } else { 1 }, fx);
                        return Handled::Yes;
                    }
                }
                Handled::No
            }
            ScreenEvent::Uncover => {
                fx.invalidate(plx_machine::present::Provenance::Nav);
                Handled::Yes
            }
            ScreenEvent::Input(_) => Handled::No,
            _ => Handled::Yes,
        }
    }
}

impl Focusable<YtHost> for SettingsScreen {
    fn groups(&self, _cx: &Cx<'_, YtHost>, out: &mut Vec<GroupSpec>) {
        out.push(self.list.spec(items(self.page).len()));
    }
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, YtHost>) -> Option<GroupId> {
        Some(G)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, YtHost>) -> Step<u32> {
        self.list.neighbour(key, dir, items(self.page).len())
    }
    fn place(&self, key: &u32, cx: &Cx<'_, YtHost>, _at: At) -> Option<Placed> {
        let n = items(self.page).len();
        self.list.place(*key, n, self.list.focused(cx, n))
    }
    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        if self.list.index(want.elem, items(self.page).len()).is_some() { want } else { self.list.key(0) }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, YtHost>) -> FocusKey<u32> {
        self.list.key(0)
    }
}

impl Screen<YtHost> for SettingsScreen {
    fn name(&self) -> &'static str {
        "settings"
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
        ui::text(p, title(self.page), 360.0, 70.0, 48, ui::WHITE, ui::ALIGN_LEFT, true);
        let s = &cx.views.store.settings;
        let rows: Vec<Row> = items(self.page)
            .iter()
            .map(|it| match it {
                Item::Page(l, _) => Row::new(*l).value("\u{203A}"),
                Item::Opt(l, get, _) => Row::new(*l).value(format!("\u{2039}  {}  \u{203A}", get(s))),
                Item::Action(l, _) => Row::new(*l),
                Item::Info(l, get) => Row::new(*l).value(get(cx)),
            })
            .collect();
        self.list.draw(p, &rows, self.list.focused(cx, rows.len()));
        super::common::toast(p, cx);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

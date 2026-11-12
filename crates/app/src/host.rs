//! The app's `machine::Host`: screen arguments, effects, messages and views, plus the `Rig` the
//! dispatcher drives (mounting screens, running effects on workers, per-frame preparation).

use crate::player::{Phase, Player, Status};
use crate::screens;
use crate::settings::Settings;
use crate::store::{FeedKey, Store};
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

/// Settings pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsPage {
    Root,
    Playback,
    Subtitles,
    SponsorBlock,
    Content,
    Interface,
    Remote,
    About,
}

/// What a navigation entry shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum YtArg {
    Browse,
    Feed(FeedKey),
    Search,
    Player { video_id: String, playlist: Option<String> },
    Settings(SettingsPage),
    SignIn,
    /// Context menu for a card (by elem).
    Menu(u32),
    /// Player options panel.
    PlayerMenu,
    Accounts,
    Comments(String),
    SearchFilters(String),
}

impl ScreenArg for YtArg {
    fn chrome(&self) -> Chrome {
        Chrome::None
    }
    fn id(&self) -> ScreenId {
        ScreenId(match self {
            YtArg::Browse => 1,
            YtArg::Feed(_) => 2,
            YtArg::Search => 3,
            YtArg::Player { .. } => 4,
            YtArg::Settings(_) => 5,
            YtArg::SignIn => 6,
            YtArg::Menu(_) => 7,
            YtArg::PlayerMenu => 8,
            YtArg::Accounts => 9,
            YtArg::Comments(_) => 10,
            YtArg::SearchFilters(_) => 11,
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
        c.u32(self.id().0);
        c.str(&format!("{self:?}"));
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("{self:?}"));
    }
}

/// Sign-in progress, shown by the sign-in screen.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum SignIn {
    #[default]
    Idle,
    Starting,
    Code { user_code: String, url: String },
    Done(String),
    Failed(String),
}

/// Things the user does to their account / content.
#[derive(Clone, Debug)]
pub enum UserAction {
    Rate(String, yt_core::actions::Rating),
    Subscribe { channel_id: String, subscribe: bool },
    WatchLater(String),
    /// One of a card's own YouTube menu items (Not interested, Remove from history, ...).
    Menu(yt_core::actions::MenuAction),
    SwitchAccount(String),
    RemoveAccount(String),
}

pub enum YtFx {
    Act(UserAction),
    SetQueue(Vec<String>, bool),
    EditFilters(Box<dyn FnOnce(&mut yt_core::search_filters::Filters) + Send>),
    LoadComments { video_id: String, continuation: Option<String> },
    Load(FeedKey),
    More(FeedKey),
    Play { video_id: String, playlist: Option<String> },
    Stop,
    TogglePause,
    /// OK in the player: skip an offered SponsorBlock segment, else toggle pause.
    OkInPlayer,
    SeekBy(i64),
    SeekTo(u64),
    SetSpeed(f32),
    Suggest(String),
    EditSettings(Box<dyn FnOnce(&mut Settings) + Send>),
    SignInStart,
    SignInCancel,
    SignOut,
    Toast(String),
}

pub enum YtMsg {
    Loaded(FeedKey),
    Failed(FeedKey, String),
    Suggestions,
    SignIn,
    Acted,
    Comments,
}

/// Snapshot of playback state for the screens, refreshed once per frame.
pub type PlayerView = Status;

#[derive(Clone, Copy)]
pub struct Views<'a> {
    pub store: &'a Store,
    pub player: &'a PlayerView,
    pub signin: &'a SignIn,
    pub now_ms: u32,
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
            YtArg::Browse => Box::new(screens::browse::BrowseScreen::new(entry, cx)),
            YtArg::Feed(k) => Box::new(screens::feed::FeedScreen::new(entry, k.clone())),
            YtArg::Search => Box::new(screens::search::SearchScreen::new(entry)),
            YtArg::Player { video_id, playlist } => {
                Box::new(screens::player::PlayerScreen::new(entry, video_id.clone(), playlist.clone()))
            }
            YtArg::Settings(page) => Box::new(screens::settings::SettingsScreen::new(entry, *page)),
            YtArg::SignIn => Box::new(screens::signin::SignInScreen::new(entry)),
            YtArg::Menu(elem) => Box::new(screens::menu::MenuScreen::card(entry, *elem, cx)),
            YtArg::PlayerMenu => Box::new(screens::menu::MenuScreen::player(entry, cx)),
            YtArg::Accounts => Box::new(screens::accounts::AccountsScreen::new(entry)),
            YtArg::Comments(v) => Box::new(screens::comments::CommentsScreen::new(entry, v.clone())),
            YtArg::SearchFilters(q) => Box::new(screens::menu::MenuScreen::filters(entry, q.clone())),
        }
    }
}

type Done = (Addr, Work);

/// Results workers post back to the main thread.
enum Work {
    Toast { msg: String, accounts_changed: bool },
    /// DeArrow replacements: (video id, title, thumbnail url).
    Branding(Vec<(String, Option<String>, Option<String>)>),
    Comments { video_id: String, page: Option<yt_core::comments::CommentsPage>, append: bool },
    Page { key: FeedKey, page: yt_core::model::Page, append: bool, failed: Option<String> },
    Suggestions(Vec<String>),
    SignIn(SignIn),
}

pub struct AppRig {
    mounter: Mount,
    pub store: Store,
    pub player_view: PlayerView,
    pub signin: SignIn,
    pub now_ms: u32,
    measure: plx_gfx::text::TtfMeasure,
    pub session: Arc<Mutex<Option<yt_core::session::Session>>>,
    done: Arc<Mutex<Vec<Done>>>,
    signin_cancel: Arc<std::sync::atomic::AtomicBool>,
    next_req: u32,
    pub player: Player,
    sb: yt_core::ext::sponsorblock::SkipTracker,
    /// A segment the user may skip with OK ("Ask" action): (end_ms, label).
    pub sb_offer: Option<(u64, String)>,
    /// Start position for the next Play of this video (phone cast, resume).
    pub pending_start: Option<(String, u64)>,
    /// Default speed to apply once the new video is playing.
    pending_rate: Option<f32>,
}

impl AppRig {
    pub fn new() -> Self {
        let store = Store::new(Settings::load());
        let mut rig = AppRig {
            mounter: Mount,
            store,
            player_view: PlayerView::default(),
            signin: SignIn::Idle,
            now_ms: 0,
            measure: plx_gfx::text::TtfMeasure,
            session: Default::default(),
            done: Default::default(),
            signin_cancel: Default::default(),
            next_req: 0,
            player: Player::new(),
            sb: Default::default(),
            sb_offer: None,
            pending_start: None,
            pending_rate: None,
        };
        rig.refresh_accounts();
        rig
    }

    /// Re-reads the account list (cheap: one small JSON file).
    pub fn refresh_accounts(&mut self) {
        let list = yt_core::accounts::list().unwrap_or_default();
        let current = yt_core::accounts::current().ok().flatten();
        self.store.accounts = list
            .iter()
            .map(|a| crate::store::AccountRow { id: a.id.clone(), name: a.name.clone(), current: current.as_ref().is_some_and(|c| c.id == a.id) })
            .collect();
        self.store.signed_in = current.is_some();
        self.store.account = current.map(|c| c.name);
    }

    fn next_addr(&mut self, to: MachineId) -> Addr {
        self.next_req += 1;
        Addr { to, req: RequestId(self.next_req) }
    }

    fn spawn(&mut self, from: MachineId, name: &'static str, job: impl FnOnce(&Mutex<Option<yt_core::session::Session>>) -> Work + Send + 'static) {
        let addr = self.next_addr(from);
        let (session, done) = (self.session.clone(), self.done.clone());
        let spawned = plx_base::task::spawn(name, move || {
            let r = job(&session);
            done.lock().unwrap().push((addr, r));
            plx_machine::present::wake_from_worker();
        });
        if spawned.is_none() {
            crate::log(&format!("{name}: could not start worker thread"));
        }
    }

    /// Main thread, once per frame: snapshot the player, move finished worker results into the
    /// store, and return the notifications for the screens that asked.
    pub fn drain(&mut self, now_ms: u32) -> Vec<(Addr, YtMsg)> {
        self.now_ms = now_ms;
        self.player_view = self.player.status.lock().unwrap().clone();
        if let Some((_, until)) = &self.store.toast {
            if now_ms > *until {
                self.store.toast = None;
            }
        }
        if self.player_view.phase == Phase::Playing {
            if let Some(r) = self.pending_rate.take() {
                self.player.set_rate(r);
            }
        }
        self.sponsorblock(now_ms);
        let done = std::mem::take(&mut *self.done.lock().unwrap());
        let mut out = Vec::new();
        for (addr, work) in done {
            match work {
                Work::Page { key, page, append, failed } => {
                    if let Some(e) = failed {
                        crate::log(&format!("feed {key:?} failed: {e}"));
                        self.store.fail(&key, e.clone());
                        out.push((addr, YtMsg::Failed(key, e)));
                    } else {
                        let before = if append { self.store.cards(&key).len() } else { 0 };
                        self.store.apply(&key, page, append);
                        crate::log(&format!("feed {key:?}: {} cards", self.store.cards(&key).len()));
                        self.dearrow(addr, &key, before);
                        out.push((addr, YtMsg::Loaded(key)));
                    }
                }
                Work::Toast { msg, accounts_changed } => {
                    self.store.toast(msg, now_ms);
                    if accounts_changed {
                        self.store.feeds.clear();
                        *self.session.lock().unwrap() = None;
                        self.refresh_accounts();
                    }
                    out.push((addr, YtMsg::Acted));
                }
                Work::Comments { video_id, page, append } => {
                    let c = self.store.comments.entry(video_id).or_default();
                    match page {
                        Some(p) => {
                            if append {
                                c.items.extend(p.comments);
                            } else {
                                c.items = p.comments;
                            }
                            c.next = p.next;
                            c.error = None;
                        }
                        None => c.error = Some("Couldn't load comments".into()),
                    }
                    c.loading = false;
                    out.push((addr, YtMsg::Comments));
                }
                Work::Branding(list) => {
                    for (vid, title, thumb) in list {
                        for f in self.store.feeds.values_mut() {
                            for c in f.cards.iter_mut().filter(|c| c.video_id() == Some(vid.as_str())) {
                                if let Some(t) = &title {
                                    c.title = t.clone();
                                }
                                if let Some(u) = &thumb {
                                    c.thumb = u.clone();
                                }
                            }
                            f.gen += 1;
                        }
                    }
                    out.push((addr, YtMsg::Acted));
                }
                Work::Suggestions(s) => {
                    self.store.suggestions = s;
                    out.push((addr, YtMsg::Suggestions));
                }
                Work::SignIn(s) => {
                    if let SignIn::Done(_) = &s {
                        self.store.feeds.clear();
                        *self.session.lock().unwrap() = None;
                        self.refresh_accounts();
                    }
                    self.signin = s;
                    out.push((addr, YtMsg::SignIn));
                }
            }
        }
        out
    }

    /// SponsorBlock: act on the segment playback just entered, per the category's setting.
    fn sponsorblock(&mut self, now_ms: u32) {
        use crate::settings::SbAction;
        if self.player_view.phase != Phase::Playing || !self.store.settings.sponsorblock {
            return;
        }
        let ex = self.player_view.extras.clone();
        let pos = self.player_view.position_ms;
        if let Some((end, _)) = &self.sb_offer {
            if pos >= *end {
                self.sb_offer = None;
            }
        }
        let Some(seg) = self.sb.check(&ex.segments, pos).cloned() else { return };
        let label = seg.category.label().to_string();
        match sb_action(&self.store.settings, &seg.category) {
            SbAction::Off => {}
            SbAction::Skip => self.player.seek_to(seg.end_ms),
            SbAction::SkipWithToast => {
                self.player.seek_to(seg.end_ms);
                self.store.toast(format!("Skipped: {label}"), now_ms);
            }
            SbAction::Ask => {
                self.store.toast(format!("{label} \u{2014} press OK to skip"), now_ms);
                self.sb_offer = Some((seg.end_ms, label));
            }
        }
        self.sb.mark_done(&seg);
    }

    /// DeArrow: replace clickbait titles / thumbnails of newly loaded cards (when enabled).
    fn dearrow(&mut self, addr: Addr, key: &FeedKey, from: usize) {
        let s = &self.store.settings;
        let (titles, thumbs) = (s.dearrow_titles, s.dearrow_thumbnails);
        if !titles && !thumbs {
            return;
        }
        let ids: Vec<String> = self.store.cards(key)[from.min(self.store.cards(key).len())..]
            .iter()
            .filter_map(|c| c.video_id().map(str::to_string))
            .collect();
        if ids.is_empty() {
            return;
        }
        let done = self.done.clone();
        let _ = plx_base::task::spawn("dearrow", move || {
            let mut out = Vec::new();
            for id in ids {
                let Ok(Some(b)) = crate::player::ext().branding(&id) else { continue };
                let title = if titles { b.title() } else { None };
                let thumb = if thumbs { b.thumbnail_url(&id) } else { None };
                if title.is_some() || thumb.is_some() {
                    out.push((id, title, thumb));
                }
            }
            if !out.is_empty() {
                done.lock().unwrap().push((addr, Work::Branding(out)));
                plx_machine::present::wake_from_worker();
            }
        });
    }

    /// OK during an "Ask" segment skips it. Returns true when it did.
    pub fn take_sb_offer(&mut self) -> bool {
        match self.sb_offer.take() {
            Some((end, _)) => {
                self.player.seek_to(end);
                true
            }
            None => false,
        }
    }

    pub fn idle_blank_ms(&self) -> u32 {
        self.store.settings.idle_blank_secs.max(30) * 1000
    }

    pub fn playing(&self) -> bool {
        self.player_view.phase == Phase::Playing
    }
}

/// The configured action for a SponsorBlock category.
pub fn sb_action(s: &Settings, c: &yt_core::ext::sponsorblock::Category) -> crate::settings::SbAction {
    use crate::settings::SbAction;
    use yt_core::ext::sponsorblock::Category as C;
    match c {
        C::Sponsor => s.sb_sponsor,
        C::Intro => s.sb_intro,
        C::Outro => s.sb_outro,
        C::Interaction => s.sb_interaction,
        C::SelfPromo => s.sb_selfpromo,
        C::MusicOfftopic => s.sb_music_offtopic,
        C::Preview => s.sb_preview,
        C::Filler => s.sb_filler,
        _ => SbAction::Off,
    }
}

/// Worker: performs a user action; returns the message to show and whether accounts changed.
fn run_action(session: &Mutex<Option<yt_core::session::Session>>, a: &UserAction) -> (String, bool) {
    use yt_core::actions::Rating;
    let r = match a {
        UserAction::Rate(v, rating) => with_session(session, |s| s.rate(v, *rating)).map(|_| {
            match rating {
                Rating::Like => "Liked",
                Rating::Dislike => "Disliked",
                Rating::None => "Rating removed",
            }
            .to_string()
        }),
        UserAction::Subscribe { channel_id, subscribe } => with_session(session, |s| {
            let info = s.subscribe_info(channel_id).ok().flatten();
            if *subscribe {
                s.subscribe(channel_id, info.as_ref().and_then(|i| i.subscribe_params.as_deref()))?;
                Ok("Subscribed".to_string())
            } else {
                s.unsubscribe(channel_id, info.as_ref().and_then(|i| i.unsubscribe_params.as_deref()))?;
                Ok("Unsubscribed".to_string())
            }
        }),
        UserAction::WatchLater(v) => with_session(session, |s| s.watch_later_add(v)).map(|_| "Saved to Watch later".to_string()),
        UserAction::Menu(m) => with_session(session, |s| s.run_menu_action(m)).map(|ok| if ok { "Done".to_string() } else { "Not available".to_string() }),
        UserAction::SwitchAccount(id) => {
            return match yt_core::accounts::switch(id) {
                Ok(()) => ("Switched account".into(), true),
                Err(e) => (format!("Couldn't switch: {e}"), false),
            }
        }
        UserAction::RemoveAccount(id) => {
            return match yt_core::accounts::remove(id) {
                Ok(_) => ("Account removed".into(), true),
                Err(e) => (format!("Couldn't remove: {e}"), false),
            }
        }
    };
    match r {
        Ok(m) => (m, false),
        Err(e) => {
            crate::log(&format!("action {a:?}: {e:#}"));
            ("That didn't work, try again".into(), false)
        }
    }
}

/// Worker: the shared signed-in session, created on first use.
pub fn with_session<T>(
    session: &Mutex<Option<yt_core::session::Session>>,
    f: impl FnOnce(&mut yt_core::session::Session) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let mut g = session.lock().unwrap();
    if g.is_none() {
        *g = Some(yt_core::session::Session::new()?);
    }
    f(g.as_mut().unwrap())
}

fn load_page(session: &Mutex<Option<yt_core::session::Session>>, key: &FeedKey, continuation: Option<String>) -> Work {
    let r = with_session(session, |s| match (&continuation, key) {
        (Some(c), k) => s.more(k.endpoint(), c),
        (None, FeedKey::Section(sec)) => s.browse(sec.browse_id(), None),
        (None, FeedKey::Browse { id, params }) => s.browse(id, params.as_deref()),
        (None, FeedKey::Search { query, params }) => s.search(query, params.as_deref()),
        (None, FeedKey::Related(_)) => Ok(Default::default()),
    });
    match r {
        Ok(page) => Work::Page { key: key.clone(), page, append: continuation.is_some(), failed: None },
        Err(e) => Work::Page { key: key.clone(), page: Default::default(), append: false, failed: Some(format!("{e:#}")) },
    }
}

impl Rig<YtHost> for AppRig {
    fn split(&mut self) -> Split<'_, YtHost> {
        Split {
            mounter: &mut self.mounter,
            views: Views { store: &self.store, player: &self.player_view, signin: &self.signin, now_ms: self.now_ms },
            measure: &self.measure,
        }
    }
    fn deliver(&mut self, _to: MachineId, _msg: &YtMsg, _p: &CxParts<u32>, _fx: &mut Effects<'_, YtHost>) -> Handled {
        Handled::No
    }
    fn timer(&mut self, _o: MachineId, _id: TimerId, _p: &CxParts<u32>, _fx: &mut Effects<'_, YtHost>) {}
    fn app_fx(&mut self, from: MachineId, fx: YtFx, _p: &CxParts<u32>, _out: &mut Effects<'_, YtHost>) {
        match fx {
            YtFx::Load(key) => {
                if self.store.feed(&key).is_some_and(|f| f.loading) {
                    return;
                }
                self.store.mark_loading(&key);
                self.spawn(from, "feed", move |s| load_page(s, &key, None));
            }
            YtFx::More(key) => {
                let Some(f) = self.store.feed(&key) else { return };
                if f.loading {
                    return;
                }
                let Some(c) = f.continuation.clone() else { return };
                self.store.mark_loading(&key);
                self.spawn(from, "more", move |s| load_page(s, &key, Some(c)));
            }
            YtFx::Play { video_id, playlist } => {
                crate::log(&format!("play {video_id} (playlist {playlist:?})"));
                let s = &self.store.settings;
                let prefs = yt_core::streams::Prefs { min_height: s.min_height as u64, max_height: s.max_height as u64, codec: "avc1" };
                let extras = crate::player::ExtrasPrefs {
                    subtitles: s.subtitles,
                    language: s.subtitle_language.clone(),
                    auto_translate: s.subtitle_auto_translate,
                    sponsorblock: s.sponsorblock,
                    categories: yt_core::ext::sponsorblock::Category::ALL
                        .iter()
                        .filter(|c| sb_action(s, c) != crate::settings::SbAction::Off)
                        .cloned()
                        .collect(),
                    dislikes: s.return_dislikes,
                };
                self.sb = yt_core::ext::sponsorblock::SkipTracker::new(!s.sb_skip_once);
                self.sb_offer = None;
                self.pending_rate = ((s.speed - 1.0).abs() > 0.01).then_some(s.speed);
                let start = self
                    .pending_start
                    .take()
                    .filter(|(v, _)| *v == video_id)
                    .map(|(_, ms)| ms)
                    .or_else(|| if s.resume_playback { self.store.resume_point(&video_id) } else { None });
                self.player.start(video_id, self.session.clone(), prefs, extras, start);
            }
            YtFx::Stop => self.player.stop(),
            YtFx::TogglePause => self.player.toggle_pause(),
            YtFx::OkInPlayer => {
                if !self.take_sb_offer() {
                    self.player.toggle_pause();
                }
            }
            YtFx::SeekBy(ms) => self.player.seek_by(ms),
            YtFx::SeekTo(ms) => self.player.seek_to(ms),
            YtFx::SetSpeed(r) => self.player.set_rate(r),
            YtFx::Suggest(prefix) => {
                self.spawn(from, "suggest", move |s| Work::Suggestions(with_session(s, |s| s.suggestions(&prefix)).unwrap_or_default()));
            }
            YtFx::EditSettings(f) => {
                f(&mut self.store.settings);
                self.store.settings.save();
            }
            YtFx::SignInStart => {
                self.signin = SignIn::Starting;
                self.signin_cancel.store(false, std::sync::atomic::Ordering::SeqCst);
                let cancel = self.signin_cancel.clone();
                let done = self.done.clone();
                let addr = self.next_addr(from);
                let _ = plx_base::task::spawn("signin", move || {
                    let post = |state: SignIn| {
                        done.lock().unwrap().push((addr, Work::SignIn(state)));
                        plx_machine::present::wake_from_worker();
                    };
                    if let Err(e) = crate::signin::run(&cancel, &post) {
                        post(SignIn::Failed(format!("{e:#}")));
                    }
                });
            }
            YtFx::SignInCancel => self.signin_cancel.store(true, std::sync::atomic::Ordering::SeqCst),
            YtFx::SignOut => {
                if let Ok(Some(cur)) = yt_core::accounts::current() {
                    let _ = yt_core::accounts::remove(&cur.id);
                }
                let _ = std::fs::remove_file(yt_core::paths::data_file("token.json"));
                self.store.feeds.clear();
                *self.session.lock().unwrap() = None;
                self.signin = SignIn::Idle;
                self.refresh_accounts();
            }
            YtFx::Act(action) => {
                self.spawn(from, "action", move |s| {
                    let (r, changed) = run_action(s, &action);
                    Work::Toast { msg: r, accounts_changed: changed }
                });
            }
            YtFx::LoadComments { video_id, continuation } => {
                let c = self.store.comments.entry(video_id.clone()).or_default();
                if c.loading {
                    return;
                }
                c.loading = true;
                let append = continuation.is_some();
                self.spawn(from, "comments", move |s| {
                    let page = with_session(s, |s| {
                        let token = match &continuation {
                            Some(t) => t.clone(),
                            None => s.watch(&video_id, None)?.comments_continuation.ok_or_else(|| anyhow::anyhow!("comments are off"))?,
                        };
                        s.comments(&token)
                    });
                    if let Err(e) = &page {
                        crate::log(&format!("comments: {e:#}"));
                    }
                    Work::Comments { video_id, page: page.ok(), append }
                });
            }
            YtFx::Toast(m) => self.store.toast(m, self.now_ms),
            YtFx::SetQueue(q, shorts) => {
                self.store.queue = q;
                self.store.queue_shorts = shorts;
            }
            YtFx::EditFilters(f) => f(&mut self.store.search_filters),
        }
    }
    fn log(&mut self, line: &str) {
        plx_base::eventlog::log(line)
    }
    fn prepare(&mut self, b: &mut Budget, present: &mut Present) {
        crate::images::prepare(b, present);
    }
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

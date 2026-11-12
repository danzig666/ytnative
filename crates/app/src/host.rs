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
    /// Replies to one comment: (replies continuation, parent author).
    Replies(String, String),
    /// Video info: description + chapters.
    Info,
    /// "Save to playlist" picker for a video: (video id, title).
    PlaylistPicker(String, String),
    Pin(PinPurpose),
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
            YtArg::Replies(..) => 12,
            YtArg::Info => 13,
            YtArg::PlaylistPicker(..) => 14,
            YtArg::Pin(_) => 15,
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

/// What a PIN entry is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PinPurpose {
    /// Choose a new settings PIN (entered twice).
    Set,
    /// Settings are locked (PIN set): unlock to open them.
    OpenSettings,
    /// Switch to an account protected by a password: (account id, stored hash).
    SwitchAccount(String, String),
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
    /// Add (true) or remove a video from a playlist.
    Playlist { playlist_id: String, video_id: String, add: bool },
    NewPlaylist { title: String, video_id: String },
    /// Add a channel to a local group (None = a new group named after the channel).
    AddToGroup { group_id: Option<String>, channel_id: String, title: String, icon: Option<String> },
    RemoveGroup(String),
    /// Import channel groups from NewPipe / Grayjay / PocketTube files on USB storage.
    ImportGroups,
    Backup,
    Restore,
    ClearSearchHistory,
    /// One of a card's own YouTube menu items (Not interested, Remove from history, ...).
    Menu(yt_core::actions::MenuAction),
    SwitchAccount(String),
    RemoveAccount(String),
}

pub enum YtFx {
    Act(UserAction),
    LoadChannelInfo(String),
    LoadPlaylists(String),
    SetQueue(Vec<String>, bool),
    EditFilters(Box<dyn FnOnce(&mut yt_core::search_filters::Filters) + Send>),
    /// `key` names the list in the store: a video id (top-level comments) or a replies token.
    LoadComments { key: String, continuation: Option<String>, append: bool },
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
    SetAudioTrack(String),
    SetZoom(crate::player::Zoom),
    /// Minutes until playback pauses (0 = off).
    SleepTimer(u32),
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
            YtArg::Comments(v) => Box::new(screens::comments::CommentsScreen::new(entry, v.clone(), None)),
            YtArg::Replies(token, author) => Box::new(screens::comments::CommentsScreen::new(entry, token.clone(), Some(author.clone()))),
            YtArg::Info => Box::new(screens::info::InfoScreen::new(entry)),
            YtArg::Pin(p) => Box::new(screens::pin::PinScreen::new(entry, p.clone())),
            YtArg::PlaylistPicker(v, t) => Box::new(screens::menu::MenuScreen::playlists(entry, v.clone(), t.clone(), cx)),
            YtArg::SearchFilters(q) => Box::new(screens::menu::MenuScreen::filters(entry, q.clone())),
        }
    }
}

type Done = (Addr, Work);

/// Results workers post back to the main thread.
enum Work {
    Toast { msg: String, accounts_changed: bool },
    ChannelInfo(String, Option<bool>),
    Playlists(String, Vec<yt_core::actions::PlaylistOption>),
    /// DeArrow replacements: (video id, title, thumbnail url).
    Branding(Vec<(String, Option<String>, Option<String>)>),
    Comments { video_id: String, page: Option<yt_core::comments::CommentsPage>, append: bool },
    Page { key: FeedKey, page: yt_core::model::Page, append: bool, failed: Option<String> },
    Suggestions(Vec<String>),
    SignIn(SignIn),
}

pub struct AppRig {
    /// `ui_language` the translations were last switched to.
    applied_language: String,
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
    /// Chosen audio track for the current video: (video id, track id).
    audio_choice: Option<(String, String)>,
    /// Sleep timer deadline (ms, frame clock).
    pub sleep_at: Option<u32>,
}

impl AppRig {
    pub fn new() -> Self {
        let store = Store::new(Settings::load());
        let mut rig = AppRig {
            applied_language: String::new(),
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
            audio_choice: None,
            sleep_at: None,
        };
        rig.refresh_accounts();
        rig.refresh_groups();
        // Fetch the client profiles (remote override, cached) off the main thread.
        let _ = plx_base::task::spawn("clients", || {
            let http = yt_core::net::builder().timeout(std::time::Duration::from_secs(8)).build().unwrap_or_default();
            let (p, src) = yt_core::clients::load(&http);
            yt_core::clients::set_active(p, src);
        });
        rig
    }

    /// Re-reads the local channel groups (one small JSON file).
    pub fn refresh_groups(&mut self) {
        let st = yt_core::channel_groups::Store::load();
        self.store.groups = st.groups.iter().map(|g| (g.id.clone(), g.title.clone())).collect();
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
        crate::ui::set_accent(&self.store.settings.accent);
        crate::ui::set_card_cols(self.store.settings.card_columns);
        if self.store.settings.ui_language != self.applied_language {
            self.applied_language = self.store.settings.ui_language.clone();
            crate::i18n::set_language(&self.applied_language, plx_platform::i18n::current().format_locale());
        }
        self.player.tick();
        self.player_view = self.player.status.lock().unwrap().clone();
        if let Some((_, until)) = &self.store.toast {
            if now_ms > *until {
                self.store.toast = None;
            }
        }
        if let Some(at) = self.sleep_at {
            if now_ms >= at {
                self.sleep_at = None;
                self.store.sleep_minutes = 0;
                if self.player_view.phase == Phase::Playing {
                    self.player.toggle_pause();
                }
                self.store.toast("Sleep timer: playback paused", now_ms);
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
                    self.refresh_groups();
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
                Work::ChannelInfo(id, sub) => {
                    if let Some(s) = sub {
                        self.store.subscribed.insert(id, s);
                    }
                    out.push((addr, YtMsg::Acted));
                }
                Work::Playlists(v, opts) => {
                    self.store.playlist_options.insert(v, opts);
                    out.push((addr, YtMsg::Acted));
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
        UserAction::Playlist { playlist_id, video_id, add } => with_session(session, |s| {
            if *add {
                s.add_to_playlist(playlist_id, video_id).map(|_| "Added to playlist".to_string())
            } else {
                s.remove_from_playlist(playlist_id, video_id).map(|_| "Removed from playlist".to_string())
            }
        }),
        UserAction::NewPlaylist { title, video_id } => with_session(session, |s| {
            s.create_playlist(title, yt_core::actions::Privacy::Private, &[video_id.as_str()]).map(|_| format!("Created playlist \u{201C}{title}\u{201D}"))
        }),
        UserAction::Menu(m) => with_session(session, |s| s.run_menu_action(m)).map(|ok| if ok { "Done".to_string() } else { "Not available".to_string() }),
        UserAction::AddToGroup { group_id, channel_id, title, icon } => (|| {
            use yt_core::channel_groups::{ChannelRef, Store};
            let mut st = Store::load();
            let gid = match group_id {
                Some(g) => g.clone(),
                None => st.create(title),
            };
            let mut c = ChannelRef::new(channel_id.clone());
            c.title = Some(title.clone()).filter(|t| !t.is_empty()).map(Into::into).unwrap_or_default();
            c.icon = icon.clone();
            st.add_channel(&gid, c);
            st.save()?;
            Ok::<_, anyhow::Error>("Added to group".to_string())
        })(),
        UserAction::RemoveGroup(id) => (|| {
            let mut st = yt_core::channel_groups::Store::load();
            st.remove(id);
            st.save()?;
            Ok::<_, anyhow::Error>("Group removed".to_string())
        })(),
        UserAction::ImportGroups => (|| {
            let mut st = yt_core::channel_groups::Store::load();
            let (mut groups, mut chans, mut files) = (0, 0, 0);
            for dir in yt_core::backup::usb_dirs() {
                for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                    let p = e.path();
                    if p.extension().is_some_and(|x| x == "json" || x == "txt") {
                        if let Ok((_, gs)) = yt_core::channel_groups::import_file(&p) {
                            let (g, c) = st.merge(gs);
                            groups += g;
                            chans += c;
                            files += 1;
                        }
                    }
                }
            }
            st.save()?;
            Ok::<_, anyhow::Error>(if files == 0 {
                "No NewPipe / Grayjay / PocketTube export found on USB".to_string()
            } else {
                format!("Imported {groups} groups, {chans} channels")
            })
        })(),
        UserAction::Backup => (|| {
            let dir = yt_core::backup::usb_dirs().into_iter().next().unwrap_or_else(|| yt_core::paths::data_file("backups"));
            std::fs::create_dir_all(&dir)?;
            let path = yt_core::backup::auto_backup(&dir, 5, Default::default())?;
            Ok::<_, anyhow::Error>(format!("Backed up to {}", path.display()))
        })(),
        UserAction::Restore => (|| {
            let mut dirs = yt_core::backup::usb_dirs();
            dirs.push(yt_core::paths::data_file("backups"));
            let latest = dirs.iter().flat_map(|d| yt_core::backup::list_backups(d)).next().ok_or_else(|| anyhow::anyhow!("no backup found"))?;
            let r = yt_core::backup::restore_from(&latest.path, Default::default())?;
            Ok::<_, anyhow::Error>(format!("Restored {} files from {} (restart the app)", r.restored.len(), latest.name))
        })(),
        UserAction::ClearSearchHistory => yt_core::search_history::clear().map(|_| "Search history cleared".to_string()),
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
        (None, FeedKey::Group(id)) => {
            let store = yt_core::channel_groups::Store::load();
            let g = store.get(id).ok_or_else(|| anyhow::anyhow!("this group no longer exists"))?;
            yt_core::channel_groups::group_feed(&s.http, g, yt_core::channel_groups::FeedCache::global())
        }
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
                let audio_track = self.audio_choice.as_ref().filter(|(v, _)| *v == video_id).map(|(_, t)| t.clone());
                let prefs = yt_core::streams::Prefs { min_height: s.min_height as u64, max_height: s.max_height as u64, codec: "avc1", audio_track };
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
                let speed = (s.remember_speed_per_channel)
                    .then(|| self.store.card_channel(&video_id).and_then(|ch| s.speed_by_channel.get(&ch).copied()))
                    .flatten()
                    .unwrap_or(s.speed);
                self.pending_rate = ((speed - 1.0).abs() > 0.01).then_some(speed);
                let start = self
                    .pending_start
                    .take()
                    .filter(|(v, _)| *v == video_id)
                    .map(|(_, ms)| ms)
                    .or_else(|| if s.resume_playback { self.store.resume_point(&video_id) } else { None })
                    .or_else(|| {
                        if s.resume_playback {
                            yt_core::memory::PlayerMemory::load().position(&video_id).map(|secs| secs * 1000)
                        } else {
                            None
                        }
                    });
                #[cfg(not(windows))]
                {
                    let codec = match s.codec {
                        crate::settings::Codec::Avc => crate::feed::VideoCodec::H264,
                        crate::settings::Codec::Vp9 => crate::feed::VideoCodec::Vp9,
                        crate::settings::Codec::Av1 => crate::feed::VideoCodec::Av1,
                    };
                    self.player.feed_prefs = Some(crate::feed::Prefs::new(s.max_height as u32, codec));
                    self.player.prefer_feed = s.engine == "auto";
                }
                self.player.start(video_id, self.session.clone(), prefs, extras, start);
            }
            YtFx::Stop => {
                let v = &self.player_view;
                if !v.video_id.is_empty() && v.duration_ms > 0 {
                    let mut m = yt_core::memory::PlayerMemory::load();
                    m.set_position(&v.video_id, v.position_ms / 1000, v.duration_ms / 1000);
                    let _ = m.save();
                }
                self.player.stop();
            }
            YtFx::TogglePause => self.player.toggle_pause(),
            YtFx::OkInPlayer => {
                if !self.take_sb_offer() {
                    self.player.toggle_pause();
                }
            }
            YtFx::SeekBy(ms) => self.player.seek_by(ms),
            YtFx::SeekTo(ms) => self.player.seek_to(ms),
            YtFx::SetSpeed(r) => {
                self.player.set_rate(r);
                if self.store.settings.remember_speed_per_channel && !self.player_view.author.is_empty() {
                    self.store.settings.speed_by_channel.insert(self.player_view.author.clone(), r);
                    self.store.settings.save();
                }
            }
            YtFx::SetAudioTrack(t) => {
                let v = self.player_view.video_id.clone();
                if !v.is_empty() {
                    self.audio_choice = Some((v.clone(), t));
                    self.pending_start = Some((v.clone(), self.player_view.position_ms));
                    // Restart this video with the new audio track at the same position.
                    self.app_fx(from, YtFx::Play { video_id: v, playlist: None }, _p, _out);
                }
            }
            YtFx::SetZoom(z) => self.player.set_zoom(z),
            YtFx::SleepTimer(min) => {
                self.sleep_at = (min > 0).then(|| self.now_ms + min * 60_000);
                self.store.sleep_minutes = min;
                self.store.toast(if min > 0 { format!("Sleep timer: {min} min") } else { "Sleep timer off".into() }, self.now_ms);
            }
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
            YtFx::LoadChannelInfo(id) => {
                self.spawn(from, "channel", move |s| {
                    let sub = with_session(s, |s| s.subscribe_info(&id)).ok().flatten().map(|i| i.subscribed);
                    Work::ChannelInfo(id, sub)
                });
            }
            YtFx::LoadPlaylists(v) => {
                self.spawn(from, "playlists", move |s| {
                    let opts = with_session(s, |s| s.playlists_for(&v)).unwrap_or_default();
                    Work::Playlists(v, opts)
                });
            }
            YtFx::Act(action) => {
                // Optimistic local state, so the UI reflects the action before YouTube's reads catch up.
                match &action {
                    UserAction::Subscribe { channel_id, subscribe } => {
                        self.store.subscribed.insert(channel_id.clone(), *subscribe);
                    }
                    UserAction::Playlist { playlist_id, video_id, add } => {
                        if let Some(o) = self.store.playlist_options.get_mut(video_id).and_then(|l| l.iter_mut().find(|o| &o.playlist_id == playlist_id)) {
                            o.contains = *add;
                        }
                    }
                    _ => {}
                }
                self.spawn(from, "action", move |s| {
                    let (r, changed) = run_action(s, &action);
                    Work::Toast { msg: r, accounts_changed: changed }
                });
            }
            YtFx::LoadComments { key, continuation, append } => {
                let video_id = key;
                let c = self.store.comments.entry(video_id.clone()).or_default();
                if c.loading {
                    return;
                }
                c.loading = true;
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

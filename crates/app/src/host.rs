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
    /// The blocked channels (OK unblocks one).
    Blocked,
    /// SponsorBlock's seek-bar markers, per category.
    Markers,
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
    /// The list of playback speeds (the player's speed button, as in SmartTube).
    SpeedMenu,
    Accounts,
    Comments(String),
    /// Replies to one comment: (replies continuation, parent author).
    Replies(String, String),
    /// Video info: description + chapters.
    Info,
    /// Chapter selector over the player.
    Chapters,
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
            YtArg::Chapters => 16,
            YtArg::SpeedMenu => 17,
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
    /// More videos of one row of a feed (rows continue to the right).
    MoreShelf { key: FeedKey, row: usize },
    Play { video_id: String, playlist: Option<String> },
    /// The next Play of `video_id` from the start and / or without watch history (card menu).
    PlayOptions { video_id: String, from_start: bool, no_history: bool },
    /// Play `video_id` again from the start (Repeat one).
    Replay { video_id: String, playlist: Option<String> },
    Stop,
    TogglePause,
    /// OK in the player: skip an offered SponsorBlock segment, else toggle pause.
    OkInPlayer,
    SeekBy(i64),
    SeekTo(u64),
    SetSpeed(f32),
    /// OK's "Toggle speed": 1.0 <-> the last other speed chosen.
    ToggleSpeed,
    SetAudioTrack(String),
    SetZoom(crate::player::Zoom),
    /// Shrink the video into `[x, y, w, h]` (comments beside it), or `None` for full screen.
    VideoRect(Option<[i32; 4]>),
    Suggest(String),
    EditSettings(Box<dyn FnOnce(&mut Settings) + Send>),
    SignInStart,
    SignInCancel,
    SignOut,
    Toast(String),
    /// Add the video to "Play next" (or take it out).
    PlayNext { video_id: String, title: String, add: bool },
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
            YtArg::SpeedMenu => Box::new(screens::menu::MenuScreen::speeds(entry, cx)),
            YtArg::Accounts => Box::new(screens::accounts::AccountsScreen::new(entry)),
            YtArg::Comments(v) => Box::new(screens::comments::CommentsScreen::new(entry, v.clone(), None)),
            YtArg::Replies(token, author) => Box::new(screens::comments::CommentsScreen::new(entry, token.clone(), Some(author.clone()))),
            YtArg::Info => Box::new(screens::info::InfoScreen::new(entry)),
            YtArg::Chapters => Box::new(screens::chapters::ChaptersScreen::new(entry)),
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
    /// `epoch`: `AppRig::feeds_epoch` when asked; a page of an older epoch (asked before the feeds
    /// were cleared: another account or region) is dropped.
    Page { epoch: u64, key: FeedKey, page: yt_core::model::Page, append: bool, failed: Option<String> },
    Shelf { epoch: u64, key: FeedKey, row: usize, token: String, result: Result<(Vec<yt_core::model::Item>, Option<String>), String> },
    Suggestions(Vec<String>),
    /// A sign-in worker's progress: (its run number, state). Only the latest run's count.
    SignIn(u32, SignIn),
}

pub struct AppRig {
    /// Video window: where it is going, where it is (animated), what was last applied.
    video_target: [f32; 4],
    video_now: [f32; 4],
    video_applied: [i32; 4],
    video_ms: u32,
    /// `settings.subtitles` as last seen, to load captions when they are switched on mid-video.
    subtitles_were: bool,
    /// The subtitle language as last seen (a change reloads the captions).
    subtitle_language_was: String,
    /// "Subtitles per channel": the video whose channel's subtitles were applied, the state seen
    /// since (a change is the user's, remembered for the channel), and the global state to put
    /// back for channels without one.
    subs_applied_for: String,
    subs_seen: (bool, String),
    subs_global: Option<(bool, String)>,
    /// `ui_language` the translations were last switched to.
    applied_language: String,
    /// `content_country` YouTube's requests were last switched to (None: not yet).
    applied_country: Option<String>,
    mounter: Mount,
    pub store: Store,
    pub player_view: PlayerView,
    pub signin: SignIn,
    pub now_ms: u32,
    measure: plx_gfx::text::TtfMeasure,
    pub session: Arc<Mutex<Option<yt_core::session::Session>>>,
    done: Arc<Mutex<Vec<Done>>>,
    /// The running sign-in's cancel flag (a new one per run) and its run number.
    signin_cancel: Arc<std::sync::atomic::AtomicBool>,
    signin_run: u32,
    /// Bumped whenever the feeds are cleared (see `Work::Page`).
    feeds_epoch: u64,
    next_req: u32,
    pub player: Player,
    sb: yt_core::ext::sponsorblock::SkipTracker,
    /// A jump the user may take with OK ("Ask" action): (target ms, label, offered until ms).
    pub sb_offer: Option<(u64, String, u64)>,
    /// The video whose highlight was handled.
    highlight_for: String,
    /// The video the paid-promotion notice was shown for.
    paid_noticed: String,
    /// The player's extras as fetched (the view's copy shows only the wanted markers), and the
    /// view's copy with what it was made from.
    raw_extras: Arc<crate::player::Extras>,
    shown_extras: Option<(Arc<crate::player::Extras>, (Vec<String>, bool, bool, u64), Arc<crate::player::Extras>)>,
    /// Start position for the next Play of this video (phone cast, resume).
    pub pending_start: Option<(String, u64)>,
    /// Default speed to apply once the new video is playing.
    pending_rate: Option<f32>,
    /// Chosen audio track for the current video: (video id, track id).
    audio_choice: Option<(String, String)>,
    /// Last key press (ms, frame clock): the sleep timer pauses after a while without one.
    pub last_key_ms: u32,
    /// The last video a phone started ("no watch history for videos started from the phone").
    pub phone_video: Option<String>,
    /// Engine B refused a speed change this run: videos wanting a speed other than 1x go to the
    /// system player.
    feed_no_rate: bool,
    /// The video to start again on the system player (refused speed), for the frame loop.
    pub system_restart: Option<(String, Option<String>)>,
    /// The speed that restart keeps, for that video only.
    restart_rate: Option<(String, f32)>,
    /// The account's missing name was looked up again once this run (see Work::Page).
    name_refreshed: bool,
    /// The playlist the current video was started with.
    current_playlist: Option<String>,
    /// "Play without history": asked for the next Play of this video / the one playing so.
    pending_private: Option<String>,
    private_video: Option<String>,
}

impl AppRig {
    pub fn new() -> Self {
        let mut settings = Settings::load();
        crate::device::apply_once(&mut settings);
        let store = Store::new(settings);
        let mut rig = AppRig {
            video_target: [0.0, 0.0, 1920.0, 1080.0],
            video_now: [0.0, 0.0, 1920.0, 1080.0],
            video_applied: [0, 0, 1920, 1080],
            video_ms: 0,
            subtitles_were: false,
            subtitle_language_was: String::new(),
            subs_applied_for: String::new(),
            subs_seen: (false, String::new()),
            subs_global: None,
            applied_language: String::new(),
            applied_country: None,
            mounter: Mount,
            store,
            player_view: PlayerView::default(),
            signin: SignIn::Idle,
            now_ms: 0,
            measure: plx_gfx::text::TtfMeasure,
            session: Default::default(),
            done: Default::default(),
            signin_cancel: Default::default(),
            signin_run: 0,
            feeds_epoch: 0,
            next_req: 0,
            player: Player::new(),
            sb: Default::default(),
            sb_offer: None,
            highlight_for: String::new(),
            paid_noticed: String::new(),
            raw_extras: Default::default(),
            shown_extras: None,
            pending_start: None,
            pending_rate: None,
            feed_no_rate: false,
            system_restart: None,
            restart_rate: None,
            name_refreshed: false,
            current_playlist: None,
            audio_choice: None,
            last_key_ms: 0,
            phone_video: None,
            pending_private: None,
            private_video: None,
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

    /// Drops every loaded list (another account or region); answers still on their way for the
    /// old lists are ignored when they arrive.
    fn clear_feeds(&mut self) {
        self.store.feeds.clear();
        self.feeds_epoch += 1;
    }

    /// Saves the settings, with the global subtitle state in place of a channel's remembered one
    /// ("Subtitles per channel" applies that only while the channel's video plays).
    pub fn save_settings(&self) {
        match &self.subs_global {
            Some((on, lang)) => {
                let mut s = self.store.settings.clone();
                s.subtitles = *on;
                s.subtitle_language = lang.clone();
                s.save();
            }
            None => self.store.settings.save(),
        }
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
        crate::ui::set_black_background(self.store.settings.black_background);
        if crate::ui::titles_under() != self.store.settings.titles_under {
            crate::ui::set_titles_under(self.store.settings.titles_under);
        }
        plx_base::surface::set_ui_scale(self.store.settings.ui_scale.clamp(80, 100) as f32 / 100.0);
        if self.store.settings.ui_language != self.applied_language {
            self.applied_language = self.store.settings.ui_language.clone();
            crate::i18n::set_language(&self.applied_language, plx_platform::i18n::current().format_locale());
        }
        if self.applied_country.as_deref() != Some(self.store.settings.content_country.as_str()) {
            yt_core::innertube::set_content_country(&self.store.settings.content_country);
            // Another region: the lists reload with its content.
            if self.applied_country.is_some() {
                self.clear_feeds();
            }
            self.applied_country = Some(self.store.settings.content_country.clone());
        }
        self.player.tick();
        // Ease the video window toward its target (comments: zoom out to the left), one placement
        // per frame while it moves.
        let dt = now_ms.wrapping_sub(self.video_ms).min(100) as f32;
        self.video_ms = now_ms;
        if self.video_now != self.video_target {
            let a = 1.0 - (-dt / 45.0).exp();
            for i in 0..4 {
                self.video_now[i] += (self.video_target[i] - self.video_now[i]) * a;
                if (self.video_now[i] - self.video_target[i]).abs() < 1.0 {
                    self.video_now[i] = self.video_target[i];
                }
            }
            crate::ui::animating();
        }
        let r = self.video_now.map(|v| v.round() as i32);
        if r != self.video_applied {
            self.video_applied = r;
            self.player.set_video_rect(if r == [0, 0, 1920, 1080] { None } else { Some(r) });
        }
        self.player_view = self.player.status.lock().unwrap().clone();
        // Engine B refused the speed: play this video again on the system player, from here.
        // webOS 4 has no other engine with a video plane (`vplane.rs`): back to normal speed.
        #[cfg(not(windows))]
        let acb = crate::vplane::feed();
        #[cfg(windows)]
        let acb = false;
        if self.player_view.rate_refused && self.system_restart.is_none() {
            self.player.status.lock().unwrap().rate_refused = false;
            let v = self.player_view.video_id.clone();
            if acb {
                crate::log(&format!("speed {}x refused by engine B; webOS 4 has no other engine: back to 1x", self.player_view.rate));
                self.player.set_rate(1.0);
                self.store.toast("This speed is not available on this TV", now_ms);
            } else if !v.is_empty() {
                crate::log(&format!("speed {}x refused by engine B; restarting {v} on the system player", self.player_view.rate));
                self.feed_no_rate = true;
                self.restart_rate = Some((v.clone(), self.player_view.rate));
                self.pending_start = Some((v.clone(), self.player_view.position_ms));
                self.system_restart = Some((v, self.current_playlist.clone()));
                self.store.toast("This speed needs the system player: switching", now_ms);
            }
        }
        self.raw_extras = self.player_view.extras.clone();
        self.player_view.extras = self.shown_extras();
        yt_core::ext::sponsorblock::set_alt_server(self.store.settings.sb_alt_server);
        crate::images::set_dearrow_fallback(crate::settings::thumbnail_frame(&self.store.settings.dearrow_fallback));
        let v = &self.player_view;
        if self.store.settings.sb_paid_notice && v.paid_promotion && v.phase == Phase::Playing && self.paid_noticed != v.video_id {
            self.paid_noticed = v.video_id.clone();
            self.store.toast(crate::i18n::tr("This video contains paid promotion"), now_ms);
        }
        self.channel_subtitles();
        let subs = self.store.settings.subtitles;
        let language_changed = self.store.settings.subtitle_language != self.subtitle_language_was;
        let wanted = (!self.subtitles_were && self.player_view.extras.cues.is_empty()) || (self.subtitles_were && language_changed);
        // The caption tracks are known once the video is resolved (Loading on).
        let can_load = !self.player_view.video_id.is_empty() && !matches!(self.player_view.phase, Phase::Idle | Phase::Resolving | Phase::Failed(_));
        if subs && wanted && can_load {
            let (v, session, status) = (self.player_view.video_id.clone(), self.session.clone(), self.player.status.clone());
            let (lang, tr) = (self.store.settings.subtitle_language.clone(), self.store.settings.subtitle_auto_translate);
            plx_base::task::spawn("captions", move || crate::player::load_captions(&v, &session, &status, &lang, tr));
        }
        // Switched on (or another language) while a video is still starting: keep the change
        // pending until the video is known, so its captions load then.
        let starting = subs && wanted && !can_load && !matches!(self.player_view.phase, Phase::Idle | Phase::Failed(_));
        if !starting {
            self.subtitles_were = subs;
            self.subtitle_language_was = self.store.settings.subtitle_language.clone();
        }
        if let Some((_, until)) = &self.store.toast {
            if now_ms > *until {
                self.store.toast = None;
            }
        }
        // Sleep timer: pause after the chosen time without a key press (any key starts it over).
        let sleep_ms = self.store.settings.sleep_timer_mins.saturating_mul(60_000);
        if sleep_ms > 0 && self.player_view.phase == Phase::Playing && now_ms.wrapping_sub(self.last_key_ms) >= sleep_ms {
            self.last_key_ms = now_ms;
            self.player.toggle_pause();
            self.store.toast("Sleep timer: playback paused", now_ms);
        }
        if self.player_view.phase == Phase::Playing {
            if let Some(r) = self.pending_rate.take() {
                self.player.set_rate(r);
            }
        }
        self.sponsorblock(now_ms);
        self.store.sb_offered = self.sb_offer.is_some();
        let done = std::mem::take(&mut *self.done.lock().unwrap());
        let mut out = Vec::new();
        for (addr, work) in done {
            match work {
                Work::Page { epoch, .. } | Work::Shelf { epoch, .. } if epoch != self.feeds_epoch => {
                    crate::log("feed: an answer for lists cleared since was dropped");
                }
                Work::Page { key, page, append, failed, .. } => {
                    if let Some(e) = failed {
                        crate::log(&format!("feed {key:?} failed: {e}"));
                        self.store.fail(&key, e.clone());
                        out.push((addr, YtMsg::Failed(key, e)));
                    } else {
                        // The session may just have filled in the account's name (a placeholder
                        // account): pick it up for the sidebar and the Accounts page.
                        if !self.name_refreshed && self.store.signed_in && self.store.account.as_deref().is_none_or(|n| n.trim().is_empty()) {
                            self.name_refreshed = true;
                            self.refresh_accounts();
                        }
                        let before = if append { self.store.cards(&key).len() } else { 0 };
                        self.store.apply(&key, page, append);
                        crate::log(&format!("feed {key:?}: {} cards", self.store.cards(&key).len()));
                        let n = self.store.cards(&key).len();
                        self.dearrow(addr, &key, before..n);
                        out.push((addr, YtMsg::Loaded(key)));
                    }
                }
                Work::Shelf { key, row, token, result, .. } => match result {
                    Ok((items, next)) => {
                        let added = self.store.apply_shelf(&key, row, &token, items, next);
                        crate::log(&format!("feed {key:?} row {row}: +{} cards", added.len()));
                        self.dearrow(addr, &key, added);
                        out.push((addr, YtMsg::Loaded(key)));
                    }
                    Err(e) => {
                        crate::log(&format!("feed {key:?} row {row} more failed: {e}"));
                        self.store.fail_shelf(&key, row);
                    }
                },
                Work::Toast { msg, accounts_changed } => {
                    self.store.toast(msg, now_ms);
                    self.refresh_groups();
                    if accounts_changed {
                        self.clear_feeds();
                        reset_session();
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
                Work::SignIn(run, s) => {
                    if let SignIn::Done(_) = &s {
                        self.clear_feeds();
                        reset_session();
                        self.refresh_accounts();
                    }
                    // A cancelled run's late news must not overwrite the current run's screen.
                    if run == self.signin_run {
                        self.signin = s;
                        out.push((addr, YtMsg::SignIn));
                    }
                }
            }
        }
        out
    }

    /// SponsorBlock: act on the segment playback just entered, per the category's setting.
    fn sponsorblock(&mut self, now_ms: u32) {
        use crate::settings::SbAction;
        use yt_core::ext::sponsorblock::Category;
        if self.player_view.phase != Phase::Playing || !self.store.settings.sponsorblock {
            return;
        }
        if self.sb_excluded() {
            self.sb_offer = None;
            return;
        }
        let ex = self.raw_extras.clone();
        let pos = self.player_view.position_ms;
        if let Some((_, _, until)) = &self.sb_offer {
            if pos >= *until {
                self.sb_offer = None;
            }
        }
        // The highlight, once per video when it starts: jump there, or offer the jump.
        if self.highlight_for != self.player_view.video_id {
            if let Some(h) = ex.segments.iter().find(|s| s.category == Category::PoiHighlight).cloned() {
                self.highlight_for = self.player_view.video_id.clone();
                if pos + 1000 < h.start_ms {
                    match sb_action(&self.store.settings, &h.category) {
                        SbAction::Off => {}
                        SbAction::Skip => self.player.seek_to(h.start_ms),
                        SbAction::SkipWithToast => {
                            self.player.seek_to(h.start_ms);
                            self.store.toast(crate::i18n::tr("Jumped to the highlight"), now_ms);
                        }
                        SbAction::Ask => {
                            self.store.toast(crate::i18n::tr("Highlight: press OK to jump there"), now_ms);
                            self.sb_offer = Some((h.start_ms, "Highlight".into(), (pos + 15_000).min(h.start_ms)));
                        }
                    }
                }
            }
        }
        // Only the categories acted on: a longer overlapping segment of a category that is off
        // must not hide one inside it.
        let settings = &self.store.settings;
        let Some(seg) = self.sb.check_where(&ex.segments, pos, |s| sb_action(settings, &s.category) != SbAction::Off).cloned() else { return };
        let label = seg.category.label().to_string();
        let shown = crate::i18n::tr(&label).to_string();
        match sb_action(&self.store.settings, &seg.category) {
            SbAction::Off => {}
            SbAction::Skip => self.player.seek_to(seg.end_ms),
            SbAction::SkipWithToast => {
                self.player.seek_to(seg.end_ms);
                self.store.toast(crate::i18n::tr("Skipped: {}").replace("{}", &shown), now_ms);
            }
            SbAction::Ask => {
                self.store.toast(crate::i18n::tr("{} \u{2014} press OK to skip").replace("{}", &shown), now_ms);
                self.sb_offer = Some((seg.end_ms, label, seg.end_ms));
            }
        }
        self.sb.mark_done(&seg);
    }

    /// DeArrow: replace clickbait titles / thumbnails of newly loaded cards (when enabled).
    fn dearrow(&mut self, addr: Addr, key: &FeedKey, range: std::ops::Range<usize>) {
        use yt_core::ext::dearrow::FallbackFrame;
        let s = &self.store.settings;
        let (titles, thumbs) = (s.dearrow_titles, s.dearrow_thumbnails);
        let fallback = crate::settings::thumbnail_frame(&s.dearrow_fallback);
        if !titles && !thumbs {
            return;
        }
        let cards = self.store.cards(key);
        let ids: Vec<String> = cards[range.start.min(cards.len())..range.end.min(cards.len())]
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
                let b = crate::player::ext().branding(&id).ok().flatten();
                let title = if titles { b.as_ref().and_then(|b| b.title()) } else { None };
                // No crowd thumbnail: YouTube's frame, if one is chosen.
                let thumb = if thumbs { b.as_ref().and_then(|b| b.thumbnail_url(&id)).or_else(|| (fallback != FallbackFrame::Default).then(|| fallback.url(&id))) } else { None };
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

    /// SponsorBlock is off for the playing video's channel.
    fn sb_excluded(&self) -> bool {
        let a = &self.player_view.author;
        !a.is_empty() && self.store.settings.sb_excluded_channels.iter().any(|c| c == a)
    }

    /// The extras the screens see: SponsorBlock segments only of the categories whose markers
    /// are shown (none for an excluded channel), the highlight point as a short mark.
    fn shown_extras(&mut self) -> Arc<crate::player::Extras> {
        let s = &self.store.settings;
        let dur = self.player_view.duration_ms;
        let key = (s.sb_marker_hidden.clone(), self.sb_excluded(), s.sponsorblock, dur / 1000);
        if let Some((src, k, out)) = &self.shown_extras {
            if Arc::ptr_eq(src, &self.raw_extras) && *k == key {
                return out.clone();
            }
        }
        let mut ex = (*self.raw_extras).clone();
        if key.1 || !key.2 {
            ex.segments.clear();
        } else {
            use yt_core::ext::sponsorblock::{ActionType, Category};
            ex.segments.retain(|seg| !key.0.iter().any(|c| c == seg.category.as_str()));
            for seg in ex.segments.iter_mut().filter(|seg| seg.category == Category::PoiHighlight && !seg.is_ranged()) {
                seg.action_type = ActionType::Skip;
                seg.end_ms = seg.start_ms + (dur / 300).max(1000);
            }
        }
        let out = Arc::new(ex);
        self.shown_extras = Some((self.raw_extras.clone(), key, out.clone()));
        out
    }

    /// Whether the playback of `video_id` goes to the watch history (and saves its position).
    fn history_for(&self, video_id: &str) -> bool {
        let s = &self.store.settings;
        let from_phone = self.phone_video.as_deref() == Some(video_id);
        let private = self.private_video.as_deref() == Some(video_id);
        s.watch_history != "off" && !(s.lounge_no_history && from_phone) && !private
    }

    /// "Subtitles per channel": applies the channel's remembered subtitles when its video starts,
    /// and remembers a change made while watching it.
    fn channel_subtitles(&mut self) {
        let (v, ch) = (&self.player_view.video_id, &self.player_view.channel_id);
        let watching = matches!(self.player_view.phase, Phase::Loading | Phase::Playing | Phase::Paused | Phase::Ended);
        if !self.store.settings.subtitles_per_channel || !watching || v.is_empty() || ch.is_empty() {
            return;
        }
        let s = &mut self.store.settings;
        let now = (s.subtitles, s.subtitle_language.clone());
        if self.subs_applied_for != *v {
            self.subs_applied_for = v.clone();
            match yt_core::memory::PlayerMemory::load().channel_subtitle.peek(ch).cloned() {
                Some(l) => {
                    self.subs_global.get_or_insert(now);
                    s.subtitles = !l.is_empty();
                    if !l.is_empty() {
                        s.subtitle_language = l;
                    }
                }
                None => {
                    if let Some((on, lang)) = self.subs_global.take() {
                        s.subtitles = on;
                        s.subtitle_language = lang;
                    }
                }
            }
            self.subs_seen = (s.subtitles, s.subtitle_language.clone());
        } else if now != self.subs_seen {
            // Changed while watching: this channel's from now on; the global one stays.
            self.subs_global.get_or_insert(self.subs_seen.clone());
            let mut m = yt_core::memory::PlayerMemory::load();
            m.set_channel_subtitle(ch, Some(if now.0 { now.1.as_str() } else { "" }));
            let _ = m.save();
            self.subs_seen = now;
        }
    }

    /// Leaving the player: the global subtitle state comes back.
    fn restore_global_subtitles(&mut self) {
        self.subs_applied_for.clear();
        if let Some((on, lang)) = self.subs_global.take() {
            self.store.settings.subtitles = on;
            self.store.settings.subtitle_language = lang;
            self.save_settings();
        }
    }

    /// A speed chosen by the user: the speed OK toggles to, and remembered per the setting.
    fn remember_speed(&mut self, r: f32) {
        use crate::settings::RememberSpeed;
        let s = &mut self.store.settings;
        if (r - 1.0).abs() > 0.01 {
            s.toggle_speed = r;
        }
        match s.remember_speed {
            RememberSpeed::Off => {}
            RememberSpeed::All => s.speed = r,
            RememberSpeed::PerVideo if !self.player_view.video_id.is_empty() => {
                let mut m = yt_core::memory::PlayerMemory::load();
                m.set_video_speed(&self.player_view.video_id, Some(r));
                let _ = m.save();
            }
            RememberSpeed::PerChannel if !self.player_view.author.is_empty() => {
                s.speed_by_channel.insert(self.player_view.author.clone(), r);
            }
            _ => {}
        }
        self.save_settings();
    }

    /// OK during an "Ask" segment skips it. Returns true when it did.
    pub fn take_sb_offer(&mut self) -> bool {
        match self.sb_offer.take() {
            Some((to, _, _)) => {
                self.player.seek_to(to);
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
        C::PoiHighlight => s.sb_highlight,
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
    take_reset(&mut g);
    if g.is_none() {
        *g = Some(yt_core::session::Session::new()?);
    }
    f(g.as_mut().unwrap())
}

/// Another account / signed out: the shared session is dropped by its next user. The main thread
/// never takes the session lock itself, since workers hold it across network calls.
static SESSION_RESET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn reset_session() {
    SESSION_RESET.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// With the session lock held: applies a pending [`reset_session`].
pub fn take_reset(g: &mut Option<yt_core::session::Session>) {
    if SESSION_RESET.swap(false, std::sync::atomic::Ordering::SeqCst) {
        *g = None;
    }
}

fn load_page(session: &Mutex<Option<yt_core::session::Session>>, epoch: u64, key: &FeedKey, continuation: Option<String>) -> Work {
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
        Ok(page) => Work::Page { epoch, key: key.clone(), page, append: continuation.is_some(), failed: None },
        Err(e) => Work::Page { epoch, key: key.clone(), page: Default::default(), append: false, failed: Some(format!("{e:#}")) },
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
                let epoch = self.feeds_epoch;
                self.spawn(from, "feed", move |s| load_page(s, epoch, &key, None));
            }
            YtFx::MoreShelf { key, row } => {
                let Some(token) = self.store.begin_shelf(&key, row) else { return };
                let endpoint = if matches!(key, FeedKey::Related(_)) { "next" } else { "browse" };
                let epoch = self.feeds_epoch;
                self.spawn(from, "shelf", move |s| {
                    let result = crate::host::with_session(s, |se| se.shelf_more(endpoint, &token)).map_err(|e| format!("{e:#}"));
                    Work::Shelf { epoch, key, row, token, result }
                });
            }
            YtFx::More(key) => {
                let Some(f) = self.store.feed(&key) else { return };
                if f.loading {
                    return;
                }
                let Some(c) = f.continuation.clone() else { return };
                self.store.mark_loading(&key);
                let epoch = self.feeds_epoch;
                self.spawn(from, "more", move |s| load_page(s, epoch, &key, Some(c)));
            }
            YtFx::PlayOptions { video_id, from_start, no_history } => {
                if from_start {
                    self.pending_start = Some((video_id.clone(), 0));
                }
                if no_history {
                    self.pending_private = Some(video_id);
                }
            }
            YtFx::Play { video_id, playlist } => {
                crate::log(&format!("play {video_id} (playlist {playlist:?})"));
                // A queued video leaves "Play next" once it plays, however it was started.
                self.store.play_next.retain(|(v, _)| *v != video_id);
                // Without history: asked for this video, or the same playback starting again
                // (repeat, another audio track).
                let again = self.player_view.video_id == video_id && self.player_view.phase != Phase::Idle;
                if self.pending_private.take().as_deref() == Some(video_id.as_str()) {
                    self.private_video = Some(video_id.clone());
                } else if !again {
                    self.private_video = None;
                }
                let s = &self.store.settings;
                let audio_track = self.audio_choice.as_ref().filter(|(v, _)| *v == video_id).map(|(_, t)| t.clone());
                let audio_language = (!s.audio_language.is_empty()).then(|| s.audio_language.clone());
                let prefs = yt_core::streams::Prefs { min_height: s.min_height as u64, max_height: s.max_height as u64, codec: "avc1", audio_track, audio_language };
                let extras = crate::player::ExtrasPrefs {
                    subtitles: s.subtitles,
                    language: s.subtitle_language.clone(),
                    auto_translate: s.subtitle_auto_translate,
                    sponsorblock: s.sponsorblock,
                    // Categories acted on, and those whose markers are shown.
                    categories: yt_core::ext::sponsorblock::Category::ALL
                        .iter()
                        .filter(|c| sb_action(s, c) != crate::settings::SbAction::Off || (s.sb_markers && !s.sb_marker_hidden.iter().any(|h| h == c.as_str())))
                        .cloned()
                        .collect(),
                    dislikes: s.return_dislikes,
                    channel_subtitles: s.subtitles_per_channel,
                    history: self.history_for(&video_id),
                };
                self.sb = yt_core::ext::sponsorblock::SkipTracker::new(!s.sb_skip_once);
                self.sb.min_duration_ms = s.sb_min_segment_ms as u64;
                self.sb_offer = None;
                self.highlight_for.clear();
                let memory = yt_core::memory::PlayerMemory::load();
                let speed = match s.remember_speed {
                    crate::settings::RememberSpeed::PerChannel => self.store.card_channel(&video_id).and_then(|ch| s.speed_by_channel.get(&ch).copied()),
                    crate::settings::RememberSpeed::PerVideo => memory.video_speed.peek(&video_id).copied(),
                    _ => None,
                }
                .unwrap_or(s.speed);
                let speed = self.restart_rate.take().filter(|(v, _)| *v == video_id).map_or(speed, |(_, r)| r);
                self.pending_rate = ((speed - 1.0).abs() > 0.01).then_some(speed);
                self.current_playlist = playlist.clone();
                // Resume: not for short videos (under 5 minutes) or live streams unless allowed.
                let stored = memory.positions.peek(&video_id).copied();
                let duration_secs = stored.map(|p| p.duration).filter(|d| *d > 0).or_else(|| self.store.card_duration_ms(&video_id).map(|ms| ms / 1000));
                let short = duration_secs.is_some_and(|d| d < 300);
                let resume = s.resume_playback && (s.resume_short_videos || !short) && (s.resume_live || !self.store.card_is_live(&video_id));
                let start = self
                    .pending_start
                    .take()
                    .filter(|(v, _)| *v == video_id)
                    .map(|(_, ms)| ms)
                    // This app's own exact record first (0 = start over); YouTube's watched
                    // percentage on the card only for videos never played here.
                    .or_else(|| if resume { stored.map(|p| p.secs * 1000) } else { None })
                    .or_else(|| if resume { self.store.resume_point(&video_id) } else { None });
                #[cfg(not(windows))]
                {
                    let codec = match s.codec {
                        crate::settings::Codec::Avc => crate::feed::VideoCodec::H264,
                        crate::settings::Codec::Vp9 => crate::feed::VideoCodec::Vp9,
                        crate::settings::Codec::Av1 => crate::feed::VideoCodec::Av1,
                    };
                    let mut fp = crate::feed::Prefs::new(s.max_height as u32, codec);
                    fp.allow_hdr = s.hdr;
                    self.player.feed_prefs = Some(fp);
                    // A speed other than 1x on a TV whose engine B refused one: the system player.
                    self.player.prefer_feed = s.engine == "auto" && !(self.feed_no_rate && (speed - 1.0).abs() > 0.01);
                }
                self.player.start(video_id, self.session.clone(), prefs, extras, start);
            }
            YtFx::Replay { video_id, playlist } => {
                self.pending_start = Some((video_id.clone(), 0));
                self.app_fx(from, YtFx::Play { video_id, playlist }, _p, _out);
            }
            YtFx::Stop => {
                let v = &self.player_view;
                // Only a video that really played reports where it is (a BACK while resolving would
                // otherwise store 0 over the resume point).
                let played = matches!(v.phase, crate::player::Phase::Playing | crate::player::Phase::Paused | crate::player::Phase::Ended);
                let keep = (!v.live || self.store.settings.resume_live) && self.history_for(&v.video_id);
                if !v.video_id.is_empty() && v.duration_ms > 0 && played && keep {
                    let mut m = yt_core::memory::PlayerMemory::load();
                    m.set_position(&v.video_id, v.position_ms / 1000, v.duration_ms / 1000);
                    let _ = m.save();
                }
                self.player.stop();
                self.restore_global_subtitles();
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
                self.remember_speed(r);
            }
            YtFx::ToggleSpeed => {
                let cur = if self.player_view.rate > 0.0 { self.player_view.rate } else { 1.0 };
                let other = self.store.settings.toggle_speed;
                let r = if (cur - 1.0).abs() > 0.01 || (other - 1.0).abs() < 0.01 { 1.0 } else { other };
                self.player.set_rate(r);
                self.store.toast(format!("{}: {r}x", crate::i18n::tr("Playback speed")), self.now_ms);
            }
            YtFx::SetAudioTrack(t) => {
                let v = self.player_view.video_id.clone();
                // The chosen language becomes the preferred one (the original track: no preference).
                let original = self.player_view.audio_tracks.iter().any(|x| x.0 == t && x.2);
                self.store.settings.audio_language = if original { String::new() } else { t.split('.').next().unwrap_or("").to_string() };
                self.save_settings();
                if !v.is_empty() {
                    self.audio_choice = Some((v.clone(), t));
                    self.pending_start = Some((v.clone(), self.player_view.position_ms));
                    // Restart this video with the new audio track at the same position.
                    let playlist = self.current_playlist.clone();
                    self.app_fx(from, YtFx::Play { video_id: v, playlist }, _p, _out);
                }
            }
            YtFx::SetZoom(z) => self.player.set_zoom(z),
            YtFx::VideoRect(r) => self.video_target = r.map_or([0.0, 0.0, 1920.0, 1080.0], |r| r.map(|v| v as f32)),
            YtFx::Suggest(prefix) => {
                self.spawn(from, "suggest", move |s| Work::Suggestions(with_session(s, |s| s.suggestions(&prefix)).unwrap_or_default()));
            }
            YtFx::EditSettings(f) => {
                f(&mut self.store.settings);
                self.save_settings();
            }
            YtFx::SignInStart => {
                self.signin = SignIn::Starting;
                // A run still going (cancelled while in a network call) stops for good: each run
                // has its own flag, and its news is told apart by its number.
                self.signin_cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                self.signin_cancel = Default::default();
                self.signin_run += 1;
                let (cancel, run) = (self.signin_cancel.clone(), self.signin_run);
                let done = self.done.clone();
                let addr = self.next_addr(from);
                let _ = plx_base::task::spawn("signin", move || {
                    let post = |state: SignIn| {
                        done.lock().unwrap().push((addr, Work::SignIn(run, state)));
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
                self.clear_feeds();
                reset_session();
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
            YtFx::PlayNext { video_id, title, add } => {
                self.store.play_next.retain(|(v, _)| *v != video_id);
                let msg = if add {
                    self.store.play_next.push((video_id, title));
                    let n = self.store.play_next.len();
                    if n == 1 { crate::i18n::tr("Plays next").to_string() } else { crate::i18n::tr("Plays next ({} in the queue)").replace("{}", &n.to_string()) }
                } else {
                    crate::i18n::tr("Removed from Play next").to_string()
                };
                self.store.toast(msg, self.now_ms);
            }
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

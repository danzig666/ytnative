//! Signed-in user actions (TV client + bearer), SmartTube's actions/playlist/feedback/notifications
//! modules: rating, subscriptions, notification bell, playlists, Watch Later, history control and
//! item-menu feedback ("Not interested", "Don't recommend channel", "Remove from watch history").
//!
//! Every request has a pure body builder (unit-tested) and a `Session` method that sends it.
//! Item menus: [`menu_actions`] reads a raw tile's long-press menu, [`menu_map`] walks a whole raw
//! response into video id -> actions. Home tiles only carry a lazy menu (`MenuCommand::Panel`),
//! resolved with [`Session::menu_panel`].

use crate::innertube::{find, text};
use crate::session::Session;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

pub const WATCH_LATER: &str = "WL";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rating {
    Like,
    Dislike,
    /// Neither (remove like / dislike).
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Privacy {
    Private,
    Unlisted,
    Public,
}

impl Privacy {
    fn as_str(self) -> &'static str {
        match self {
            Privacy::Private => "PRIVATE",
            Privacy::Unlisted => "UNLISTED",
            Privacy::Public => "PUBLIC",
        }
    }
}

// ---------- request bodies ----------

pub fn video_target_body(video_id: &str) -> Value {
    json!({ "target": { "videoId": video_id } })
}

pub fn playlist_target_body(playlist_id: &str) -> Value {
    json!({ "target": { "playlistId": playlist_id } })
}

/// `params` comes from the channel's subscribe button (`subscribeEndpoint.params`); optional.
pub fn subscribe_body(channel_id: &str, params: Option<&str>) -> Value {
    json!({ "channelIds": [channel_id], "params": params.unwrap_or("") })
}

pub fn params_body(params: &str) -> Value {
    json!({ "params": params })
}

pub fn get_add_to_playlist_body(video_ids: &[&str]) -> Value {
    json!({ "videoIds": video_ids })
}

pub fn add_video_body(playlist_id: &str, video_id: &str) -> Value {
    json!({ "playlistId": playlist_id, "actions": [{ "addedVideoId": video_id, "action": "ACTION_ADD_VIDEO" }] })
}

pub fn remove_video_body(playlist_id: &str, video_id: &str) -> Value {
    json!({ "playlistId": playlist_id, "actions": [{ "removedVideoId": video_id, "action": "ACTION_REMOVE_VIDEO_BY_VIDEO_ID" }] })
}

pub fn rename_playlist_body(playlist_id: &str, name: &str) -> Value {
    json!({ "playlistId": playlist_id, "actions": [{ "playlistName": name, "action": "ACTION_SET_PLAYLIST_NAME" }] })
}

pub fn create_playlist_body(title: &str, privacy: Privacy, video_ids: &[&str]) -> Value {
    let mut b = json!({ "title": title, "privacyStatus": privacy.as_str() });
    if !video_ids.is_empty() {
        b["videoIds"] = json!(video_ids);
    }
    b
}

pub fn delete_playlist_body(playlist_id: &str) -> Value {
    json!({ "playlistId": playlist_id })
}

pub fn feedback_body(tokens: &[&str]) -> Value {
    json!({ "feedbackTokens": tokens, "isFeedbackTokenUnencrypted": false, "shouldMerge": false })
}

pub fn panel_body(panel_id: &str, params: &str) -> Value {
    json!({ "panelId": panel_id, "params": params })
}

/// Body of the history switches (`history/pause_watch_history`, `resume_watch_history`,
/// `clear_watch_history`, `clear_search_history`): nothing but the context.
pub fn history_body() -> Value {
    json!({})
}

/// Endpoint for a rating change.
pub fn rating_endpoint(r: Rating) -> &'static str {
    match r {
        Rating::Like => "like/like",
        Rating::Dislike => "like/dislike",
        Rating::None => "like/removelike",
    }
}

// ---------- response parsers ----------

/// One row of the "Save to playlist" dialog (`playlist/get_add_to_playlist`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaylistOption {
    pub playlist_id: String,
    pub title: String,
    /// The video(s) asked about are already in this playlist.
    pub contains: bool,
    /// "PRIVATE" / "UNLISTED" / "PUBLIC".
    pub privacy: Option<String>,
}

pub fn parse_add_to_playlist(r: &Value) -> Vec<PlaylistOption> {
    let mut out = Vec::new();
    walk(r, &mut |k, v| {
        if k == "playlistAddToOptionRenderer" {
            if let Some(id) = v["playlistId"].as_str() {
                out.push(PlaylistOption {
                    playlist_id: id.into(),
                    title: text(&v["title"]).unwrap_or_default(),
                    contains: v["containsSelectedVideos"].as_str().is_some_and(|c| c == "ALL" || c == "SOME"),
                    privacy: v["privacy"].as_str().map(str::to_string),
                });
            }
        }
    });
    out
}

/// A notification bell choice of a subscribed channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotificationOption {
    pub label: String,
    /// SUBSCRIPTION_NOTIFICATION_STATE_ALL / _OCCASIONAL / _OFF.
    pub state: String,
    /// For `Session::set_notifications`.
    pub params: String,
    pub selected: bool,
}

/// State of a channel's subscribe button (channel page, watch-next).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscribeInfo {
    pub channel_id: String,
    pub subscribed: bool,
    pub subscribe_params: Option<String>,
    pub unsubscribe_params: Option<String>,
    pub notifications: Vec<NotificationOption>,
}

/// First `subscribeButtonRenderer` in a response.
pub fn subscribe_info(r: &Value) -> Option<SubscribeInfo> {
    let b = find(r, "subscribeButtonRenderer")?;
    let channel_id = b["channelId"].as_str()?.to_string();
    let p = |key: &str| find(b, key).and_then(|e| e["params"].as_str()).map(str::to_string);
    let mut notifications = Vec::new();
    if let Some(t) = find(b, "subscriptionNotificationToggleButtonRenderer") {
        let current = t["currentStateId"].as_i64();
        for st in t["states"].as_array().into_iter().flatten() {
            let btn = &st["inlineMenuButton"]["buttonRenderer"];
            let Some(params) = btn["serviceEndpoint"]["modifyChannelNotificationPreferenceEndpoint"]["params"].as_str() else {
                continue;
            };
            notifications.push(NotificationOption {
                label: text(&btn["text"]).unwrap_or_default(),
                state: st["notificationState"].as_str().unwrap_or("").into(),
                params: params.into(),
                selected: current.is_some() && st["stateId"].as_i64() == current,
            });
        }
    }
    Some(SubscribeInfo {
        channel_id,
        subscribed: b["subscribed"].as_bool().unwrap_or(false),
        subscribe_params: p("subscribeEndpoint"),
        unsubscribe_params: p("unsubscribeEndpoint"),
        notifications,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LikeInfo {
    pub rating: Rating,
    pub like_count: Option<u64>,
}

/// Like state from a `next` response (`likeButtonRenderer`).
pub fn like_info(r: &Value) -> Option<LikeInfo> {
    let b = find(r, "likeButtonRenderer")?;
    let rating = match b["likeStatus"].as_str()? {
        "LIKE" => Rating::Like,
        "DISLIKE" => Rating::Dislike,
        _ => Rating::None,
    };
    Some(LikeInfo { rating, like_count: b["likeCount"].as_u64() })
}

// ---------- item menus ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MenuKind {
    Play,
    WatchLater,
    RemoveFromWatchLater,
    /// Opens the "Save to playlist" dialog (`Session::playlists_for`).
    SaveToPlaylist,
    RemoveFromPlaylist,
    GoToChannel,
    NotInterested,
    DontRecommendChannel,
    RemoveFromHistory,
    Subscribe,
    Unsubscribe,
    /// Some other feedback token.
    Feedback,
    /// Lazy menu: fetch the real items with `Session::menu_panel`.
    More,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MenuCommand {
    Watch { video_id: String, start_secs: Option<u64> },
    Browse { browse_id: String, params: Option<String> },
    PlaylistEdit { playlist_id: String, video_id: String, add: bool },
    AddToPlaylist { video_id: String },
    Feedback { token: String },
    Subscribe { channel_ids: Vec<String>, params: Option<String> },
    Unsubscribe { channel_ids: Vec<String>, params: Option<String> },
    Panel { panel_id: String, params: String },
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MenuAction {
    pub kind: MenuKind,
    pub label: String,
    pub command: MenuCommand,
}

impl MenuAction {
    /// Feedback token, if this action is a feedback one.
    pub fn token(&self) -> Option<&str> {
        match &self.command {
            MenuCommand::Feedback { token } => Some(token),
            _ => None,
        }
    }
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(|s| s.as_str().map(str::to_string)).collect()
}

/// The meaningful command inside an endpoint/command object.
fn command_of(ep: &Value) -> MenuCommand {
    let Some(m) = ep.as_object() else { return MenuCommand::None };
    if let Some(cmds) = m.get("commandExecutorCommand").and_then(|c| c["commands"].as_array()) {
        return cmds.iter().map(command_of).find(|c| *c != MenuCommand::None).unwrap_or(MenuCommand::None);
    }
    if let Some(inner) = m.get("innertubeCommand") {
        return command_of(inner);
    }
    if let Some(f) = m.get("feedbackEndpoint") {
        if let Some(t) = f["feedbackToken"].as_str() {
            return MenuCommand::Feedback { token: t.into() };
        }
    }
    if let Some(p) = m.get("playlistEditEndpoint") {
        let a = &p["actions"][0];
        let add = a["action"].as_str() == Some("ACTION_ADD_VIDEO");
        let vid = a["addedVideoId"].as_str().or(a["removedVideoId"].as_str()).unwrap_or("");
        return MenuCommand::PlaylistEdit {
            playlist_id: p["playlistId"].as_str().unwrap_or("").into(),
            video_id: vid.into(),
            add,
        };
    }
    if let Some(a) = m.get("addToPlaylistEndpoint") {
        return MenuCommand::AddToPlaylist { video_id: a["videoId"].as_str().unwrap_or("").into() };
    }
    if let Some(o) = m.get("openClientOverlayAction") {
        if o["type"].as_str() == Some("CLIENT_OVERLAY_TYPE_ADD_TO_PLAYLIST") {
            return MenuCommand::AddToPlaylist { video_id: o["context"].as_str().unwrap_or("").into() };
        }
    }
    if let Some(s) = m.get("subscribeEndpoint") {
        return MenuCommand::Subscribe { channel_ids: strs(&s["channelIds"]), params: s["params"].as_str().map(str::to_string) };
    }
    if let Some(s) = m.get("unsubscribeEndpoint") {
        return MenuCommand::Unsubscribe { channel_ids: strs(&s["channelIds"]), params: s["params"].as_str().map(str::to_string) };
    }
    if let Some(w) = m.get("watchEndpoint") {
        if let Some(id) = w["videoId"].as_str() {
            return MenuCommand::Watch { video_id: id.into(), start_secs: w["startTimeSeconds"].as_u64() };
        }
    }
    if let Some(b) = m.get("browseEndpoint") {
        if let Some(id) = b["browseId"].as_str() {
            return MenuCommand::Browse { browse_id: id.into(), params: b["params"].as_str().map(str::to_string) };
        }
    }
    if let Some(p) = m.get("showEngagementPanelEndpoint") {
        if let (Some(tag), Some(params)) = (p["identifier"]["tag"].as_str(), p["globalConfiguration"]["params"].as_str()) {
            return MenuCommand::Panel { panel_id: tag.into(), params: params.into() };
        }
    }
    // Confirmation popups ("Don't recommend channel" -> Confirm) hide the real command inside.
    if m.contains_key("openPopupAction") || m.contains_key("authDeterminedCommand") {
        for key in ["feedbackEndpoint", "unsubscribeEndpoint", "subscribeEndpoint"] {
            if let Some(e) = find(ep, key) {
                let c = command_of(&json!({ key: e }));
                if c != MenuCommand::None {
                    return c;
                }
            }
        }
    }
    MenuCommand::None
}

fn classify(label: &str, cmd: &MenuCommand, raw: &Value) -> MenuKind {
    let l = label.to_lowercase();
    match cmd {
        MenuCommand::Watch { .. } => MenuKind::Play,
        MenuCommand::Browse { browse_id, .. } if browse_id.starts_with("UC") => MenuKind::GoToChannel,
        MenuCommand::Browse { .. } => MenuKind::Other,
        MenuCommand::PlaylistEdit { playlist_id, add, .. } => match (playlist_id == WATCH_LATER, add) {
            (true, true) => MenuKind::WatchLater,
            (true, false) => MenuKind::RemoveFromWatchLater,
            (false, true) => MenuKind::SaveToPlaylist,
            (false, false) => MenuKind::RemoveFromPlaylist,
        },
        MenuCommand::AddToPlaylist { .. } => MenuKind::SaveToPlaylist,
        MenuCommand::Subscribe { .. } => MenuKind::Subscribe,
        MenuCommand::Unsubscribe { .. } => MenuKind::Unsubscribe,
        MenuCommand::Panel { .. } => MenuKind::More,
        MenuCommand::Feedback { .. } => {
            let s = raw.to_string();
            if l.contains("history") {
                MenuKind::RemoveFromHistory
            } else if l.contains("recommend channel") || s.contains("removeItemAction") {
                MenuKind::DontRecommendChannel
            } else if l.contains("not interested") || s.contains("DISMISSAL_FOLLOW_UP") {
                MenuKind::NotInterested
            } else {
                MenuKind::Feedback
            }
        }
        MenuCommand::None => MenuKind::Other,
    }
}

fn action(label: String, ep: &Value) -> MenuAction {
    let command = command_of(ep);
    MenuAction { kind: classify(&label, &command, ep), label, command }
}

/// Items of a `menuRenderer` (classic TV/web menus).
fn menu_renderer_items(menu: &Value, out: &mut Vec<MenuAction>) {
    for it in menu["items"].as_array().into_iter().flatten() {
        let (r, ep) = if let Some(r) = it.get("menuNavigationItemRenderer") {
            (r, &r["navigationEndpoint"])
        } else if let Some(r) = it.get("menuServiceItemRenderer") {
            (r, &r["serviceEndpoint"])
        } else {
            continue;
        };
        out.push(action(text(&r["text"]).unwrap_or_default(), ep));
    }
}

/// Items of a `listViewModel` (the `get_panel` context menu).
fn list_view_items(list: &Value, out: &mut Vec<MenuAction>) {
    for it in list["listItems"].as_array().into_iter().flatten() {
        let li = &it["listItemViewModel"];
        let label = text(&li["title"]).unwrap_or_default();
        out.push(action(label, &li["rendererContext"]["commandContext"]["onTap"]));
    }
}

/// Menu actions of one raw tile (`tileRenderer` object, or `{"tileRenderer": ...}`).
pub fn menu_actions(tile: &Value) -> Vec<MenuAction> {
    let t = tile.get("tileRenderer").unwrap_or(tile);
    let mut out = Vec::new();
    for key in ["onLongPressCommand", "menu"] {
        let cmd = &t[key];
        if cmd.is_null() {
            continue;
        }
        if let Some(menu) = find(cmd, "menuRenderer") {
            menu_renderer_items(menu, &mut out);
        } else if let Some(list) = find(cmd, "listViewModel") {
            list_view_items(list, &mut out);
        } else {
            let a = action(String::new(), cmd);
            if a.command != MenuCommand::None {
                out.push(a);
            }
        }
    }
    out
}

/// Menu actions of a `get_panel` response (the lazy context menu of Home tiles).
pub fn parse_panel_menu(r: &Value) -> Vec<MenuAction> {
    let mut out = Vec::new();
    if let Some(list) = find(r, "listViewModel") {
        list_view_items(list, &mut out);
    } else if let Some(menu) = find(r, "menuRenderer") {
        menu_renderer_items(menu, &mut out);
    }
    out
}

/// Video id of a raw tile.
pub fn tile_video_id(t: &Value) -> Option<String> {
    let t = t.get("tileRenderer").unwrap_or(t);
    let ct = t["contentType"].as_str().unwrap_or("TILE_CONTENT_TYPE_VIDEO");
    if ct != "TILE_CONTENT_TYPE_VIDEO" && ct != "TILE_CONTENT_TYPE_SHORT" {
        return None;
    }
    t["contentId"]
        .as_str()
        .or_else(|| find(&t["onSelectCommand"], "watchEndpoint").and_then(|w| w["videoId"].as_str()))
        .map(str::to_string)
}

/// Walks a raw browse/next/search response: video id -> menu actions of its tile(s).
pub fn menu_map(r: &Value) -> HashMap<String, Vec<MenuAction>> {
    let mut map: HashMap<String, Vec<MenuAction>> = HashMap::new();
    walk(r, &mut |k, v| {
        if k == "tileRenderer" {
            if let Some(id) = tile_video_id(v) {
                let acts = menu_actions(v);
                if !acts.is_empty() {
                    map.entry(id).or_insert(acts);
                }
            }
        }
    });
    map
}

/// Calls `f(key, value)` for every object entry, depth first.
fn walk(v: &Value, f: &mut dyn FnMut(&str, &Value)) {
    match v {
        Value::Object(m) => {
            for (k, c) in m {
                f(k, c);
                walk(c, f);
            }
        }
        Value::Array(a) => a.iter().for_each(|c| walk(c, f)),
        _ => {}
    }
}

// ---------- Session ----------

/// Client for the playlist create/delete endpoints: TVHTML5 is refused ("Precondition check
/// failed"), WEB/ANDROID reject the TV OAuth token ("invalid argument"), the embedded TV player
/// client accepts it (verified 2026-11).
const EMBED_CLIENT: (&str, &str, &str) = ("TVHTML5_SIMPLY_EMBEDDED_PLAYER", "85", "2.0");

pub fn embed_context(visitor_data: Option<&str>) -> Value {
    let (name, _, ver) = EMBED_CLIENT;
    json!({ "client": { "clientName": name, "clientVersion": ver, "hl": "en", "gl": "US", "visitorData": visitor_data.unwrap_or_default() } })
}

impl Session {
    /// A signed-in InnerTube call as the embedded TV player client (same OAuth token).
    pub fn embed_raw(&mut self, endpoint: &str, mut body: Value) -> Result<Value> {
        let bearer = self.bearer()?.with_context(|| format!("{endpoint}: not signed in"))?;
        let (_, id, ver) = EMBED_CLIENT;
        body["context"] = embed_context(self.cfg.visitor_data.as_deref());
        let mut req = self
            .http
            .post(format!("https://www.youtube.com/youtubei/v1/{endpoint}?prettyPrint=false"))
            .header("User-Agent", crate::bootstrap::TV_UA)
            .header("X-YouTube-Client-Name", id)
            .header("X-YouTube-Client-Version", ver)
            .header("Origin", "https://www.youtube.com")
            .bearer_auth(bearer)
            .json(&body);
        if let Some(v) = &self.cfg.visitor_data {
            req = req.header("X-Goog-Visitor-Id", v);
        }
        if let Some(p) = &self.page_id {
            req = req.header("X-Goog-PageId", p);
        }
        let resp = req.send()?;
        let status = resp.status();
        let v: Value = resp.json().unwrap_or(Value::Null);
        if !status.is_success() {
            bail!("{endpoint} (embedded client) -> HTTP {status}: {}", v["error"]["message"]);
        }
        Ok(v)
    }

    /// `raw` that requires a signed-in session.
    pub fn signed_raw(&mut self, endpoint: &str, body: Value) -> Result<Value> {
        if !self.signed_in() {
            bail!("{endpoint}: not signed in");
        }
        self.raw(endpoint, body)
    }

    pub fn rate(&mut self, video_id: &str, rating: Rating) -> Result<()> {
        self.signed_raw(rating_endpoint(rating), video_target_body(video_id)).map(drop)
    }

    pub fn like(&mut self, video_id: &str) -> Result<()> {
        self.rate(video_id, Rating::Like)
    }

    pub fn dislike(&mut self, video_id: &str) -> Result<()> {
        self.rate(video_id, Rating::Dislike)
    }

    /// Clears a like or a dislike.
    pub fn remove_like(&mut self, video_id: &str) -> Result<()> {
        self.rate(video_id, Rating::None)
    }

    pub fn subscribe(&mut self, channel_id: &str, params: Option<&str>) -> Result<()> {
        self.signed_raw("subscription/subscribe", subscribe_body(channel_id, params)).map(drop)
    }

    pub fn unsubscribe(&mut self, channel_id: &str, params: Option<&str>) -> Result<()> {
        self.signed_raw("subscription/unsubscribe", subscribe_body(channel_id, params)).map(drop)
    }

    /// Subscribe button state of a channel (from its TV browse page).
    pub fn subscribe_info(&mut self, channel_id: &str) -> Result<Option<SubscribeInfo>> {
        let r = self.raw("browse", json!({ "browseId": channel_id }))?;
        Ok(subscribe_info(&r))
    }

    /// Like state + subscribe button of a video (`next`).
    pub fn video_state(&mut self, video_id: &str) -> Result<(Option<LikeInfo>, Option<SubscribeInfo>)> {
        let r = self.raw("next", json!({ "videoId": video_id }))?;
        Ok((like_info(&r), subscribe_info(&r)))
    }

    /// Notification bell: `params` from a `NotificationOption`.
    pub fn set_notifications(&mut self, params: &str) -> Result<()> {
        self.signed_raw("notification/modify_channel_preference", params_body(params)).map(drop)
    }

    /// The user's playlists (Watch Later first), each saying whether it contains `video_id`.
    pub fn playlists_for(&mut self, video_id: &str) -> Result<Vec<PlaylistOption>> {
        let r = self.signed_raw("playlist/get_add_to_playlist", get_add_to_playlist_body(&[video_id]))?;
        Ok(parse_add_to_playlist(&r))
    }

    /// The user's own playlists (Watch Later included).
    pub fn my_playlists(&mut self) -> Result<Vec<PlaylistOption>> {
        // Any public video works: the dialog lists every playlist of the account.
        self.playlists_for("jNQXAC9IVRw").map(|mut v| {
            v.iter_mut().for_each(|p| p.contains = false);
            v
        })
    }

    pub fn add_to_playlist(&mut self, playlist_id: &str, video_id: &str) -> Result<()> {
        self.signed_raw("browse/edit_playlist", add_video_body(playlist_id, video_id)).map(drop)
    }

    pub fn remove_from_playlist(&mut self, playlist_id: &str, video_id: &str) -> Result<()> {
        self.signed_raw("browse/edit_playlist", remove_video_body(playlist_id, video_id)).map(drop)
    }

    pub fn rename_playlist(&mut self, playlist_id: &str, name: &str) -> Result<()> {
        self.signed_raw("browse/edit_playlist", rename_playlist_body(playlist_id, name)).map(drop)
    }

    pub fn watch_later_add(&mut self, video_id: &str) -> Result<()> {
        self.add_to_playlist(WATCH_LATER, video_id)
    }

    pub fn watch_later_remove(&mut self, video_id: &str) -> Result<()> {
        self.remove_from_playlist(WATCH_LATER, video_id)
    }

    /// Creates a playlist; returns its id.
    pub fn create_playlist(&mut self, title: &str, privacy: Privacy, video_ids: &[&str]) -> Result<String> {
        let r = self.embed_raw("playlist/create", create_playlist_body(title, privacy, video_ids))?;
        r["playlistId"].as_str().map(str::to_string).context(format!("playlist/create: no playlistId in {r}"))
    }

    pub fn delete_playlist(&mut self, playlist_id: &str) -> Result<()> {
        self.embed_raw("playlist/delete", delete_playlist_body(playlist_id)).map(drop)
    }

    /// Saves someone else's playlist to the library.
    pub fn save_playlist(&mut self, playlist_id: &str) -> Result<()> {
        self.signed_raw("like/like", playlist_target_body(playlist_id)).map(drop)
    }

    pub fn unsave_playlist(&mut self, playlist_id: &str) -> Result<()> {
        self.signed_raw("like/removelike", playlist_target_body(playlist_id)).map(drop)
    }

    pub fn pause_watch_history(&mut self) -> Result<()> {
        self.signed_raw("history/pause_watch_history", history_body()).map(drop)
    }

    pub fn resume_watch_history(&mut self) -> Result<()> {
        self.signed_raw("history/resume_watch_history", history_body()).map(drop)
    }

    /// Deletes the whole watch history. Irreversible.
    pub fn clear_watch_history(&mut self) -> Result<()> {
        self.signed_raw("history/clear_watch_history", history_body()).map(drop)
    }

    /// Deletes the whole search history. Irreversible.
    pub fn clear_search_history(&mut self) -> Result<()> {
        self.signed_raw("history/clear_search_history", history_body()).map(drop)
    }

    /// Sends feedback tokens ("Not interested", "Remove from watch history", ...).
    pub fn feedback(&mut self, tokens: &[&str]) -> Result<()> {
        let r = self.signed_raw("feedback", feedback_body(tokens))?;
        let ok = r["feedbackResponses"]
            .as_array()
            .map_or(true, |a| a.iter().all(|x| x["isProcessed"].as_bool().unwrap_or(true)));
        if !ok {
            bail!("feedback not processed: {r}");
        }
        Ok(())
    }

    /// Resolves a lazy tile menu (`MenuCommand::Panel`).
    pub fn menu_panel(&mut self, panel_id: &str, params: &str) -> Result<Vec<MenuAction>> {
        Ok(parse_panel_menu(&self.raw("get_panel", panel_body(panel_id, params))?))
    }

    /// Runs a menu action that is a service call. Returns false for navigation-type actions
    /// (Play, Go to channel, Save to playlist dialog, More) which the UI handles itself.
    pub fn run_menu_action(&mut self, a: &MenuAction) -> Result<bool> {
        match &a.command {
            MenuCommand::Feedback { token } => self.feedback(&[token])?,
            MenuCommand::PlaylistEdit { playlist_id, video_id, add: true } => self.add_to_playlist(playlist_id, video_id)?,
            MenuCommand::PlaylistEdit { playlist_id, video_id, add: false } => self.remove_from_playlist(playlist_id, video_id)?,
            MenuCommand::Subscribe { channel_ids, params } => {
                for c in channel_ids {
                    self.subscribe(c, params.as_deref())?;
                }
            }
            MenuCommand::Unsubscribe { channel_ids, params } => {
                for c in channel_ids {
                    self.unsubscribe(c, params.as_deref())?;
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bodies() {
        assert_eq!(video_target_body("v"), json!({"target":{"videoId":"v"}}));
        assert_eq!(playlist_target_body("PL1"), json!({"target":{"playlistId":"PL1"}}));
        assert_eq!(subscribe_body("UC1", Some("EgIIAhgA")), json!({"channelIds":["UC1"],"params":"EgIIAhgA"}));
        assert_eq!(subscribe_body("UC1", None)["params"], "");
        assert_eq!(params_body("p"), json!({"params":"p"}));
        assert_eq!(get_add_to_playlist_body(&["v"]), json!({"videoIds":["v"]}));
        assert_eq!(
            add_video_body("WL", "v"),
            json!({"playlistId":"WL","actions":[{"addedVideoId":"v","action":"ACTION_ADD_VIDEO"}]})
        );
        assert_eq!(
            remove_video_body("PL", "v"),
            json!({"playlistId":"PL","actions":[{"removedVideoId":"v","action":"ACTION_REMOVE_VIDEO_BY_VIDEO_ID"}]})
        );
        assert_eq!(rename_playlist_body("PL", "n")["actions"][0]["action"], "ACTION_SET_PLAYLIST_NAME");
        assert_eq!(create_playlist_body("t", Privacy::Private, &[]), json!({"title":"t","privacyStatus":"PRIVATE"}));
        assert_eq!(create_playlist_body("t", Privacy::Public, &["v"])["videoIds"], json!(["v"]));
        assert_eq!(delete_playlist_body("PL"), json!({"playlistId":"PL"}));
        assert_eq!(feedback_body(&["tok"])["feedbackTokens"], json!(["tok"]));
        assert_eq!(panel_body("PAcontext_menu", "x"), json!({"panelId":"PAcontext_menu","params":"x"}));
        assert_eq!(history_body(), json!({}));
        assert_eq!(rating_endpoint(Rating::Like), "like/like");
        assert_eq!(rating_endpoint(Rating::Dislike), "like/dislike");
        assert_eq!(rating_endpoint(Rating::None), "like/removelike");
    }

    #[test]
    fn add_to_playlist_parse() {
        let r = json!({"contents":[{"addToPlaylistRenderer":{"playlists":[
            {"playlistAddToOptionRenderer":{"playlistId":"WL","title":{"simpleText":"Watch later"},"containsSelectedVideos":"NONE","privacy":"PRIVATE"}},
            {"playlistAddToOptionRenderer":{"playlistId":"PL1","title":{"runs":[{"text":"Mine"}]},"containsSelectedVideos":"ALL","privacy":"PUBLIC"}}
        ]}}]});
        let p = parse_add_to_playlist(&r);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].playlist_id, "WL");
        assert!(!p[0].contains);
        assert_eq!(p[1].title, "Mine");
        assert!(p[1].contains);
        assert_eq!(p[1].privacy.as_deref(), Some("PUBLIC"));
    }

    #[test]
    fn subscribe_and_like_parse() {
        let r = json!({"x":{"subscribeButtonRenderer":{
            "channelId":"UC1","subscribed":true,
            "notificationPreferenceButton":{"subscriptionNotificationToggleButtonRenderer":{"currentStateId":3,"states":[
                {"stateId":2,"notificationState":"SUBSCRIPTION_NOTIFICATION_STATE_ALL","inlineMenuButton":{"buttonRenderer":{"text":{"simpleText":"All"},"serviceEndpoint":{"modifyChannelNotificationPreferenceEndpoint":{"params":"PA"}}}}},
                {"stateId":3,"notificationState":"SUBSCRIPTION_NOTIFICATION_STATE_OCCASIONAL","inlineMenuButton":{"buttonRenderer":{"text":{"simpleText":"Personalized"},"serviceEndpoint":{"modifyChannelNotificationPreferenceEndpoint":{"params":"PP"}}}}},
                {"state":{"buttonRenderer":{"command":{"commandExecutorCommand":{"commands":[{"unsubscribeEndpoint":{"channelIds":["UC1"],"params":"CgIIAhgA"}}]}}}}}
            ]}},
            "serviceEndpoints":[{"authDeterminedCommand":{"authenticatedCommand":{"subscribeEndpoint":{"channelIds":["UC1"],"params":"EgIIAhgA"}}}}]
        }},
        "likeButtonRenderer":{"likeStatus":"LIKE","likeCount":42}});
        let s = subscribe_info(&r).unwrap();
        assert_eq!(s.channel_id, "UC1");
        assert!(s.subscribed);
        assert_eq!(s.subscribe_params.as_deref(), Some("EgIIAhgA"));
        assert_eq!(s.unsubscribe_params.as_deref(), Some("CgIIAhgA"));
        assert_eq!(s.notifications.len(), 2);
        assert!(!s.notifications[0].selected);
        assert!(s.notifications[1].selected);
        assert_eq!(s.notifications[1].params, "PP");
        let l = like_info(&r).unwrap();
        assert_eq!(l.rating, Rating::Like);
        assert_eq!(l.like_count, Some(42));
    }

    fn history_tile() -> Value {
        json!({"tileRenderer":{"contentId":"vid","contentType":"TILE_CONTENT_TYPE_VIDEO",
          "onSelectCommand":{"watchEndpoint":{"videoId":"vid","startTimeSeconds":22}},
          "onLongPressCommand":{"showMenuCommand":{"menu":{"menuRenderer":{"items":[
            {"menuNavigationItemRenderer":{"text":{"runs":[{"text":"Play"}]},"navigationEndpoint":{"watchEndpoint":{"videoId":"vid","startTimeSeconds":22}}}},
            {"menuServiceItemRenderer":{"text":{"runs":[{"text":"Remove from watch history"}]},"serviceEndpoint":{"feedbackEndpoint":{"feedbackToken":"HTOK","uiActions":{"hideEnclosingContainer":true}}}}},
            {"menuServiceItemRenderer":{"text":{"runs":[{"text":"Save to Watch later"}]},"serviceEndpoint":{"playlistEditEndpoint":{"playlistId":"WL","actions":[{"action":"ACTION_ADD_VIDEO","addedVideoId":"vid"}]}}}},
            {"menuNavigationItemRenderer":{"text":{"runs":[{"text":"Save to playlist"}]},"navigationEndpoint":{"addToPlaylistEndpoint":{"videoId":"vid"}}}},
            {"menuNavigationItemRenderer":{"text":{"runs":[{"text":"Go to channel"}]},"navigationEndpoint":{"browseEndpoint":{"browseId":"UCx"}}}}
          ]}}}}}})
    }

    #[test]
    fn tile_menu() {
        let a = menu_actions(&history_tile());
        let kinds: Vec<_> = a.iter().map(|a| a.kind).collect();
        assert_eq!(
            kinds,
            [MenuKind::Play, MenuKind::RemoveFromHistory, MenuKind::WatchLater, MenuKind::SaveToPlaylist, MenuKind::GoToChannel]
        );
        assert_eq!(a[0].command, MenuCommand::Watch { video_id: "vid".into(), start_secs: Some(22) });
        assert_eq!(a[1].token(), Some("HTOK"));
        assert_eq!(a[1].label, "Remove from watch history");
        let m = menu_map(&json!({"items":[history_tile(), {"tileRenderer":{"contentId":"PLx","contentType":"TILE_CONTENT_TYPE_PLAYLIST"}}]}));
        assert_eq!(m.len(), 1);
        assert_eq!(m["vid"].len(), 5);
    }

    #[test]
    fn lazy_panel_menu() {
        let tile = json!({"contentId":"v2","onLongPressCommand":{"showEngagementPanelEndpoint":{
            "identifier":{"tag":"PAcontext_menu"},"globalConfiguration":{"params":"P=="}}}});
        let a = menu_actions(&tile);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].kind, MenuKind::More);
        assert_eq!(a[0].command, MenuCommand::Panel { panel_id: "PAcontext_menu".into(), params: "P==".into() });

        let tap = |cmd: Value, title: &str| json!({"listItemViewModel":{"title":{"content":title},"rendererContext":{"commandContext":{"onTap":{"innertubeCommand":cmd}}}}});
        let panel = json!({"content":{"engagementPanelSectionListRenderer":{"content":{"listViewModel":{"listItems":[
            tap(json!({"commandExecutorCommand":{"commands":[{"signalServiceEndpoint":{"signal":"CLIENT_SIGNAL"}},{"watchEndpoint":{"videoId":"v2"}}]}}), "Play"),
            tap(json!({"commandExecutorCommand":{"commands":[{"signalServiceEndpoint":{}},{"playlistEditEndpoint":{"playlistId":"WL","actions":[{"action":"ACTION_ADD_VIDEO","addedVideoId":"v2"}]}}]}}), "Save to Watch later"),
            tap(json!({"openClientOverlayAction":{"context":"v2","type":"CLIENT_OVERLAY_TYPE_ADD_TO_PLAYLIST"}}), "Save to playlist"),
            tap(json!({"feedbackEndpoint":{"feedbackToken":"NI","actions":[{"openClientOverlayAction":{"type":"CLIENT_OVERLAY_TYPE_DISMISSAL_FOLLOW_UP"}}]}}), "Not interested"),
            tap(json!({"openPopupAction":{"popup":{"overlaySectionRenderer":{"overlay":{"items":[{"compactLinkRenderer":{"serviceEndpoint":{"commandExecutorCommand":{"commands":[
                {"feedbackEndpoint":{"feedbackToken":"DRC","actions":[{"removeItemAction":{"childId":"v2"}}]}},{"signalAction":{"signal":"CLOSE_POPUP"}}]}}}}]}}}}}), "Don't recommend channel")
        ]}}}}});
        let a = parse_panel_menu(&panel);
        let kinds: Vec<_> = a.iter().map(|a| a.kind).collect();
        assert_eq!(
            kinds,
            [MenuKind::Play, MenuKind::WatchLater, MenuKind::SaveToPlaylist, MenuKind::NotInterested, MenuKind::DontRecommendChannel]
        );
        assert_eq!(a[3].token(), Some("NI"));
        assert_eq!(a[4].token(), Some("DRC"));
        assert_eq!(a[2].command, MenuCommand::AddToPlaylist { video_id: "v2".into() });
    }

    fn private(name: &str) -> Option<Value> {
        let p = format!("{}/../../fixtures/private/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(p).ok().map(|s| serde_json::from_str(&s).unwrap())
    }

    /// Parsers against real (signed-in, uncommitted) responses when they are present.
    #[test]
    fn private_fixtures() {
        if let Some(h) = private("history.json") {
            let m = menu_map(&h);
            assert!(!m.is_empty());
            assert!(m.values().all(|a| a.iter().any(|a| a.kind == MenuKind::RemoveFromHistory && a.token().is_some())));
        }
        if let Some(h) = private("home.json") {
            let m = menu_map(&h);
            assert!(m.values().any(|a| a.iter().any(|a| a.kind == MenuKind::More)));
        }
        if let Some(p) = private("panel.json") {
            let a = parse_panel_menu(&p);
            assert!(a.iter().any(|a| a.kind == MenuKind::NotInterested && a.token().is_some()));
            assert!(a.iter().any(|a| a.kind == MenuKind::DontRecommendChannel && a.token().is_some()));
            assert!(a.iter().any(|a| a.kind == MenuKind::WatchLater));
        }
        if let Some(p) = private("add_to_pl.json") {
            let p = parse_add_to_playlist(&p);
            assert_eq!(p[0].playlist_id, "WL");
        }
        if let Some(c) = private("channel.json") {
            let s = subscribe_info(&c).unwrap();
            assert!(s.subscribe_params.is_some() && s.unsubscribe_params.is_some());
            assert_eq!(s.notifications.len(), 3);
        }
        if let Some(n) = private("next.json") {
            assert!(like_info(&n).is_some());
        }
    }

    // ---------- live, reversible tests on the real account (run with --ignored) ----------

    const TEST_VIDEO: &str = "jNQXAC9IVRw"; // "Me at the zoo"
    /// jawed: a channel the test account is not subscribed to (checked at runtime).
    const TEST_CHANNEL: &str = "UC4QobU6STFB0P71PMvOGN5A";

    fn live() -> Session {
        crate::accounts::live_data_dir();
        let s = Session::new().unwrap();
        assert!(s.signed_in());
        s
    }

    /// Reads are eventually consistent: poll `f` (2 s apart) until it returns `want`.
    fn eventually<T: PartialEq + std::fmt::Debug>(mut f: impl FnMut() -> T, want: T) -> T {
        for _ in 0..10 {
            let v = f();
            if v == want {
                return v;
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        f()
    }

    fn rating(s: &mut Session) -> Rating {
        s.video_state(TEST_VIDEO).unwrap().0.unwrap().rating
    }

    #[test]
    #[ignore]
    fn live_like_cycle() {
        let mut s = live();
        let before = rating(&mut s);
        if before != Rating::None {
            eprintln!("skipping: test video already rated {before:?}");
            return;
        }
        s.like(TEST_VIDEO).unwrap();
        let liked = eventually(|| rating(&mut s), Rating::Like);
        s.dislike(TEST_VIDEO).unwrap();
        let disliked = eventually(|| rating(&mut s), Rating::Dislike);
        s.remove_like(TEST_VIDEO).unwrap();
        let after = eventually(|| rating(&mut s), Rating::None);
        assert_eq!((liked, disliked, after), (Rating::Like, Rating::Dislike, Rating::None));
    }

    #[test]
    #[ignore]
    fn live_subscribe_cycle() {
        let mut s = live();
        let info = s.subscribe_info(TEST_CHANNEL).unwrap().unwrap();
        if info.subscribed {
            eprintln!("skipping: already subscribed to the test channel");
            return;
        }
        s.subscribe(TEST_CHANNEL, info.subscribe_params.as_deref()).unwrap();
        let subbed = |s: &mut Session| s.subscribe_info(TEST_CHANNEL).unwrap().unwrap();
        let on = eventually(|| subbed(&mut s).subscribed, true);
        // Bell: pick "None", then unsubscribe (which drops the preference too).
        let info2 = subbed(&mut s);
        let off = info2.notifications.iter().find(|n| n.state.ends_with("_OFF")).cloned();
        let bell = off.map(|n| s.set_notifications(&n.params));
        s.unsubscribe(TEST_CHANNEL, info2.unsubscribe_params.as_deref().or(info.unsubscribe_params.as_deref())).unwrap();
        let after = eventually(|| subbed(&mut s).subscribed, false);
        assert!(on, "subscribe did not take");
        assert!(!after, "unsubscribe did not take");
        if let Some(b) = bell {
            b.unwrap();
        }
    }

    fn contains(s: &mut Session, playlist_id: &str) -> Option<bool> {
        s.playlists_for(TEST_VIDEO).unwrap().iter().find(|p| p.playlist_id == playlist_id).map(|p| p.contains)
    }

    #[test]
    #[ignore]
    fn live_playlist_cycle() {
        let mut s = live();
        let lists = s.my_playlists().unwrap();
        assert_eq!(lists.first().map(|p| p.playlist_id.as_str()), Some(WATCH_LATER));
        // Clean up a leftover from an aborted run first.
        for p in lists.iter().filter(|p| p.title == "ytnative-test") {
            s.delete_playlist(&p.playlist_id).unwrap();
        }
        let id = s.create_playlist("ytnative-test", Privacy::Private, &[TEST_VIDEO]).unwrap();
        let listed = eventually(|| contains(&mut s, &id), Some(true));
        let privacy = s.playlists_for(TEST_VIDEO).unwrap().into_iter().find(|p| p.playlist_id == id).and_then(|p| p.privacy);
        s.remove_from_playlist(&id, TEST_VIDEO).unwrap();
        let removed = eventually(|| contains(&mut s, &id), Some(false));
        s.add_to_playlist(&id, TEST_VIDEO).unwrap();
        let added = eventually(|| contains(&mut s, &id), Some(true));
        s.delete_playlist(&id).unwrap();
        let gone = eventually(|| contains(&mut s, &id), None);
        assert_eq!((listed, removed, added, gone), (Some(true), Some(false), Some(true), None));
        assert_eq!(privacy.as_deref(), Some("PRIVATE"));
    }

    #[test]
    #[ignore]
    fn live_watch_later_cycle() {
        let mut s = live();
        if contains(&mut s, WATCH_LATER) != Some(false) {
            eprintln!("skipping: test video already in Watch Later");
            return;
        }
        s.watch_later_add(TEST_VIDEO).unwrap();
        let added = eventually(|| contains(&mut s, WATCH_LATER), Some(true));
        s.watch_later_remove(TEST_VIDEO).unwrap();
        let removed = eventually(|| contains(&mut s, WATCH_LATER), Some(false));
        assert_eq!((added, removed), (Some(true), Some(false)));
    }

    /// A public playlist the account has not saved (checked at runtime): YouTube's
    /// "Popular Music Videos".
    const TEST_PLAYLIST: &str = "PLFgquLnL59alCl_2TQvOiD5Vgm1hCaGSI";

    fn library_has(s: &mut Session, playlist_id: &str) -> bool {
        let r = s.raw("browse", json!({ "browseId": crate::session::feeds::PLAYLISTS })).unwrap();
        r.to_string().contains(playlist_id)
    }

    #[test]
    #[ignore]
    fn live_save_foreign_playlist_cycle() {
        let mut s = live();
        if library_has(&mut s, TEST_PLAYLIST) {
            eprintln!("skipping: test playlist already saved");
            return;
        }
        s.save_playlist(TEST_PLAYLIST).unwrap();
        let saved = eventually(|| library_has(&mut s, TEST_PLAYLIST), true);
        s.unsave_playlist(TEST_PLAYLIST).unwrap();
        let after = eventually(|| library_has(&mut s, TEST_PLAYLIST), false);
        assert!(!after, "unsave did not take");
        assert!(saved, "save did not show up in the library");
    }

    #[test]
    #[ignore]
    fn live_home_menu_panel() {
        let mut s = live();
        let home = s.raw("browse", json!({ "browseId": "FEwhat_to_watch" })).unwrap();
        let m = menu_map(&home);
        let (_, acts) = m.iter().find(|(_, a)| a.iter().any(|a| a.kind == MenuKind::More)).expect("lazy menu");
        let MenuCommand::Panel { panel_id, params } = &acts.iter().find(|a| a.kind == MenuKind::More).unwrap().command else { unreachable!() };
        let items = s.menu_panel(panel_id, params).unwrap();
        assert!(items.iter().any(|a| a.kind == MenuKind::NotInterested), "{items:?}");
        // Read-only: the feedback tokens are not sent.
    }
}

//! Lounge messages: commands a phone sends to the screen ([`Command`]) and state updates the screen
//! sends back ([`Event`]). All payload values travel as strings; times are in seconds.

use super::chunk::Message;
use serde_json::Value;

/// A phone / remote taking part in the lounge.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// e.g. `REMOTE_CONTROL`, `LOUNGE_SCREEN`.
    pub device_type: String,
    pub app: String,
    /// `manual` (TV code), `cast`, `dial`, ...
    pub pairing_type: String,
    pub client_name: String,
    pub user: String,
}

impl Device {
    fn from_value(v: &Value) -> Device {
        Device {
            id: s(v, "id").unwrap_or_default(),
            name: s(v, "name").unwrap_or_default(),
            device_type: s(v, "type").unwrap_or_default(),
            app: s(v, "app").unwrap_or_default(),
            pairing_type: s(v, "pairingType").unwrap_or_default(),
            client_name: s(v, "clientName").unwrap_or_default(),
            user: s(v, "user").unwrap_or_default(),
        }
    }

    pub fn is_remote(&self) -> bool {
        self.device_type == "REMOTE_CONTROL"
    }
}

/// `setPlaylist` / `updatePlaylist` / `setVideo` payload.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Playlist {
    /// The video to play now (may be empty for `updatePlaylist`).
    pub video_id: String,
    /// Whole queue (comma-separated `videoIds` on the wire); may be empty when only `listId` is given.
    pub video_ids: Vec<String>,
    pub current_index: Option<i64>,
    /// Start position, seconds.
    pub current_time: Option<f64>,
    pub list_id: Option<String>,
    /// Opaque `params` (player params of the sender, e.g. for playlists/mixes).
    pub params: Option<String>,
    /// Click-tracking token; echoed back in `nowPlaying`.
    pub ctt: Option<String>,
    pub audio_only: bool,
}

impl Playlist {
    fn from_value(v: &Value) -> Playlist {
        Playlist {
            video_id: s(v, "videoId").unwrap_or_default(),
            video_ids: s(v, "videoIds")
                .map(|ids| ids.split(',').map(str::trim).filter(|i| !i.is_empty()).map(String::from).collect())
                .unwrap_or_default(),
            current_index: f(v, "currentIndex").map(|x| x as i64),
            current_time: f(v, "currentTime"),
            list_id: s(v, "listId").filter(|x| !x.is_empty()),
            params: s(v, "params").filter(|x| !x.is_empty()),
            ctt: s(v, "ctt").filter(|x| !x.is_empty()),
            audio_only: s(v, "audioOnly").as_deref() == Some("true"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpadKey {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Back,
}

/// Commands from the phone, delivered to the app through `Sender<Command>`.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    RemoteConnected(Device),
    RemoteDisconnected(Device),
    /// Sent at bind time and when the set of devices changes.
    LoungeStatus { devices: Vec<Device>, queue_id: Option<String> },
    /// Replace the queue and start playing `video_id` at `current_time`.
    SetPlaylist(Playlist),
    /// Queue changed without interrupting playback.
    UpdatePlaylist(Playlist),
    /// Jump to `video_id` within the current queue.
    SetVideo { video_id: String, current_time: Option<f64> },
    Play,
    Pause,
    /// Seconds.
    SeekTo(f64),
    /// `volume` is absolute 0..=100; `delta` a relative step from phone volume keys.
    SetVolume { volume: Option<u32>, delta: Option<i32>, muted: Option<bool> },
    GetVolume,
    Next,
    Previous,
    StopVideo,
    /// Answer with [`Event::NowPlaying`].
    GetNowPlaying,
    GetPlaylist,
    GetSubtitlesTrack,
    /// `language_code == None` means captions off.
    SetSubtitlesTrack { video_id: String, language_code: Option<String>, vss_id: Option<String> },
    /// `ENABLED` / `DISABLED`.
    SetAutoplayMode(String),
    Dpad(DpadKey),
    /// Voice search started (`true`) or cancelled (`false`).
    Voice { started: bool },
    /// Anything else (name + raw payload), so the app can log/handle it.
    Unknown { name: String, payload: Value },
}

/// Browserchannel-internal messages that never reach the app.
pub(crate) fn is_internal(name: &str) -> bool {
    matches!(name, "c" | "S" | "noop" | "getDiscoveryDeviceId")
}

impl Command {
    /// Converts one channel message to a command. `None` for internal messages (`c`, `S`, `noop`).
    pub fn from_message(m: &Message) -> Option<Command> {
        if is_internal(&m.name) {
            return None;
        }
        let p = m.payload();
        Some(match m.name.as_str() {
            "remoteConnected" => Command::RemoteConnected(Device::from_value(p)),
            "remoteDisconnected" => Command::RemoteDisconnected(Device::from_value(p)),
            "loungeStatus" => {
                let devices = s(p, "devices")
                    .and_then(|d| serde_json::from_str::<Value>(&d).ok())
                    .and_then(|d| d.as_array().map(|a| a.iter().map(Device::from_value).collect()))
                    .unwrap_or_default();
                Command::LoungeStatus { devices, queue_id: s(p, "queueId") }
            }
            "setPlaylist" => Command::SetPlaylist(Playlist::from_value(p)),
            "updatePlaylist" => Command::UpdatePlaylist(Playlist::from_value(p)),
            "setVideo" => Command::SetVideo { video_id: s(p, "videoId").unwrap_or_default(), current_time: f(p, "currentTime") },
            "play" => Command::Play,
            "pause" => Command::Pause,
            "seekTo" => Command::SeekTo(f(p, "newTime").unwrap_or(0.0)),
            "setVolume" => Command::SetVolume {
                volume: f(p, "volume").map(|v| v.clamp(0.0, 100.0) as u32),
                delta: f(p, "delta").map(|d| d as i32),
                muted: s(p, "muted").map(|m| m == "true"),
            },
            "getVolume" => Command::GetVolume,
            "next" => Command::Next,
            "previous" => Command::Previous,
            "stopVideo" => Command::StopVideo,
            "getNowPlaying" => Command::GetNowPlaying,
            "getPlaylist" => Command::GetPlaylist,
            "getSubtitlesTrack" => Command::GetSubtitlesTrack,
            "setSubtitlesTrack" => Command::SetSubtitlesTrack {
                video_id: s(p, "videoId").unwrap_or_default(),
                language_code: s(p, "languageCode").filter(|x| !x.is_empty()),
                vss_id: s(p, "vssId").or_else(|| s(p, "vss_id")).filter(|x| !x.is_empty()),
            },
            "setAutoplayMode" => Command::SetAutoplayMode(s(p, "autoplayMode").unwrap_or_default()),
            "dpadCommand" => match s(p, "key").as_deref() {
                Some("UP") => Command::Dpad(DpadKey::Up),
                Some("DOWN") => Command::Dpad(DpadKey::Down),
                Some("LEFT") => Command::Dpad(DpadKey::Left),
                Some("RIGHT") => Command::Dpad(DpadKey::Right),
                Some("ENTER") => Command::Dpad(DpadKey::Enter),
                Some("BACK") => Command::Dpad(DpadKey::Back),
                _ => Command::Unknown { name: m.name.clone(), payload: p.clone() },
            },
            "voiceCommand" => Command::Voice { started: s(p, "status").as_deref() == Some("INITIATED") },
            _ => Command::Unknown { name: m.name.clone(), payload: p.clone() },
        })
    }
}

/// Player state as the lounge knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerState {
    Unstarted = -1,
    Ended = 0,
    Playing = 1,
    Paused = 2,
    Buffering = 3,
}

/// Position report shared by `nowPlaying` and `onStateChange`. Times in seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub state: PlayerState,
    pub current_time: f64,
    /// 0 for live streams (send `current_time == duration` there, as SmartTube does).
    pub duration: f64,
    /// Buffered up to, seconds.
    pub loaded_time: f64,
    /// Client playback nonce of the current playback, if known.
    pub cpn: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NowPlaying {
    pub video_id: String,
    pub status: Status,
    /// Defaults to the `listId` / `currentIndex` of the last `setPlaylist`.
    pub list_id: Option<String>,
    pub current_index: Option<i64>,
}

/// State updates the app sends to the phones through `Sender<Event>`.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// `None` = nothing playing.
    NowPlaying(Option<NowPlaying>),
    StateChange(Status),
    /// 0..=100.
    VolumeChanged { volume: u32, muted: bool },
    /// `ENABLED` / `DISABLED` / `UNSUPPORTED`.
    AutoplayModeChanged(String),
    HasPreviousNextChanged { has_previous: bool, has_next: bool },
    /// `language_code == None` = captions off.
    SubtitlesTrackChanged { video_id: String, language_code: Option<String>, vss_id: Option<String> },
    /// Escape hatch: any other event with its (unprefixed) fields.
    Raw { name: String, fields: Vec<(String, String)> },
}

/// Remembered from the last `setPlaylist`, echoed in `nowPlaying` like the official receivers do.
#[derive(Debug, Clone, Default)]
pub(crate) struct PlaylistContext {
    pub ctt: Option<String>,
    pub list_id: Option<String>,
    pub current_index: Option<i64>,
}

impl PlaylistContext {
    pub fn update(&mut self, c: &Command) {
        if let Command::SetPlaylist(p) | Command::UpdatePlaylist(p) = c {
            if p.ctt.is_some() {
                self.ctt = p.ctt.clone();
            }
            if p.list_id.is_some() {
                self.list_id = p.list_id.clone();
            }
            if p.current_index.is_some() {
                self.current_index = p.current_index;
            }
        }
    }
}

const CPN_NONE: &str = "foo";

fn secs(x: f64) -> String {
    let x = if x.is_finite() && x > 0.0 { x } else { 0.0 };
    let t = format!("{x:.3}");
    let t = t.trim_end_matches('0').trim_end_matches('.');
    t.to_string()
}

fn status_fields(st: &Status, out: &mut Vec<(String, String)>) {
    out.push(("state".into(), (st.state as i32).to_string()));
    out.push(("currentTime".into(), secs(st.current_time)));
    out.push(("duration".into(), secs(st.duration)));
    out.push(("loadedTime".into(), secs(st.loaded_time)));
    out.push(("seekableStartTime".into(), "0".into()));
    out.push(("seekableEndTime".into(), secs(st.duration)));
    out.push(("cpn".into(), st.cpn.clone().unwrap_or_else(|| CPN_NONE.into())));
}

impl Event {
    /// Wire name and unprefixed fields of this event.
    pub(crate) fn fields(&self, ctx: &PlaylistContext) -> (String, Vec<(String, String)>) {
        let mut o: Vec<(String, String)> = Vec::new();
        let name = match self {
            Event::NowPlaying(None) => "nowPlaying",
            Event::NowPlaying(Some(np)) => {
                o.push(("videoId".into(), np.video_id.clone()));
                status_fields(&np.status, &mut o);
                if let Some(l) = np.list_id.clone().or_else(|| ctx.list_id.clone()) {
                    o.push(("listId".into(), l));
                }
                if let Some(i) = np.current_index.or(ctx.current_index) {
                    o.push(("currentIndex".into(), i.to_string()));
                }
                if let Some(c) = &ctx.ctt {
                    o.push(("ctt".into(), c.clone()));
                }
                "nowPlaying"
            }
            Event::StateChange(st) => {
                status_fields(st, &mut o);
                "onStateChange"
            }
            Event::VolumeChanged { volume, muted } => {
                o.push(("volume".into(), (*volume).min(100).to_string()));
                o.push(("muted".into(), muted.to_string()));
                "onVolumeChanged"
            }
            Event::AutoplayModeChanged(mode) => {
                o.push(("autoplayMode".into(), mode.clone()));
                "onAutoplayModeChanged"
            }
            Event::HasPreviousNextChanged { has_previous, has_next } => {
                o.push(("hasPrevious".into(), has_previous.to_string()));
                o.push(("hasNext".into(), has_next.to_string()));
                "onHasPreviousNextChanged"
            }
            Event::SubtitlesTrackChanged { video_id, language_code, vss_id } => {
                o.push(("videoId".into(), video_id.clone()));
                if let Some(v) = vss_id {
                    o.push(("vss_id".into(), v.clone()));
                }
                if let Some(l) = language_code {
                    o.push(("languageCode".into(), l.clone()));
                    o.push(("sourceLanguageCode".into(), l.clone()));
                }
                "onSubtitlesTrackChanged"
            }
            Event::Raw { name, fields } => {
                o.extend(fields.iter().cloned());
                return (name.clone(), o);
            }
        };
        (name.to_string(), o)
    }
}

/// Form body of one outgoing POST: `count`, `ofs`, `reqN__sc` (double underscore) and `reqN_<field>`.
pub(crate) fn encode_batch(events: &[Event], ofs: u64, ctx: &PlaylistContext) -> Vec<(String, String)> {
    let mut out = vec![("count".to_string(), events.len().to_string()), ("ofs".to_string(), ofs.to_string())];
    for (i, e) in events.iter().enumerate() {
        let (name, fields) = e.fields(ctx);
        out.push((format!("req{i}__sc"), name));
        for (k, v) in fields {
            out.push((format!("req{i}_{k}"), v));
        }
    }
    out
}

fn s(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(x) => Some(x.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn f(v: &Value, key: &str) -> Option<f64> {
    match v.get(key)? {
        Value::String(x) => x.trim().parse().ok(),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::chunk::parse_all;
    use super::*;

    fn cmds(raw: &str) -> Vec<Command> {
        parse_all(raw.as_bytes()).iter().filter_map(Command::from_message).collect()
    }

    #[test]
    fn set_playlist_from_phone() {
        let raw = r#"300
[[5,["remoteConnected",{"app":"android-phone-19.01.35","pairingType":"manual","capabilities":"que,dsdtr,atp","clientName":"android","experiments":"","name":"Pixel 7","remoteControllerUrl":"ws://x","id":"abc-123","type":"REMOTE_CONTROL","user":"Someone","userAvatarUri":""}]]
,[6,["setPlaylist",{"videoId":"dQw4w9WgXcQ","currentTime":"12.5","ctt":"TOKEN","currentIndex":"1","listId":"RDdQw4w9WgXcQ","videoIds":"aaaaaaaaaaa,dQw4w9WgXcQ","params":"OAI%3D","audioOnly":"false"}]]
,[7,["noop"]]
]
"#;
        let c = cmds(raw);
        assert_eq!(c.len(), 2);
        match &c[0] {
            Command::RemoteConnected(d) => {
                assert_eq!(d.name, "Pixel 7");
                assert_eq!(d.id, "abc-123");
                assert_eq!(d.pairing_type, "manual");
                assert!(d.is_remote());
            }
            o => panic!("{o:?}"),
        }
        match &c[1] {
            Command::SetPlaylist(p) => {
                assert_eq!(p.video_id, "dQw4w9WgXcQ");
                assert_eq!(p.video_ids, ["aaaaaaaaaaa", "dQw4w9WgXcQ"]);
                assert_eq!(p.current_index, Some(1));
                assert_eq!(p.current_time, Some(12.5));
                assert_eq!(p.list_id.as_deref(), Some("RDdQw4w9WgXcQ"));
                assert_eq!(p.params.as_deref(), Some("OAI%3D"));
                assert_eq!(p.ctt.as_deref(), Some("TOKEN"));
                assert!(!p.audio_only);
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn simple_commands() {
        let raw = r#"999
[[1,["play"]],[2,["pause"]],[3,["seekTo",{"newTime":"95.5"}]],[4,["setVolume",{"volume":"40","muted":"false"}]],
[5,["setVolume",{"delta":"-5"}]],[6,["next"]],[7,["previous"]],[8,["stopVideo"]],[9,["getNowPlaying"]],
[10,["getVolume"]],[11,["dpadCommand",{"key":"LEFT"}]],[12,["voiceCommand",{"status":"INITIATED"}]],
[13,["voiceCommand",{"status":"CANCELED"}]],[14,["setSubtitlesTrack",{"videoId":"v","languageCode":"en"}]],
[15,["setSubtitlesTrack",{"videoId":"v"}]],[16,["setAutoplayMode",{"autoplayMode":"ENABLED"}]],
[17,["remoteDisconnected",{"id":"abc","name":"Pixel"}]],[18,["somethingNew",{"a":"b"}]],[19,["c","SID","",8]]]
"#;
        let c = cmds(raw);
        assert_eq!(
            c,
            vec![
                Command::Play,
                Command::Pause,
                Command::SeekTo(95.5),
                Command::SetVolume { volume: Some(40), delta: None, muted: Some(false) },
                Command::SetVolume { volume: None, delta: Some(-5), muted: None },
                Command::Next,
                Command::Previous,
                Command::StopVideo,
                Command::GetNowPlaying,
                Command::GetVolume,
                Command::Dpad(DpadKey::Left),
                Command::Voice { started: true },
                Command::Voice { started: false },
                Command::SetSubtitlesTrack { video_id: "v".into(), language_code: Some("en".into()), vss_id: None },
                Command::SetSubtitlesTrack { video_id: "v".into(), language_code: None, vss_id: None },
                Command::SetAutoplayMode("ENABLED".into()),
                Command::RemoteDisconnected(Device { id: "abc".into(), name: "Pixel".into(), ..Default::default() }),
                Command::Unknown { name: "somethingNew".into(), payload: serde_json::json!({"a":"b"}) },
            ]
        );
    }

    #[test]
    fn lounge_status_devices() {
        let raw = r#"1
[[2,["loungeStatus",{"queueId":"RQ1","devices":"[{\"app\":\"lb-v4\",\"name\":\"TV\",\"id\":\"s1\",\"type\":\"LOUNGE_SCREEN\"},{\"name\":\"Phone\",\"id\":\"p1\",\"type\":\"REMOTE_CONTROL\",\"pairingType\":\"manual\"}]"}]]]"#;
        match &cmds(raw)[0] {
            Command::LoungeStatus { devices, queue_id } => {
                assert_eq!(queue_id.as_deref(), Some("RQ1"));
                assert_eq!(devices.len(), 2);
                assert!(!devices[0].is_remote());
                assert_eq!(devices[1].name, "Phone");
                assert!(devices[1].is_remote());
            }
            o => panic!("{o:?}"),
        }
    }

    fn get<'a>(f: &'a [(String, String)], k: &str) -> Option<&'a str> {
        f.iter().find(|(a, _)| a == k).map(|(_, b)| b.as_str())
    }

    #[test]
    fn encodes_batch() {
        let mut ctx = PlaylistContext::default();
        ctx.update(&Command::SetPlaylist(Playlist {
            ctt: Some("CTT".into()),
            list_id: Some("PL1".into()),
            current_index: Some(2),
            ..Default::default()
        }));
        let st = Status { state: PlayerState::Playing, current_time: 12.5, duration: 330.0, loaded_time: 20.25, cpn: None };
        let events = [
            Event::NowPlaying(Some(NowPlaying { video_id: "vid".into(), status: st.clone(), list_id: None, current_index: None })),
            Event::StateChange(Status { state: PlayerState::Paused, ..st }),
            Event::VolumeChanged { volume: 55, muted: false },
            Event::HasPreviousNextChanged { has_previous: false, has_next: true },
            Event::SubtitlesTrackChanged { video_id: "vid".into(), language_code: Some("en".into()), vss_id: Some(".en".into()) },
            Event::AutoplayModeChanged("DISABLED".into()),
            Event::NowPlaying(None),
        ];
        let f = encode_batch(&events, 7, &ctx);
        assert_eq!(get(&f, "count"), Some("7"));
        assert_eq!(get(&f, "ofs"), Some("7"));
        assert_eq!(get(&f, "req0__sc"), Some("nowPlaying"));
        assert_eq!(get(&f, "req0_videoId"), Some("vid"));
        assert_eq!(get(&f, "req0_state"), Some("1"));
        assert_eq!(get(&f, "req0_currentTime"), Some("12.5"));
        assert_eq!(get(&f, "req0_duration"), Some("330"));
        assert_eq!(get(&f, "req0_loadedTime"), Some("20.25"));
        assert_eq!(get(&f, "req0_seekableEndTime"), Some("330"));
        assert_eq!(get(&f, "req0_listId"), Some("PL1"));
        assert_eq!(get(&f, "req0_currentIndex"), Some("2"));
        assert_eq!(get(&f, "req0_ctt"), Some("CTT"));
        assert_eq!(get(&f, "req0_cpn"), Some("foo"));
        assert_eq!(get(&f, "req1__sc"), Some("onStateChange"));
        assert_eq!(get(&f, "req1_state"), Some("2"));
        assert_eq!(get(&f, "req2__sc"), Some("onVolumeChanged"));
        assert_eq!(get(&f, "req2_volume"), Some("55"));
        assert_eq!(get(&f, "req2_muted"), Some("false"));
        assert_eq!(get(&f, "req3_hasNext"), Some("true"));
        assert_eq!(get(&f, "req4_vss_id"), Some(".en"));
        assert_eq!(get(&f, "req4_languageCode"), Some("en"));
        assert_eq!(get(&f, "req5_autoplayMode"), Some("DISABLED"));
        assert_eq!(get(&f, "req6__sc"), Some("nowPlaying"));
        assert!(!f.iter().any(|(k, _)| k.starts_with("req6_") && k != "req6__sc"));
    }

    #[test]
    fn state_values() {
        assert_eq!(PlayerState::Unstarted as i32, -1);
        assert_eq!(PlayerState::Ended as i32, 0);
        assert_eq!(PlayerState::Buffering as i32, 3);
        assert_eq!(secs(-3.0), "0");
        assert_eq!(secs(1.0 / 3.0), "0.333");
    }
}

//! Minimal DIAL server (<http://www.dial-multiscreen.org>), a port of SmartTube's `DialServer`: makes the TV
//! show up in the cast menu of YouTube apps on the same network.
//!
//! The phone sends an SSDP `M-SEARCH` for `urn:dial-multiscreen-org:service:dial:1`; we answer with the URL of
//! our device description, whose `Application-URL` header points at `/apps/`. The phone reads
//! `GET /apps/YouTube` and launches it with `POST /apps/YouTube` and a body such as
//! `pairingCode=<uuid>&theme=cl&v=<videoId>&t=<seconds>`. The app passes that pairing code to
//! [`LoungeScreen::register_pairing_code`](super::LoungeScreen::register_pairing_code), after which the phone
//! talks to us over the lounge session that [`LoungeScreen::run`](super::LoungeScreen::run) keeps open.
//!
//! **UNTESTED on webOS.** The app sandbox may block multicast or binding port 1900, and webOS runs its own DIAL
//! server (for the built-in YouTube app) that may already own the SSDP port; then [`start`] fails for SSDP,
//! or two TVs appear in the phone's list. Ports are configurable.

use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const SSDP_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
pub const DIAL_ST: &str = "urn:dial-multiscreen-org:service:dial:1";
const APP_NAME: &str = "YouTube";
const MAX_REQUEST: usize = 32 * 1024;

#[derive(Debug, Clone)]
pub struct DialConfig {
    /// Shown in the phone's cast list.
    pub friendly_name: String,
    /// Stable unique id (e.g. the lounge `device_id`), used for `UDN`/`USN`.
    pub udn: String,
    /// Lounge screen id, advertised in `<additionalData>` so senders that skip the pairing code
    /// (ytcast-style) can find the screen directly.
    pub screen_id: Option<String>,
    /// HTTP port of the device description / app resource (SmartTube uses 8090). 0 = any free port.
    pub http_port: u16,
    /// SSDP port (1900). `None` disables the SSDP responder (HTTP only, useful for tests).
    pub ssdp_port: Option<u16>,
}

impl DialConfig {
    pub fn new(friendly_name: &str, udn: &str) -> Self {
        DialConfig { friendly_name: friendly_name.into(), udn: udn.into(), screen_id: None, http_port: 8090, ssdp_port: Some(1900) }
    }
}

/// A `POST /apps/YouTube` from a phone.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LaunchRequest {
    /// Pass to `LoungeScreen::register_pairing_code`.
    pub pairing_code: Option<String>,
    pub theme: Option<String>,
    /// Video to start (`v`), if the phone was already playing one.
    pub video_id: Option<String>,
    /// Start time (`t`), seconds.
    pub start_time: Option<f64>,
    /// All decoded body fields.
    pub fields: Vec<(String, String)>,
}

/// Running DIAL server; stops on [`stop`](Self::stop) or drop.
pub struct DialServer {
    stop: Arc<AtomicBool>,
    http_port: u16,
}

impl DialServer {
    pub fn http_port(&self) -> u16 {
        self.http_port
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for DialServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Starts the HTTP server and (if configured) the SSDP responder, each on its own thread.
/// `on_launch` runs on the HTTP thread after the `201 Created` answer was sent.
pub fn start(cfg: DialConfig, on_launch: impl Fn(LaunchRequest) + Send + Sync + 'static) -> Result<DialServer> {
    let stop = Arc::new(AtomicBool::new(false));
    let listener = TcpListener::bind(("0.0.0.0", cfg.http_port)).with_context(|| format!("DIAL: bind TCP {}", cfg.http_port))?;
    listener.set_nonblocking(true)?;
    let http_port = listener.local_addr()?.port();

    let ssdp = match cfg.ssdp_port {
        Some(port) => {
            let sock = UdpSocket::bind(("0.0.0.0", port)).with_context(|| format!("DIAL: bind UDP {port} (SSDP port taken?)"))?;
            sock.join_multicast_v4(&SSDP_ADDR, &Ipv4Addr::UNSPECIFIED).context("DIAL: join SSDP multicast group")?;
            sock.set_read_timeout(Some(Duration::from_millis(500)))?;
            Some(sock)
        }
        None => None,
    };

    let state = Arc::new(Shared { cfg, http_port, app_running: AtomicBool::new(false) });
    {
        let (stop, state) = (stop.clone(), state.clone());
        let on_launch = Arc::new(on_launch);
        std::thread::Builder::new().name("dial-http".into()).spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let (state, on_launch) = (state.clone(), on_launch.clone());
                        let _ = std::thread::Builder::new().name("dial-client".into()).spawn(move || {
                            if let Some(launch) = handle_client(stream, &state) {
                                on_launch(launch);
                            }
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(100)),
                    Err(e) => {
                        eprintln!("dial: accept failed: {e}");
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        })?;
    }
    if let Some(sock) = ssdp {
        let (stop, state) = (stop.clone(), state.clone());
        std::thread::Builder::new().name("dial-ssdp".into()).spawn(move || {
            let mut buf = [0u8; 2048];
            while !stop.load(Ordering::SeqCst) {
                let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
                let msg = String::from_utf8_lossy(&buf[..n]);
                if !is_dial_search(&msg) {
                    continue;
                }
                let Some(ip) = local_ip_towards(from) else { continue };
                let reply = ssdp_response(ip, state.http_port, &state.cfg.udn);
                if let Ok(out) = UdpSocket::bind(("0.0.0.0", 0)) {
                    let _ = out.send_to(reply.as_bytes(), from);
                }
            }
        })?;
    }
    Ok(DialServer { stop, http_port })
}

struct Shared {
    cfg: DialConfig,
    http_port: u16,
    app_running: AtomicBool,
}

fn is_dial_search(msg: &str) -> bool {
    msg.starts_with("M-SEARCH") && matches!(header(msg, "ST").as_deref(), Some(DIAL_ST) | Some("ssdp:all"))
}

fn header(msg: &str, name: &str) -> Option<String> {
    msg.split("\r\n").find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_string())
    })
}

/// The local address the OS would use to reach `peer` (no packet is sent).
fn local_ip_towards(peer: SocketAddr) -> Option<IpAddr> {
    let s = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    s.connect(peer).ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then_some(ip)
}

fn ssdp_response(ip: IpAddr, http_port: u16, udn: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nEXT:\r\nLOCATION: http://{ip}:{http_port}/dd.xml\r\n\
         SERVER: Linux/1.0 UPnP/1.0 ytnative/1.0\r\nST: {DIAL_ST}\r\nUSN: uuid:{udn}::{DIAL_ST}\r\nBOOTID.UPNP.ORG: 1\r\n\r\n"
    )
}

struct Response {
    status: &'static str,
    content_type: Option<&'static str>,
    headers: Vec<(&'static str, String)>,
    body: String,
}

/// Reads one request, answers it, returns the launch request if it was one.
fn handle_client(mut stream: TcpStream, state: &Shared) -> Option<LaunchRequest> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let base = format!("http://{}:{}", stream.local_addr().ok()?.ip(), state.http_port);
    let (method, path, body) = read_request(&mut stream)?;
    let (resp, launch) = route(&method, &path, &body, &base, state);
    let mut head = format!("HTTP/1.1 {}\r\n", resp.status);
    for (k, v) in &resp.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(ct) = resp.content_type {
        head.push_str(&format!("Content-Type: {ct}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n", resp.body.len()));
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(resp.body.as_bytes());
    let _ = stream.flush();
    launch
}

fn read_request(stream: &mut impl Read) -> Option<(String, String, String)> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let head_end = loop {
        if let Some(p) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        if data.len() > MAX_REQUEST {
            return None;
        }
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&buf[..n]);
    };
    let head = String::from_utf8_lossy(&data[..head_end]).to_string();
    let mut parts = head.lines().next()?.split_whitespace();
    let (method, path) = (parts.next()?.to_string(), parts.next()?.to_string());
    let len: usize = header(&head, "Content-Length").and_then(|v| v.parse().ok()).unwrap_or(0).min(MAX_REQUEST);
    let mut body = data[head_end + 4..].to_vec();
    while body.len() < len {
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
    }
    body.truncate(len);
    Some((method, path, String::from_utf8_lossy(&body).to_string()))
}

fn route(method: &str, path: &str, body: &str, base: &str, state: &Shared) -> (Response, Option<LaunchRequest>) {
    let p = path.split('?').next().unwrap_or("").trim_end_matches('/');
    let app_path = format!("/apps/{APP_NAME}");
    let resp = |status, content_type, headers, body| Response { status, content_type, headers, body };
    match method {
        "GET" if p == "/dd.xml" => (
            resp("200 OK", Some("text/xml; charset=utf-8"), vec![("Application-URL", format!("{base}/apps/"))], device_description(&state.cfg)),
            None,
        ),
        "GET" if p == app_path => {
            let running = state.app_running.load(Ordering::SeqCst);
            (resp("200 OK", Some("text/xml; charset=utf-8"), vec![], app_status(running, state.cfg.screen_id.as_deref())), None)
        }
        "POST" if p == app_path => {
            state.app_running.store(true, Ordering::SeqCst);
            let launch = parse_launch(body);
            (resp("201 Created", None, vec![("Location", format!("{base}{app_path}/run"))], String::new()), Some(launch))
        }
        "DELETE" if p.starts_with(&app_path) => {
            state.app_running.store(false, Ordering::SeqCst);
            (resp("200 OK", None, vec![], String::new()), None)
        }
        _ => (resp("404 Not Found", None, vec![], String::new()), None),
    }
}

fn parse_launch(body: &str) -> LaunchRequest {
    let fields: Vec<(String, String)> = body
        .trim()
        .split('&')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            Some((url_decode(k), url_decode(v)))
        })
        .filter(|(k, _)| !k.is_empty())
        .collect();
    let get = |k: &str| fields.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone()).filter(|v| !v.is_empty());
    LaunchRequest {
        pairing_code: get("pairingCode"),
        theme: get("theme"),
        video_id: get("v"),
        start_time: get("t").and_then(|t| t.parse().ok()),
        fields: fields.clone(),
    }
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = |c: u8| (c as char).to_digit(16);
                match (hex(b[i + 1]), hex(b[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn device_description(cfg: &DialConfig) -> String {
    format!(
        "<?xml version=\"1.0\"?>\n<root xmlns=\"urn:schemas-upnp-org:device-1-0\">\n  <specVersion><major>1</major><minor>0</minor></specVersion>\n  <device>\n    <deviceType>urn:dial-multiscreen-org:device:dial:1</deviceType>\n    <friendlyName>{}</friendlyName>\n    <manufacturer>ytnative</manufacturer>\n    <modelName>webOS TV</modelName>\n    <UDN>uuid:{}</UDN>\n  </device>\n</root>",
        xml(&cfg.friendly_name),
        xml(&cfg.udn)
    )
}

fn app_status(running: bool, screen_id: Option<&str>) -> String {
    let extra = match screen_id {
        Some(id) if running => format!("  <additionalData>\n    <screenId>{}</screenId>\n  </additionalData>\n", xml(id)),
        _ => String::new(),
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<service xmlns=\"urn:dial-multiscreen-org:schemas:dial\" dialVer=\"1.7\">\n  <name>{APP_NAME}</name>\n  <options allowStop=\"false\"/>\n  <state>{}</state>\n{extra}</service>",
        if running { "running" } else { "stopped" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn ssdp_search_and_reply() {
        let search = format!("M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nST: {DIAL_ST}\r\nMX: 3\r\n\r\n");
        assert!(is_dial_search(&search));
        assert!(!is_dial_search("M-SEARCH * HTTP/1.1\r\nST: urn:other\r\n\r\n"));
        assert!(!is_dial_search("NOTIFY * HTTP/1.1\r\nST: ssdp:all\r\n\r\n"));
        let r = ssdp_response("192.168.1.5".parse().unwrap(), 8090, "abc");
        assert!(r.contains("LOCATION: http://192.168.1.5:8090/dd.xml\r\n"));
        assert!(r.contains(&format!("USN: uuid:abc::{DIAL_ST}")));
    }

    #[test]
    fn launch_body() {
        let l = parse_launch("pairingCode=6c3e5b0e-1f2a-4b3c&theme=cl&v=dQw4w9WgXcQ&t=12.5&dialLaunch=watch&name=My%20Phone+X");
        assert_eq!(l.pairing_code.as_deref(), Some("6c3e5b0e-1f2a-4b3c"));
        assert_eq!(l.theme.as_deref(), Some("cl"));
        assert_eq!(l.video_id.as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(l.start_time, Some(12.5));
        assert!(l.fields.contains(&("name".into(), "My Phone X".into())));
        assert_eq!(url_decode("a%2"), "a%2");
        assert_eq!(url_decode("%41%"), "A%");
    }

    fn request(port: u16, raw: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(raw.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    #[test]
    fn http_server_on_localhost() {
        let mut cfg = DialConfig::new("Living <room>", "uuid-1");
        cfg.http_port = 0;
        cfg.ssdp_port = None;
        cfg.screen_id = Some("screen123".into());
        let (tx, rx) = mpsc::channel();
        let server = start(cfg, move |l| tx.send(l).unwrap()).unwrap();
        let port = server.http_port();

        let dd = request(port, "GET /dd.xml HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(dd.starts_with("HTTP/1.1 200 OK"));
        assert!(dd.contains(&format!("Application-URL: http://127.0.0.1:{port}/apps/")));
        assert!(dd.contains("<friendlyName>Living &lt;room&gt;</friendlyName>"));

        let st = request(port, "GET /apps/YouTube HTTP/1.1\r\n\r\n");
        assert!(st.contains("<state>stopped</state>"));
        assert!(!st.contains("screen123"));

        let body = "pairingCode=abc-123&theme=cl";
        let post = request(port, &format!("POST /apps/YouTube HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}", body.len()));
        assert!(post.starts_with("HTTP/1.1 201 Created"));
        assert!(post.contains(&format!("Location: http://127.0.0.1:{port}/apps/YouTube/run")));
        let launch = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(launch.pairing_code.as_deref(), Some("abc-123"));

        let st = request(port, "GET /apps/YouTube/ HTTP/1.1\r\n\r\n");
        assert!(st.contains("<state>running</state>"));
        assert!(st.contains("<screenId>screen123</screenId>"));

        assert!(request(port, "GET /nope HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 404"));
        server.stop();
    }
}

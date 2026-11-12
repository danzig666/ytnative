//! Windows simulator stand-in for `remote.rs`: the same key-token channel the TV build reads from
//! its FIFO, served on a localhost TCP port instead (`YTNATIVE_SIM_PORT`, default 47800), so
//! `cargo xtask sim-key` / `sim-shot` (or any script) can drive the UI.
//!
//! Protocol: connect, send whitespace-separated tokens ending with a newline, read the reply until
//! the server closes. Tokens are the TV ones (`up down left right ok enter back exit`) plus
//! `wait:<ms>` and `shot` / `shot:<name>`, which saves a presented frame as
//! `<shot dir>/<name>.png`. Tokens run in order, one key every `KEY_GAP_MS` (a key per frame
//! batch, like a person pressing a remote; OK is followed by a short settle so its press commits),
//! so `down down ok wait:1500 shot:x` does what it reads.
//! The reply (`ok`, or `shot <path>` for each screenshot) is sent once the whole request ran.

use plx_machine::machine::{InputEvent, Tick};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;

pub const DEFAULT_PORT: u16 = 47800;
const KEY_GAP_MS: u32 = 120;
/// After an OK release: the press commits a beat later, and a key arriving sooner cancels it.
const OK_SETTLE_MS: u32 = 400;

enum Step {
    Key(u32, u32),
    Move(f32, f32),
    Wheel(f32),
    Click(f32, f32),
    ReleasePointer,
    Release(u32, u32),
    Wait(u32),
    Shot(PathBuf),
    Exit,
    /// End of a request: reply on this connection.
    Done(usize),
}

struct Conn {
    stream: TcpStream,
    buf: String,
    reply: String,
    queued: bool,
}

pub struct Remote {
    listener: TcpListener,
    conns: Vec<Option<Conn>>,
    steps: VecDeque<Step>,
    /// Nothing runs before this time (ms, SDL clock).
    not_before: u32,
    /// The shot at the front of the queue, to be taken by the next presented frame.
    shot: Option<PathBuf>,
}

fn shot_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("YTNATIVE_SHOT_DIR") {
        return PathBuf::from(d);
    }
    // target/<profile>/ytnative.exe -> target/shots
    let exe = std::env::current_exe().unwrap_or_default();
    exe.parent().and_then(|p| p.parent()).map(|t| t.join("shots")).unwrap_or_else(|| PathBuf::from("shots"))
}

fn parse(tok: &str) -> Option<Step> {
    const ARROW: u32 = 1 << 30;
    Some(match tok {
        "up" => Step::Key(82 | ARROW, 0),
        "down" => Step::Key(81 | ARROW, 0),
        "left" => Step::Key(80 | ARROW, 0),
        "right" => Step::Key(79 | ARROW, 0),
        "ok" | "enter" => Step::Key(13, 0),
        "back" => Step::Key(0, 482),
        "exit" => Step::Exit,
        t if t.starts_with("wait:") => Step::Wait(t[5..].parse().ok()?),
        t if t.starts_with("wheel:") => Step::Wheel(t[6..].parse().ok()?),
        t if t.starts_with("move:") || t.starts_with("click:") => {
            let (kind, rest) = t.split_once(':')?;
            let (x, y) = rest.split_once(',')?;
            let (x, y) = (x.parse().ok()?, y.parse().ok()?);
            if kind == "move" { Step::Move(x, y) } else { Step::Click(x, y) }
        }
        t if t == "shot" || t.starts_with("shot:") => {
            let name = t.strip_prefix("shot:").filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| {
                format!("sim-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs())
            });
            let name: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || "-_.".contains(*c)).collect();
            Step::Shot(shot_dir().join(format!("{name}.png")))
        }
        _ => return None,
    })
}

impl Remote {
    pub fn open() -> Option<Remote> {
        let port = std::env::var("YTNATIVE_SIM_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT);
        let listener = match TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) => l,
            Err(e) => {
                crate::log(&format!("sim remote: cannot listen on 127.0.0.1:{port}: {e}"));
                return None;
            }
        };
        listener.set_nonblocking(true).ok()?;
        crate::log(&format!("sim remote: listening on 127.0.0.1:{port}"));
        Some(Remote { listener, conns: Vec::new(), steps: VecDeque::new(), not_before: 0, shot: None })
    }

    fn read_requests(&mut self) {
        while let Ok((s, _)) = self.listener.accept() {
            let _ = s.set_nonblocking(true);
            let conn = Conn { stream: s, buf: String::new(), reply: String::new(), queued: false };
            match self.conns.iter().position(Option::is_none) {
                Some(i) => self.conns[i] = Some(conn),
                None => self.conns.push(Some(conn)),
            }
        }
        for (id, slot) in self.conns.iter_mut().enumerate() {
            let Some(c) = slot else { continue };
            if c.queued {
                continue;
            }
            let mut chunk = [0u8; 512];
            let mut closed = false;
            loop {
                match c.stream.read(&mut chunk) {
                    Ok(0) => {
                        closed = true;
                        break;
                    }
                    Ok(n) => c.buf.push_str(&String::from_utf8_lossy(&chunk[..n])),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => {
                        closed = true;
                        break;
                    }
                }
            }
            let line = match c.buf.find('\n') {
                Some(end) => c.buf[..end].to_string(),
                None if closed => std::mem::take(&mut c.buf),
                None => continue,
            };
            for tok in line.split_whitespace() {
                match parse(tok) {
                    Some(step) => self.steps.push_back(step),
                    None => c.reply.push_str(&format!("unknown token {tok:?}\n")),
                }
            }
            self.steps.push_back(Step::Done(id));
            c.queued = true;
        }
    }

    fn finish(&mut self, id: usize) {
        if let Some(mut c) = self.conns.get_mut(id).and_then(Option::take) {
            if c.reply.is_empty() {
                c.reply.push_str("ok\n");
            }
            let _ = c.stream.set_nonblocking(false);
            let _ = c.stream.write_all(c.reply.as_bytes());
        }
    }

    /// Key events for the next queued token(s), press + release each.
    pub fn poll(&mut self, now: Tick, quit: &mut bool) -> Vec<InputEvent<u32>> {
        self.read_requests();
        let mut out = Vec::new();
        while self.shot.is_none() && now.ms >= self.not_before {
            let Some(step) = self.steps.pop_front() else { break };
            match step {
                // Press and release land in different frames, like a real key: the dispatcher
                // arms OK's press when it handles the down edge and activates on the up edge.
                Step::Key(sym, wcode) => {
                    out.push(crate::input::key_input(sym, wcode, 1, now));
                    self.steps.push_front(Step::Release(sym, wcode));
                    self.not_before = now.ms + KEY_GAP_MS / 2;
                    break;
                }
                Step::Release(sym, wcode) => {
                    out.push(crate::input::key_input(sym, wcode, 0, now));
                    // OK commits a beat after its release; a key arriving sooner cancels it.
                    self.not_before = now.ms + if sym == 13 { OK_SETTLE_MS } else { KEY_GAP_MS / 2 };
                    break;
                }
                Step::Wheel(dy) => {
                    out.push(crate::input::wheel_event(dy, now));
                    self.not_before = now.ms + KEY_GAP_MS;
                    break;
                }
                Step::Move(x, y) => {
                    out.push(crate::input::pointer(x, y, now));
                    self.not_before = now.ms + KEY_GAP_MS;
                    break;
                }
                Step::Click(x, y) => {
                    out.push(crate::input::pointer(x, y, now));
                    out.push(crate::input::click(x, y, now));
                    self.steps.push_front(Step::ReleasePointer);
                    self.not_before = now.ms + KEY_GAP_MS;
                    break;
                }
                Step::ReleasePointer => {
                    out.push(crate::input::release(now));
                    self.not_before = now.ms + OK_SETTLE_MS;
                    break;
                }
                Step::Wait(ms) => self.not_before = now.ms + ms,
                Step::Shot(p) => self.shot = Some(p),
                Step::Exit => *quit = true,
                Step::Done(id) => self.finish(id),
            }
        }
        out
    }

    /// A screenshot is waiting for the next presented frame.
    pub fn shot_pending(&self) -> bool {
        self.shot.is_some()
    }

    /// Called after the frame is drawn, before the swap: saves the pending shot.
    pub fn take_shots(&mut self, w: i32, h: i32) {
        let Some(path) = self.shot.take() else { return };
        let pixels = crate::sim::shot::read_frame(w, h);
        let line = match crate::sim::shot::save_png(&path, w, h, &pixels) {
            Ok(()) => {
                crate::log(&format!("sim: screenshot {}", path.display()));
                format!("shot {}\n", path.display())
            }
            Err(e) => {
                crate::log(&format!("sim: screenshot {} failed: {e}", path.display()));
                format!("error {e}\n")
            }
        };
        // The reply goes to the request this shot belongs to: the first `Done` still queued.
        let owner = self.steps.iter().find_map(|s| if let Step::Done(id) = s { Some(*id) } else { None });
        if let Some(c) = owner.and_then(|id| self.conns.get_mut(id)).and_then(Option::as_mut) {
            c.reply.push_str(&line);
        }
    }
}

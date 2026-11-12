//! Dev remote: a FIFO at `/tmp/ytnative/remote` drained once per frame. Writing key tokens to it
//! over SSH (`cargo xtask key down down ok`) drives the UI exactly like the physical remote.
//! In-app because webOS's compositor does not deliver injected system input to app surfaces
//! (same finding and design as plx-native's `src/remote.rs`).

use plx_machine::machine::{InputEvent, Tick};
use std::ffi::CString;

const DIR: &str = "/tmp/ytnative";
const PATH: &str = "/tmp/ytnative/remote";

pub struct Remote {
    fd: i32,
    _writer: i32,
    buf: String,
}

impl Remote {
    pub fn open() -> Option<Remote> {
        let _ = std::fs::create_dir_all(DIR);
        let p = CString::new(PATH).unwrap();
        unsafe {
            libc::unlink(p.as_ptr());
            if libc::mkfifo(p.as_ptr(), 0o666) != 0 {
                return None;
            }
            libc::chmod(p.as_ptr(), 0o666);
            let fd = libc::open(p.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK);
            if fd < 0 {
                return None;
            }
            // Hold a writer ourselves so the FIFO never reports EOF between external writes.
            let writer = libc::open(p.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK);
            Some(Remote { fd, _writer: writer, buf: String::new() })
        }
    }

    /// Key events for every complete token received since the last call (press + release each).
    pub fn poll(&mut self, now: Tick, quit: &mut bool) -> Vec<InputEvent<u32>> {
        let mut chunk = [0u8; 256];
        loop {
            let n = unsafe { libc::read(self.fd, chunk.as_mut_ptr().cast(), chunk.len()) };
            if n <= 0 {
                break;
            }
            self.buf.push_str(&String::from_utf8_lossy(&chunk[..n as usize]));
        }
        let mut out = Vec::new();
        let complete = match self.buf.rfind(|c: char| c.is_whitespace()) {
            Some(i) => self.buf.drain(..=i).collect::<String>(),
            None => return out,
        };
        for tok in complete.split_whitespace() {
            const ARROW: u32 = 1 << 30;
            let (sym, wcode) = match tok {
                "up" => (82 | ARROW, 0),
                "down" => (81 | ARROW, 0),
                "left" => (80 | ARROW, 0),
                "right" => (79 | ARROW, 0),
                "ok" | "enter" => (13, 0),
                "back" => (0, 482),
                "exit" => {
                    *quit = true;
                    continue;
                }
                other => {
                    plx_base::eventlog::log(&format!("remote: unknown token {other:?}"));
                    continue;
                }
            };
            out.push(crate::input::key_input(sym, wcode, 1, now));
            out.push(crate::input::key_input(sym, wcode, 0, now));
        }
        out
    }
}

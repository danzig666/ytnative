//! Fatal-signal tracer: on SIGSEGV/SIGBUS/SIGILL/SIGABRT/SIGFPE append a record to
//! `/tmp/ytnative/crash.log` and let the signal take its normal course.
//!
//! The idea (and the record shape) follows plx-native's `src/crashtrace.c` (GPL-3.0-or-later): the
//! TV has no core dumps for Developer Mode apps, so this record is the only evidence of a crash.
//! `cargo xtask crash` fetches the log and symbolizes it against the kept `ytnative.debug`.
//!
//! Record (one per crash):
//! ```text
//! *** SIGNAL 11 (SIGSEGV) code=1 addr=0x00000000 pc=0x... lr=0x...
//! build-id: <hex>                       which binary crashed (matched against ytnative.debug)
//! time: <unix secs> pid=<n> tid=<n> thread=<comm>
//! reg: r0=0x... ... r10=0x... fp=0x... ip=0x... sp=0x... lr=0x... pc=0x... cpsr=0x...
//! bt: 0x... 0x...                       pc, lr, then the frame-pointer chain (return addresses)
//! scan: 0x... 0x...                     stack words that point into our own code (fallback)
//! map: <every executable /proc/self/maps line>
//! *** END
//! ```
//!
//! Inside the handler only async-signal-safe calls are used: open/write/read/close/mkdir,
//! clock_gettime, getpid, gettid/prctl (syscalls), sigaction, raise. No allocation, no locks, no
//! formatting machinery: everything is written through a fixed buffer on the handler's stack.

/// Installs the tracer. Call once, first thing in `main`. A no-op off the TV (32-bit ARM Linux).
pub fn install() {
    #[cfg(all(target_os = "linux", target_arch = "arm"))]
    imp::install();
}

#[cfg(all(target_os = "linux", target_arch = "arm"))]
mod imp {
    use libc::{c_int, c_void, siginfo_t};
    use std::sync::atomic::{AtomicBool, Ordering};

    const DIR: &[u8] = b"/tmp/ytnative\0";
    const LOG: &[u8] = b"/tmp/ytnative/crash.log\0";
    const LOG_OLD: &[u8] = b"/tmp/ytnative/crash.log.1\0";
    const MAPS: &[u8] = b"/proc/self/maps\0";
    /// Rotate (once, at startup) past this size so the log cannot grow without bound in /tmp.
    const MAX_LOG: i64 = 256 * 1024;
    const SIGNALS: [c_int; 5] = [libc::SIGSEGV, libc::SIGBUS, libc::SIGILL, libc::SIGABRT, libc::SIGFPE];

    static BUSY: AtomicBool = AtomicBool::new(false);
    // Written once by `install` before any handler is armed, only read afterwards.
    static mut OLD: [libc::sigaction; 5] = unsafe { std::mem::zeroed() };
    static mut BUILD_ID: [u8; 64] = [0; 64];
    static mut BUILD_ID_LEN: usize = 0;
    static mut EXE: [u8; 256] = [0; 256];
    static mut EXE_LEN: usize = 0;
    // Scratch for reading /proc/self/maps inside the handler (static: keep the handler's stack
    // small, it may be running on a small sigaltstack). Guarded by BUSY.
    static mut CHUNK: [u8; 2048] = [0; 2048];
    static mut LINE: [u8; 384] = [0; 384];

    pub fn install() {
        unsafe {
            libc::mkdir(DIR.as_ptr().cast(), 0o755);
            let mut st: libc::stat = std::mem::zeroed();
            if libc::stat(LOG.as_ptr().cast(), &mut st) == 0 && st.st_size as i64 > MAX_LOG {
                libc::rename(LOG.as_ptr().cast(), LOG_OLD.as_ptr().cast());
            }
            let exe = &mut *std::ptr::addr_of_mut!(EXE);
            let n = libc::readlink(b"/proc/self/exe\0".as_ptr().cast(), exe.as_mut_ptr().cast(), exe.len() - 1);
            EXE_LEN = if n > 0 { n as usize } else { 0 };
            read_build_id();

            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = handler as *const () as usize;
            sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
            libc::sigemptyset(&mut sa.sa_mask);
            let old = &mut *std::ptr::addr_of_mut!(OLD);
            for (i, &sig) in SIGNALS.iter().enumerate() {
                libc::sigaction(sig, &sa, &mut old[i]);
            }
        }
    }

    /// Our own NT_GNU_BUILD_ID, found through the program headers the kernel handed us.
    unsafe fn read_build_id() {
        let phdr = libc::getauxval(libc::AT_PHDR) as usize;
        let phnum = libc::getauxval(libc::AT_PHNUM) as usize;
        if phdr == 0 {
            return;
        }
        let ph = |i: usize| std::slice::from_raw_parts((phdr + i * 32) as *const u32, 8);
        // Load bias: PT_PHDR's link-time address vs where it actually is (non-zero for PIE).
        let mut bias = 0usize;
        for i in 0..phnum {
            if ph(i)[0] == 6 {
                bias = phdr.wrapping_sub(ph(i)[2] as usize);
            }
        }
        for i in 0..phnum {
            let p = ph(i);
            if p[0] != 4 {
                continue; // PT_NOTE
            }
            let (start, size) = (bias.wrapping_add(p[2] as usize), p[4] as usize);
            let mut o = 0usize;
            while o + 12 <= size {
                let w = |k: usize| *((start + o + k) as *const u32) as usize;
                let (nsz, dsz, ty) = (w(0), w(4), w(8));
                let desc = o + 12 + nsz.div_ceil(4) * 4;
                if ty == 3 && nsz == 4 && desc + dsz <= size {
                    let id = std::slice::from_raw_parts((start + desc) as *const u8, dsz);
                    let out = &mut *std::ptr::addr_of_mut!(BUILD_ID);
                    let mut n = 0;
                    for b in id.iter().take(out.len() / 2) {
                        out[n] = HEX[(b >> 4) as usize];
                        out[n + 1] = HEX[(b & 15) as usize];
                        n += 2;
                    }
                    BUILD_ID_LEN = n;
                    return;
                }
                o = desc + dsz.div_ceil(4) * 4;
            }
        }
    }

    const HEX: &[u8; 16] = b"0123456789abcdef";

    /// Fixed-buffer writer; flushes with write(2).
    struct W {
        fd: c_int,
        buf: [u8; 256],
        len: usize,
    }

    impl W {
        fn s(&mut self, b: &[u8]) -> &mut Self {
            for &c in b {
                if self.len == self.buf.len() {
                    self.flush();
                }
                self.buf[self.len] = c;
                self.len += 1;
            }
            self
        }
        fn hex(&mut self, v: u32) -> &mut Self {
            let mut t = *b"0x00000000";
            for i in 0..8 {
                t[9 - i] = HEX[((v >> (i * 4)) & 15) as usize];
            }
            self.s(&t)
        }
        fn dec(&mut self, mut v: u64) -> &mut Self {
            let mut t = [0u8; 20];
            let mut i = t.len();
            loop {
                i -= 1;
                t[i] = b'0' + (v % 10) as u8;
                v /= 10;
                if v == 0 {
                    break;
                }
            }
            let mut c = [0u8; 20];
            let n = t.len() - i;
            c[..n].copy_from_slice(&t[i..]);
            self.s(&c[..n])
        }
        fn flush(&mut self) {
            let mut off = 0;
            while off < self.len {
                let r = unsafe { libc::write(self.fd, self.buf[off..].as_ptr().cast(), self.len - off) };
                if r <= 0 {
                    break;
                }
                off += r as usize;
            }
            self.len = 0;
        }
    }

    fn signame(sig: c_int) -> &'static [u8] {
        match sig {
            libc::SIGSEGV => b"SIGSEGV",
            libc::SIGBUS => b"SIGBUS",
            libc::SIGILL => b"SIGILL",
            libc::SIGABRT => b"SIGABRT",
            libc::SIGFPE => b"SIGFPE",
            _ => b"?",
        }
    }

    extern "C" fn handler(sig: c_int, info: *mut siginfo_t, ctx: *mut c_void) {
        unsafe {
            if !BUSY.swap(true, Ordering::AcqRel) {
                record(sig, info, ctx as *const libc::ucontext_t);
            }
            // Hand the signal back to whoever had it (Rust's stack-overflow handler or SIG_DFL).
            let idx = SIGNALS.iter().position(|&s| s == sig).unwrap_or(0);
            libc::sigaction(sig, &(*std::ptr::addr_of!(OLD))[idx], std::ptr::null_mut());
            // A fault re-executes the instruction and recurs under the old disposition. A signal
            // sent by kill/raise/abort (si_code <= 0) does not, so re-raise it: it stays pending
            // until this handler returns, so SAM sees a real death by that signal.
            if info.is_null() || (*info).si_code <= 0 {
                libc::raise(sig);
            }
        }
    }

    /// One parsed /proc/self/maps line.
    struct Map<'a> {
        start: u32,
        end: u32,
        exec: bool,
        path: &'a [u8],
        line: &'a [u8],
    }

    fn parse_hex(s: &[u8]) -> u32 {
        let mut v = 0u32;
        for &c in s {
            let d = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => break,
            };
            v = v.wrapping_mul(16).wrapping_add(d as u32);
        }
        v
    }

    fn parse_map(line: &[u8]) -> Option<Map<'_>> {
        let mut fields = [&line[..0]; 6];
        let mut n = 0;
        let mut i = 0;
        while i < line.len() && n < 6 {
            while i < line.len() && line[i] == b' ' {
                i += 1;
            }
            let s = i;
            if n == 5 {
                fields[5] = &line[s..];
                n += 1;
                break;
            }
            while i < line.len() && line[i] != b' ' {
                i += 1;
            }
            fields[n] = &line[s..i];
            n += 1;
        }
        if n < 5 {
            return None;
        }
        let dash = fields[0].iter().position(|&c| c == b'-')?;
        Some(Map {
            start: parse_hex(&fields[0][..dash]),
            end: parse_hex(&fields[0][dash + 1..]),
            exec: fields[1].get(2) == Some(&b'x'),
            path: fields[5],
            line,
        })
    }

    /// Streams /proc/self/maps line by line through the static buffers (lines longer than the
    /// buffer are truncated, which only shortens the path).
    unsafe fn for_each_map(f: &mut dyn FnMut(&Map)) {
        let fd = libc::open(MAPS.as_ptr().cast(), libc::O_RDONLY | libc::O_CLOEXEC);
        if fd < 0 {
            return;
        }
        let chunk = &mut *std::ptr::addr_of_mut!(CHUNK);
        let line = &mut *std::ptr::addr_of_mut!(LINE);
        let mut len = 0usize;
        loop {
            let r = libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len());
            if r <= 0 {
                break;
            }
            for &c in &chunk[..r as usize] {
                if c == b'\n' {
                    if let Some(m) = parse_map(&line[..len]) {
                        f(&m);
                    }
                    len = 0;
                } else if len < line.len() {
                    line[len] = c;
                    len += 1;
                }
            }
        }
        libc::close(fd);
    }

    unsafe fn record(sig: c_int, info: *const siginfo_t, uc: *const libc::ucontext_t) {
        libc::mkdir(DIR.as_ptr().cast(), 0o755);
        let mut fd = libc::open(LOG.as_ptr().cast(), libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND | libc::O_CLOEXEC, 0o640);
        if fd < 0 {
            fd = 2;
        }
        let mut w = W { fd, buf: [0; 256], len: 0 };
        let mc = if uc.is_null() { None } else { Some(&(*uc).uc_mcontext) };
        let (pc, lr, sp, fp) = mc.map(|m| (m.arm_pc, m.arm_lr, m.arm_sp, m.arm_fp)).unwrap_or((0, 0, 0, 0));
        let (code, addr) = if info.is_null() { (0, 0) } else { ((*info).si_code, (*info).si_addr() as usize as u32) };

        w.s(b"*** SIGNAL ").dec(sig as u64).s(b" (").s(signame(sig)).s(b") code=");
        if code < 0 {
            w.s(b"-").dec(code.unsigned_abs() as u64);
        } else {
            w.dec(code as u64);
        }
        w.s(b" addr=").hex(addr).s(b" pc=").hex(pc).s(b" lr=").hex(lr).s(b"\n");
        let bid = &*std::ptr::addr_of!(BUILD_ID);
        w.s(b"build-id: ").s(&bid[..BUILD_ID_LEN]).s(b"\n");
        let mut ts: libc::timespec = std::mem::zeroed();
        libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts);
        let mut comm = [0u8; 17];
        libc::prctl(libc::PR_GET_NAME, comm.as_mut_ptr());
        let clen = comm.iter().position(|&c| c == 0).unwrap_or(16);
        w.s(b"time: ").dec(ts.tv_sec as u64).s(b" pid=").dec(libc::getpid() as u64);
        w.s(b" tid=").dec(libc::syscall(libc::SYS_gettid) as u64).s(b" thread=").s(&comm[..clen]).s(b"\n");
        if let Some(m) = mc {
            let r = [m.arm_r0, m.arm_r1, m.arm_r2, m.arm_r3, m.arm_r4, m.arm_r5, m.arm_r6, m.arm_r7, m.arm_r8, m.arm_r9, m.arm_r10];
            w.s(b"reg:");
            for (i, v) in r.iter().enumerate() {
                w.s(b" r").dec(i as u64).s(b"=").hex(*v);
            }
            w.s(b" fp=").hex(m.arm_fp).s(b" ip=").hex(m.arm_ip).s(b" sp=").hex(sp).s(b" lr=").hex(lr);
            w.s(b" pc=").hex(pc).s(b" cpsr=").hex(m.arm_cpsr).s(b"\n");
        }

        // Pass 1 over the maps: the stack mapping holding sp (bounds every read below, so the
        // walk can never fault) and our own executable mappings (for the stack scan).
        let exe = &*std::ptr::addr_of!(EXE);
        let exe = &exe[..EXE_LEN];
        let (mut lo, mut hi) = (0u32, 0u32);
        let mut text = [(0u32, 0u32); 4];
        let mut ntext = 0;
        for_each_map(&mut |m| {
            if sp >= m.start && sp < m.end {
                lo = m.start;
                hi = m.end;
            }
            if m.exec && !exe.is_empty() && m.path == exe && ntext < text.len() {
                text[ntext] = (m.start, m.end);
                ntext += 1;
            }
        });
        let readable = |a: u32| a & 3 == 0 && a >= lo && a.wrapping_add(4) <= hi && hi != 0;

        // Frame-pointer chain. Rust code here is built with frame pointers in ARM mode, where
        // r11 points at the saved {r11, lr} pair: [fp] = caller's fp, [fp + 4] = return address.
        w.s(b"bt: ").hex(pc).s(b" ").hex(lr);
        let mut f = fp;
        for _ in 0..64 {
            if !readable(f) || !readable(f.wrapping_add(4)) {
                break;
            }
            let next = *(f as *const u32);
            let ret = *((f + 4) as *const u32);
            if ret == 0 {
                break;
            }
            w.s(b" ").hex(ret);
            if next <= f {
                break;
            }
            f = next;
        }
        w.s(b"\n");

        // Fallback when the chain breaks (a fault inside a TV library built without frame
        // pointers): stack words that point into our own code are likely return addresses.
        w.s(b"scan:");
        let mut a = sp;
        let mut found = 0;
        while found < 24 && readable(a) && a < sp.wrapping_add(16 * 1024) {
            let v = *(a as *const u32);
            if text[..ntext].iter().any(|&(s, e)| v >= s && v < e) {
                w.s(b" ").hex(v);
                found += 1;
            }
            a += 4;
        }
        w.s(b"\n");

        // Pass 2: every executable mapping, so the symbolizer can turn any address into
        // (file, offset).
        for_each_map(&mut |m| {
            if m.exec {
                w.s(b"map: ").s(m.line).s(b"\n");
            }
        });
        w.s(b"*** END\n");
        w.flush();
        if fd != 2 {
            libc::fsync(fd);
            libc::close(fd);
        }
    }
}

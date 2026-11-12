# Debugging ytnative on the TV

Everything the app writes on the TV lives under `/tmp/ytnative/` (cleared on reboot).

## Logs

| File | Written by | Read with |
|---|---|---|
| `/tmp/ytnative/ytnative.log` | the event log (`log()` in `crates/app/src/main.rs`) | `cargo xtask logs [n]` |
| `/tmp/ytnative/crash.log` | the crash tracer, append-only, survives relaunches | `cargo xtask crash` |
| `/var/log/messages` (SAM lines with `exit_status`) | the system | `cargo xtask ssh "grep exit_status /var/log/messages \| tail"` |

A Rust panic prints to stderr and aborts: the tracer records the SIGABRT, but the panic message is
in the app log / stderr, not in the crash record.

## Crash log

`crates/app/src/crashtrace.rs` (installed first thing in `main`) catches SIGSEGV, SIGBUS, SIGILL,
SIGABRT and SIGFPE, appends one record to `/tmp/ytnative/crash.log` using only
async-signal-safe calls, then hands the signal back to the previous disposition so the process
still dies of that signal (SAM sees a real signal death). The log is rotated to `crash.log.1` at
startup once it passes 256 KiB. A record:

```text
*** SIGNAL 11 (SIGSEGV) code=1 addr=0x00000000 pc=0x0091f0d0 lr=0x0091f02c
build-id: 09faebab944aec143b8cde4e4845d5a6fa0c8ec6
time: 1700000000 pid=1234 tid=1240 thread=ytnative
reg: r0=0x... r10=0x... fp=0x... ip=0x... sp=0x... lr=0x... pc=0x... cpsr=0x...
bt: <pc> <lr> <return addresses from the frame-pointer chain>
scan: <stack words that point into our own code: a fallback when the chain breaks in a TV library>
map: <every executable line of /proc/self/maps>
*** END
```

The frame-pointer walk relies on the build's `-C force-frame-pointers=yes` (ARM mode: `r11` points
at the saved `{r11, lr}` pair) and is bounded to the stack mapping that holds `sp`, so it cannot
fault. Frames inside TV libraries usually have no frame pointers; the `scan:` line covers that.

### Symbolizing

`cargo xtask build` keeps the linked binary (line tables + symbol table) as
`target/armv7-unknown-linux-gnueabi/release/dist/ytnative.debug` and ships a copy stripped with
`zig objcopy --strip-all` (`dist/ytnative`). Both carry the same GNU build id (`--build-id=sha1`),
which the tracer logs.

```bash
cargo xtask crash                    # fetch /tmp/ytnative/crash.log from the TV (saved under
                                     # target/crash/) and symbolize the newest record
cargo xtask crash some.log [--all]   # a local copy; --all = every record
cargo xtask crash --selftest         # synthetic record from known functions; no TV needed
```

For each address the symbolizer finds its `map:` line, turns it into a file offset
(`addr - start + offset`) and then, through the binary's PT_LOAD headers, into a link-time address
(right for today's PIE binary and for a non-PIE one), and resolves it with the `addr2line` crate
(function, file:line, inlined frames). Return addresses are looked up at `addr - 1` (the call).
Addresses in TV libraries are printed as `libfoo.so+0xoffset`.

If the record's build id differs from `ytnative.debug`, the output says so: the frames would be
confidently wrong. Symbolize against the `.debug` of the build that crashed: for a release that is
the `ytnative-X.Y.Z.debug` release asset (copy it to `dist/ytnative.debug`).

Hints by signal: SIGILL = codegen (an instruction this SoC lacks: check `target-cpu`/`-neon`);
SIGABRT = panic/abort, read the log; SIGSEGV/SIGBUS = bad pointer or wrong struct offset at the
first `ytnative` frame (often an FFI signature that does not match the TV library).

## Will it load on another TV? (`symcheck`, `fwcompat`)

The binary links against generated stubs (docs/release.md), so a successful build says nothing
about whether a given TV has every library and symbol. Two checks answer that offline:

* `cargo xtask needed [--check]`: the binary's DT_NEEDED (and the comparison with
  `ci/expected-dt-needed.txt`; `cargo xtask needed <file>` dumps any ELF's DT_NEEDED, soname and
  build id).
* `cargo xtask symcheck`: every import of the binary against `sysroot/lib` (your TV), resolved the
  way `ld.so` does it: breadth-first through DT_NEEDED and the libraries' own DT_NEEDED, so GL
  symbols count as provided by `libmali.so.0` that `libGLESv2.so.2` pulls in. Symbol versions
  (`memcpy@GLIBC_2.4`) must match. Weak imports are optional and skipped.
* `cargo xtask fwcompat <dir> [-v]`: the same, graded against every snapshot under
  `<dir>/<name>/lib/`, with the GLIBC versions the binary needs. Example:

  ```text
  snapshot                     firmware                            missing     libs  verdict
  lg-w23o-10.2.2               VERSION="10.2.2" "hardware_id": ...        0        0  OK (loads: every import resolves)
  ```

  OK means "the dynamic loader can start it", not "it works": behaviour (video pipeline, luna
  services) can still differ between firmwares.

### Adding another TV's libraries

1. On a checkout, point the tools at that TV (`YTNATIVE_TV=<ip>`, its SSH key in
   `YTNATIVE_TV_KEY`) and pull its libraries:
   ```bash
   mv sysroot sysroot.mine                     # keep yours
   YTNATIVE_TV=192.0.2.10 cargo xtask pull-sysroot
   mkdir -p sysroots && mv sysroot sysroots/<model>-<webos-version>
   mv sysroot.mine sysroot
   ```
   Without SSH access, copy the files listed in `SYSROOT_LIBS` (`xtask/src/main.rs`) from the TV
   by any means into `sysroots/<name>/lib/` (real files under their soname, not symlinks) and
   write the firmware version into `sysroots/<name>/FIRMWARE.txt`.
2. `cargo xtask fwcompat sysroots/`.

`sysroots/` and `sysroot/` are gitignored: they hold LG binaries and must never be committed. Only
the symbol lists derived from them (`ci/symbols/`) are.

If a snapshot misses a library or symbol the app needs on every firmware, the choices are: load it
with `dlopen` and degrade gracefully (as `ls2.rs` does for luna-service2), or set a minimum webOS
version in the Homebrew Channel listing.

### Refreshing the stub symbol lists

`cargo xtask stub-symbols` rewrites `ci/symbols/*.txt` from `sysroot/lib` (full export lists; the
GL/EGL lists take the `gl*`/`egl*` exports of the Mali driver). Do it when linking a new library
(add it to `STUBS` in `xtask/src/checks.rs` and to `link_tv_stubs` in `crates/app/build.rs`);
`cargo xtask stubs` regenerates `target/stubs/` by hand (the build does it automatically).

# Vendored: plx-native UI core

- Source: https://github.com/GLinnik21/plx-native at commit `31e6f24` (2026-11-12)
- License: GPL-3.0-or-later (see `LICENSE`, `LICENSING.md`, `THIRD-PARTY-NOTICES.md` in this folder).
  Fonts and icons under `assets/` keep their own licenses (OFL-1.1, Apache-2.0, MIT) as listed there.
- Copied: `rust-modules/{base,machine,gfx,platform,ui,build_support}`, `assets/`, `locales/`,
  `ci/install-identities.json`, `src/main.c` (referenced by a test via include_str).
- The PlxNative name and brand artwork are not used by ytnative (see upstream TRADEMARKS.md).
- Local changes are made as separate commits so they can be diffed against upstream.

## Local changes
- `ui/src/lib.rs`: `card_row` made `pub` (apps need `RowStyle` / `TileLabel` for their card sources).
- `platform/src/tv/toast.rs`: `deliver_without_port` made `pub` (used by our `tv::Port`).

### Windows desktop simulator (all `cfg(windows)`-gated; the TV build compiles exactly what it did)
The ytnative simulator (`cargo xtask sim`, `crates/app/src/sim/`) builds these crates for
`x86_64-pc-windows-msvc` with the `hostsim` feature. Shims for Unix-only APIs:
- `build_support/host_link.rs`: `emit` returns early for `target_os = "windows"` (the app's
  build.rs links SDL2/SDL2_ttf from `third_party/win` and compiles nanosvg; no python3/pkg-config/libGL).
- `base/src/dynlib.rs`: a private `win` module implementing the `dlopen`/`dlsym`/`dlerror` subset over
  `LoadLibraryA`/`GetModuleHandleA`/`GetProcAddress`; `RTLD_NOLOAD` imported from it / libc.
- `base/src/eventlog.rs`: Windows `open_log_append` (plain append+create; no O_NOFOLLOW/uid/nlink checks).
- `base/src/task/watchdog.rs`: no `pthread_self` (main thread id 0) and `probe_main_thread` returns
  false (no SIGUSR2 freeze probe / crash tracer); `handler_is_ours` is Unix-only.
- `gfx/src/text.rs`: `app_font_path` builds the path from UTF-8 (no `OsStrExt::from_bytes`).
- `platform/src/lib.rs`: `storage` (the on-TV storage-helper client: fd passing, openat, statvfs, uid
  checks) is replaced by `platform/src/storage/win_stub.rs` (new file: `state`, `wire` and a no-op
  `diagnostics::activation`, all `webos.rs` needs).
- `platform/src/storage_service/wire.rs`: the `activation` submodule (Unix fd/uid protocol) is not built on Windows.
- `platform/src/imgcache.rs`: Windows `free_bytes` (assumes 4 GiB, no statvfs) and `private_dir`
  (`create_dir_all`); `probe_dir` passes owner 0 instead of `geteuid()`.
- `platform/src/playback_trace.rs`: Windows `open_owned_log` (plain open).
- `platform/src/i18n/mod.rs`: `localtime_r`/`mktime` go through a `crt` alias: libc on Unix,
  `_localtime64_s`/`_mktime64` from the MSVC CRT on Windows.
- `platform/src/webos.rs`, `platform/src/webos/jail_repair.rs`: `libc::R_OK` via a crate-level `R_OK`
  constant (libc has none on Windows; same value 4).
- `ui/src/rec.rs`: `bounded_read` skips `custom_flags` on Windows; `PATH_MAX` written as its Linux
  value 4096; Windows `file_identity` (len + mtime instead of dev/ino), `DirSink::create` (no mode)
  and `private_new_file` (no mode/flags).

- `base/src/surface.rs`: `set_ui_scale` (ytnative's Screen size / overscan setting) multiplies the fitted scale before the viewport is derived.
- `ui/src/card_row.rs`, `ui/src/poster_grid.rs`: `set_label_band_extra` adds room under every row (titles under every thumbnail); the collapsed band is read through `label_band_collapsed()`.
- `gfx/src/text.rs`: extra faces (`subfont-*.ttf`, subtitle fonts) selected by `bold >> 4` in `font_at`.
- gfx/src/gfx/tokens.rs: `set_clear_black` / `clear_rgb()` — runtime ground colour (OLED black);
  gfx.rs, ui/popover.rs and ui/widgets.rs read `clear_rgb()` instead of the `CLEAR_RGB` constant,
  so page-transition snapshots clear to black too.
- gfx/src/gfx.rs `try_compile`: a fragment shader the driver rejects is retried once with `highp`
  replaced by `mediump` (fragment highp is optional in GLES2), for GPUs without it.
- gfx/src/text.rs: the font chain gains the television's own script faces after `Sys`
  (`/usr/share/fonts/LG_Smart_UI_{Devanagari,Bengali,Gujarati,Gurmukhi,Kannada,Malayalam,Oriya,Tamil,Telugu,Amharic}.ttf`,
  `DroidSansThai-Regular.ttf`, `NotoSans{Sinhala,Khmer,Lao,Myanmar,Georgian,Armenian}-Regular.ttf`), drawn by SDL2_ttf
  unshaped; `LINKS`, `SCRIPT_FONTS`, `FONTS_SCRIPT`; the test helpers treat links past `Hebrew` as absent.
- gfx/src/text.rs: the script links are `shaped()` when `rtl::available()` (HarfBuzz conjuncts and
  reordering for Indic and South-East Asian scripts), with their own `FACES_SCRIPT` slots; without
  the library they stay on SDL2_ttf. The shaping library itself is built from plx-native's
  `src/ass_text.c` in `native/textshape/` against the TV's FreeType/HarfBuzz/FriBidi.

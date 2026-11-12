# ytnative-poc

Step 1 of a native Rust YouTube client for LG webOS (in the spirit of
[plx-native](https://github.com/GLinnik21/plx-native), with
[SmartTube](https://github.com/yuliskov/smarttube) as the feature reference). It answers one
question: **can plain Rust, with no JS engine and no Android code, sign in like a TV and get
playable 1080p streams?**

## Usage

```bash
cargo build --release
ytnative-poc login                       # TV code sign-in (google.com/device)
ytnative-poc whoami                      # authenticated call: account name
ytnative-poc subs                        # authenticated call: subscriptions feed
ytnative-poc streams <id|url> [--tv] [--sample 15]
```

`streams` resolves the best ≤1080p H.264 video plus AAC audio. It checks both URLs with a
real ranged GET and prints the HLS manifest summary. `--sample N` muxes the first N seconds into
`sample.mp4` with ffmpeg. `--tv` also reports what the TVHTML5 client returns.

## How it works

1. **Bootstrap**: a single GET of `https://www.youtube.com/tv` with a Cobalt user agent. This is
   the page LG's own YouTube app loads. Regexes pull out the TV OAuth `clientId`/secret (inline in
   the HTML), the current `TVHTML5` client version and a `visitorData`. Hardcoded fallbacks cover
   layout changes.
2. **Sign-in**: the standard TV device flow. `POST /o/oauth2/device/code` returns a user code,
   then the CLI polls `/o/oauth2/token`. The refresh token goes to `token.json`.
3. **Streams**: `POST /youtubei/v1/player` as the `VISIONOS` client, the same client yt-dlp
   2026.10 uses when no JS runtime is available. It returns direct HTTPS URLs: no
   `signatureCipher`, no `n` challenge, no PO token.

## Results (2026-11-12)

| Check | Result |
|---|---|
| Bootstrap from youtube.com/tv | ✅ client id/secret, TV version and visitorData scraped |
| Device code issued | ✅ `google.com/device` + user code |
| Big Buck Bunny 60fps 4K | ✅ itag 299 1080p60 avc1 + itag 140 AAC, both HTTP 206 / `ftyp` |
| Rick Astley, Despacito (VEVO), Me at the zoo | ✅ all OK, 1080p avc1 (240p for the 2005 video) |
| Ciphered / `n`-challenge formats | 0 of 25–32 per video, so **no JS engine needed** |
| HLS manifest | ✅ up to 2160p (17 variants) |
| ffmpeg mux of 15s from the two URLs | ✅ h264 1920x1080@60 + aac, 15.0s |
| Throughput | One 50 MB range: ~5.6 Mbit/s (throttled). 2 MB chunks via `&range=`: **~35 Mbit/s**, near line speed |
| TVHTML5 player call, anonymous | ❌ `UNPLAYABLE`, as expected (needs sign-in / PO token) |
| Device-code sign-in completed | ✅ token saved; refresh handled on load |
| `whoami` (account/accounts_list, signed in) | ✅ returns account name + handle |
| `subs` (browse FEsubscriptions, signed in) | ✅ 44 videos with titles on the first page |
| TVHTML5 player call, signed in | ⚠️ `OK`, but **SABR only**: formats have no `url`/`signatureCipher`, only a `serverAbrStreamingUrl` |

## What this means for the webOS app

- **Extraction is viable without a JS engine today.** The anonymous `VISIONOS` client delivers
  everything up to 4K. That is a lucky window, not a guarantee: yt-dlp has cycled through
  android → ios → android_vr → visionos as YouTube closes each one. The client profile must stay
  data-driven and easy to swap. Keep a fallback plan (QuickJS + yt-dlp's EJS solver) for the
  day every JS-less client needs PO tokens.
- **The real TV client is already SABR-only.** Signed-in TVHTML5 returns no per-format URLs at
  all, just a `serverAbrStreamingUrl`. Copying LG's own app 1:1 would mean implementing SABR
  (protobuf-over-POST streaming) from day one, so don't. SABR is the long-term threat to the
  JS-less route too.
- **Split the roles.** Use the signed-in `TVHTML5` client for personal data (home, subscriptions,
  history, likes) and the anonymous JS-less client for stream URLs. SmartTube-like behaviour
  (watch history) then needs explicit `/stats/playback` pings with the auth token.
- **Player must fetch in chunks.** Request ≤ ~2–10 MB per request using the `range=` URL
  parameter, as YouTube's own players do. One long GET gets throttled below 1080p60 bitrates.
- **Two playback paths on webOS:**
  - HLS manifest (up to 4K) handed to the TV's native media pipeline. This is the least code and
    may be the fastest way to step 2.
  - DASH-style separate H.264/VP9/AV1 + AAC/Opus tracks fed to the hardware decoder. This means
    more work, but gives full control over quality switching.
- **Not covered yet:** age-restricted / members-only / "made for kids" videos (the JS-less clients
  can't play some of these), live streams, SABR-only formats.

---

# Step 2: playback on the LG C3 (2026-11-12) ✅

**Result:** YouTube DASH (H.264 + AAC) plays full screen with sound on the C3 through the TV's
own hardware pipeline. LG's adaptive logic climbs 144p → 480p30 → 480p60 → **1080p60** in about
12 s, and a 60 s run played without errors (confirmed by watching the TV).

TV: webOS 10.2.2 (o22n, HE_DTV_W23O). The kernel is aarch64, but userland is **32-bit ARM,
soft-float EABI** (`/lib/ld-linux.so.3`) with glibc 2.35.

## Pieces

| Piece | What it does |
|---|---|
| `ytnative-poc streams <id>` | Now also writes `last.mpd`: a DASH manifest of all H.264 ≤1080p + AAC representations (SegmentBase with init/index ranges) |
| [`tvplay/`](tvplay) | ~450 KB Rust binary for the TV. Loads the TV's `libSDL2` and `libluna-service2` with dlopen, so nothing is linked against LG libraries |
| [`tools/zig-cc-armv7.cmd`](tools/zig-cc-armv7.cmd) | Uses zig as the linker to cross-compile from Windows (`armv7-unknown-linux-gnueabi`, glibc 2.17 symbols) |
| [`tools/fix-ipk-modes.py`](tools/fix-ipk-modes.py) | Restores `+x` in IPKs built by `ares-package` on Windows |

`tvplay` flow:
1. Creates a transparent fullscreen SDL window and gets an exported video window id from
   `SDL_webOSCreateExportedWindow(0)`.
2. Registers on the luna bus as `com.webos.media.client.ytnative<pid>`.
3. Calls `com.webos.media/load` with `{uri, type:"media", payload.option:{appId, windowId}}`,
   then `subscribe`, `notifyForeground`, `play`.
4. Exits on BACK, timeout or end of stream, and always unloads.

## Build / deploy / run

```bash
cargo build --release && ./target/release/ytnative-poc streams <id>                # -> last.mpd
cd tvplay && cargo build --release && cp target/armv7-unknown-linux-gnueabi/release/tvplay pkg/
ares-package -n pkg -o . && python ../tools/fix-ipk-modes.py com.ytnative.poc_<ver>_arm.ipk poc.ipk tvplay
# on the TV: dev/install via luna://com.webos.appInstallService, then
luna-send -n 1 luna://com.webos.applicationManager/launch '{"id":"com.ytnative.poc","params":{"url":"file:///tmp/ytnative/last.mpd","secs":60}}'
```

Logs go to `/tmp/ytnative/tvplay.log`. Pipeline internals are in `/var/log/legacy-log`
(timestamps in UTC).

## Dead ends (each cost a TV round trip)

- **`luna-send` from a root shell without a window:** decoding works (it reported 1080p60 H.264 to
  the hardware decoder) but nothing reaches the screen. The video needs an exported window that
  belongs to an app.
- **Running the SDL binary from SSH:** the window manager shows it, then hides it within a second.
  It has to run as an installed app.
- **`ares-package` on Windows:** the binary loses `+x`, so the app silently never starts.
- **A wrapper script as `main`:** luna ACG roles are bound to the exe path, so after `exec` the
  binary is "Invalid permissions". The bare app-id name is also already taken by SDL's lifecycle
  registration. Use a `com.webos.media.client.*` name, which the generated role allows.
- **YouTube HLS:**
  - The full master playlist dies when the TV switches to the VP9/fMP4 variants (error 100).
  - An H.264/TS-only playlist downloads segments but never prerolls. That's a stall in LG's
    hlsdemux2/TS path, not caused by the separate audio rendition (video-only stalls too).
- **DASH with normal URLs:** webOS's `dashdemux2` drops the query string of a `BaseURL`
  (`GET /videoplayback` → 404). Fix: googlevideo accepts the parameters as path segments
  (`/videoplayback/itag/299/...`), the same form its own HLS manifests use.

## Next

- Serve the MPD from the app itself (localhost HTTP or `file://` written by the app). The PC CLI
  goes away once extraction runs on the TV (same Rust code, different target).
- VP9/AV1 ladders for 4K/HDR (check which codecs `dashdemux2` accepts on this model), quality
  caps, seeking, pause/resume, an overlay UI.
- Package properly: build the IPK without `ares-package` so permissions are right, and use the
  `nativeLifeCycleInterfaceVersion: 2` relaunch handling.

## Footprint on the TV (all reversible, no system partitions touched)

- App `com.ytnative.poc` in `/media/developer/apps`. Remove with
  `luna-send -n 1 luna://com.webos.appInstallService/dev/remove '{"id":"com.ytnative.poc"}'`.
- Scratch files in `/tmp/ytnative`, which is a tmpfs and gone on reboot.
- One line in `/home/root/.ssh/authorized_keys` (`ytnative-claude`), added by you.

---

# Progress toward the full app (see [PLAN.md](PLAN.md))

**2026-11-12: first end-to-end native app on the C3** (`crates/app`, installed as
`io.github.danzig666.ytnative`):
- Boots on plx-native's vendored UI core (GLES2, focus engine, card grid, transitions).
- Loads your signed-in Home feed through `yt-core` on a worker thread, with thumbnails.
- OK opens the player: streams resolved in-app, DASH manifest (720p–1080p H.264 + AAC) played
  by the TV's media service on an exported video window. Starts at 720p and switches to 1080p.
  OK = pause, ←/→ = seek 10 s, BACK = stop and return to Home.

Dev loop (all from Windows): `cargo xtask build | install | deploy | run | key <keys> | shot | logs`.

## Windows desktop simulator (no TV needed)

The same app (`crates/app`, `--features sim`) runs in a 1920x1080 window on Windows with desktop
OpenGL, for exercising and screenshotting the UI. There is no video: the player resolves the video
for real (title, captions, SponsorBlock, storyboard) and runs a fake clock under "Simulator: no
video", so the HUD, seeking and menus work. Nothing is reported to watch history from it.

```
powershell -ExecutionPolicy Bypass -File tools/fetch-sim-deps.ps1   # once: SDL2 + SDL2_ttf (SHA-256 checked)
cargo xtask sim                      # build + run (--release, --detach; -- passes args to the app)
cargo xtask sim-key down down ok     # remote keys: up down left right ok back exit, wait:<ms>
cargo xtask sim-shot sim-home        # -> target/shots/sim-home.png
```

- Keyboard: arrows, Enter = OK, Escape / Backspace = BACK. Close the window to quit.
- Data (token, accounts, settings): `%LOCALAPPDATA%\ytnative`; `token.json` from the repo (or the
  main checkout, or `YTNATIVE_SIM_TOKEN`) is copied there when no account exists yet.
- Key channel: TCP `127.0.0.1:47800` (`YTNATIVE_SIM_PORT`); one request = tokens + newline, e.g.
  `right ok wait:1500 shot:x` runs in order and replies when done.
- Env: `YTNATIVE_SIM_WINDOW=1280x720` (window size, UI scaled), `YTNATIVE_SIM_HIDDEN=1`,
  `YTNATIVE_SHOT_DIR`, `YTNATIVE_SIM_LOUNGE=1` (phone casting is off by default in the simulator).
- GL: a 4.1 core context (plx-native's desktop GLSL preamble + VAO, as its macOS simulator), else
  GLES 2.0, else the driver default; every GL entry point is forwarded through
  `SDL_GL_GetProcAddress` (`crates/app/src/sim/gl.rs`).

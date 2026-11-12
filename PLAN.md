# ytnative: full plan for a Rust SmartTube for LG webOS

Goal: a native Rust YouTube client for LG webOS TVs with **every SmartTube feature that makes
sense on a TV**. It should be faster than LG's web-based YouTube app and controlled entirely by us.
The first target is your LG C3 (webOS 10.2.2). The second is webOS 5+ in general.

This plan is based on:
- the working proof of concept in this repo ([README.md](README.md): sign-in, extraction, and
  1080p60 playback on the C3);
- a code-level study of **plx-native** (UI shell, packaging, webOS lifecycle, both playback engines);
- an inventory of **SmartTube** and its fork **NewTube** (features, settings, engine).

Licences: SmartTube and NewTube are MIT. plx-native is GPL-3.0-or-later. Because we reuse plx
crates, **ytnative is GPL-3.0**, and the source must be published next to every release binary.

---

## 1. What we start from

| Area | State today |
|---|---|
| TV sign-in (device code), token refresh | ✅ working (`src/auth.rs`) |
| InnerTube browse (subscriptions), account | ✅ working (TVHTML5, signed in) |
| Stream URLs without JS | ✅ `VISIONOS` client: direct URLs, no cipher, no PO token. This window can close at any time |
| DASH MPD builder | ✅ H.264 ≤1080p + AAC, path-style URLs (`src/mpd.rs`) |
| Playback on the C3 | ✅ `tvplay`: SDL exported window → `com.webos.media` load → 1080p60 with sound, adaptive bitrate in firmware |
| Cross-build from Windows | ✅ Rust `armv7-unknown-linux-gnueabi` + `zig cc` (plx-native used the same approach before moving to the NDK) |
| Signed-in TVHTML5 streams | ⚠️ SABR only (no per-format URLs) |

## 2. Architecture

```
┌──────────────────────── ytnative (one native webOS app, GPL-3.0) ────────────────────────┐
│ app        main loop (SDL is the only scheduler), lifecycle, keys, routing               │
│ screens    Home · Subs · History · Playlists · Channels · Search · Channel · Player HUD  │
│            · Settings (declarative forms) · Accounts · Comments · Chat · Dialogs         │
│ ui/gfx     ← plx-native `ui`, `gfx`, `machine`, `base` (vendored, trimmed)               │
│ yt-core    InnerTube clients · auth (multi-account) · browse/next/search/guide parsers   │
│            · actions · player client ring · MPD/storyboard/caption builders · Lounge     │
│ yt-ext     SponsorBlock · DeArrow · RYD · RSS channel groups · oEmbed                    │
│ playback   Engine A: URI + MPD (firmware DASH)   Engine B: Feed (StarfishMediaAPIs)      │
│            ← plx `starfish.c`, `video_plane.c`, payload builders, framers, feed loop     │
│            SABR client (protobuf/UMP) → Engine B                                         │
│ platform   luna (LS2 worker threads) · storage · i18n · image cache · curl transport     │
├───────────────────────────────────────────────────────────────────────────────────────────┤
│ JS helper (webOS JS service on the TV's Node 16): n-sig/sig solver (yt-dlp EJS)          │
│ BotGuard helper (hidden web app on WebAppMgr/Chromium, only if PO tokens are needed)     │
└───────────────────────────────────────────────────────────────────────────────────────────┘
```

**Decisions and why:**

1. **Cherry-pick plx crates; don't fork and strip.** plx-native is about 850k lines of Rust, about
   60% of it Plex logic and CI gates built around Plex. We vendor only these crates: `base`
   (trimmed), `machine` (5.5k lines), `gfx` (25k), and `ui` (101k: retained views, focus engine,
   NavStack/ModalStack, declarative settings forms, springs, text with font fallback and
   right-to-left support). We use plx `screens/` and `appkit/` only as reference for our own
   screens. Fallback if `ui` proves too heavy: keep only `gfx`, `machine::motion` and `focus.rs`,
   and write a smaller UI ourselves (+2–3 weeks).
2. **Two playback engines.**
   - **A, URI + MPD:** the TV's dashdemux2 handles ABR, sync and HDR signalling. It already works,
     so it is the default for H.264 ≤1080p.
   - **B, Feed:** our own demux of separate video and audio files, framed and pushed through
     `StarfishMediaAPIs::Feed`. This is plx-native's default engine and is proven on device for
     H.264, HEVC, Dolby Vision and Atmos. We need it for VP9/AV1/4K, Opus, SABR and live streams,
     and anywhere dashdemux2 misbehaves.
3. **Extraction is a ring of clients, kept as data.** We copy SmartTube's fallback ring, adjusted
   by today's measurements:
   `VISIONOS (no JS) → TV signed in (SABR) → WEB/TV_DOWNGRADED + n-sig (JS helper) → IOS/MWEB/ANDROID_VR`.
   Client profiles live in a remotely updatable JSON, so a YouTube change becomes a config push
   rather than a release.
4. **JS runs on the TV's own engines.** webOS ships Node 16 (for JS services) and WebAppMgr with
   LG's Chromium (`libcbe.so`). The n-sig solver becomes a bundled webOS JS service. BotGuard (PO
   tokens), if needed, runs in a hidden web app. SmartTube needs an Android WebView for the same
   job. Fallback: embed QuickJS (rquickjs builds with zig).
5. **Build: Windows only, no LG NDK.**
   - zig links against `.so` stubs pulled from the C3, with glibc symbols ≤2.17 and plx's flags
     (`-C target-cpu=cortex-a9 -C target-feature=-neon,-soft-float`).
   - Our own IPK writer replaces `ares-package`. It writes bare `ar` member names with
     `debian-binary` first, sets 0755 on the binary inside the tar, and includes `packageinfo.json`
     and `Installed-Size`, as `ci/mkipk.py` does.
   - A symbol check runs against the TV's real libraries, guarding against plx's SDL_ttf trap
     (27 `TTF_*` symbols that link and don't exist on any TV).
6. **Windows simulator.** The same app runs on a desktop SDL2 + GL window, with video stubbed or
   played by ffmpeg. plx's simulator is macOS/Linux only, so the Unix-only bits (`O_NOFOLLOW`,
   `sigaction`, `statvfs`, FIFOs) get `cfg(windows)` shims. UI work then mostly happens without the TV.

## 3. Feature matrix (SmartTube → ytnative)

Legend: **P** = port as is · **A** = adapt to webOS · **✗** = not applicable on webOS. The
number is the phase (§4).

### Browsing and content
| Feature | | Ph |
|---|---|---|
| Home, Subscriptions, History, Playlists, Channels, Shorts, Music, Gaming, News, Sports, Movies, Live, Trending, Kids, Notifications, My videos, Queue | P | 2–3 |
| Sidebar: pin channels/playlists, reorder/rename/hide sections, boot section | P | 3 |
| Channel page (uploads, popular, live, playlists, channel search, sort, subscribe + bell) | P | 2 |
| Channel groups (local, RSS-fed) + import from NewPipe/Grayjay/PocketTube | P | 5 |
| Content filters: hide Shorts per area, Mixes, watched, upcoming, streams; blocked channels | P | 5 |
| Playlists: create/rename/delete, add/remove, save, Watch Later, sort; editable queue | P | 3 |
| Context menu (play next, queue, mark watched, not interested, don't recommend, remove from history, pin, share/QR, reminder, block…), configurable order | P | 3 |
| Search: suggestions, history, filters (date/duration/type/features/sort), on-screen keyboard | P | 2 (filters 3) |
| Voice search | A: Magic Remote mic via webOS voice service, unproven | 7 |

### Accounts and personal data
| Feature | | Ph |
|---|---|---|
| Device-code sign-in with QR code, multiple accounts, pick account at boot | P (done for one account) | 2 / 5 |
| Account password, separate settings per account, child mode, master/settings password | P | 5 |
| Watch history sync, resume position (local + server), watchtime pings, incognito | P | 2 |
| Like/dislike/subscribe, notifications menu, stream reminders | P | 3 |
| Comments (read, like, replies; write in NewTube), live chat | P | 3 |
| Backup/restore: local file + Google Drive (device code), auto-backup | A: local first, Drive later | 5 |

### Player
| Feature | | Ph |
|---|---|---|
| Quality presets (144p–8K × avc/vp9/av1 × fps × HDR), codec preference, prefer AVC | A: engine A for H.264 ≤1080p, engine B for the rest | 2 / 4 |
| Speed (lists, remember per video/channel) | A: `setPlayRate` in engine A; rate-scaled PTS in engine B (plx does this) | 3 |
| Subtitles incl. auto-generated + **auto-translate** (`tlang`), style, scale, position | A: we render TTML/VTT ourselves over the video | 3 |
| Audio track / original language, audio delay, volume boost | A: MPD AdaptationSets / Feed lane; delay via PTS offset; boost ✗ (hardware volume) | 3 |
| Chapters, chapter notifications | P | 3 |
| Seek preview from storyboards, seek types and intervals, number-key seeks | P | 3 |
| Autoplay/next, suggestions row, repeat modes, shuffle, section as playlist, loop Shorts | P | 3 |
| Sleep timer, screen dimming, screensaver blocking | P (`SDL_VIDEO_ALLOW_SCREENSAVER=0`) | 3 |
| Zoom/aspect/stretch | A: `SDL_webOSSetExportedWindow` src/dst rectangles | 3 |
| Rotate/flip | ✗ (not possible on the video plane), drop | – |
| Auto frame rate | A: webOS does frame-rate matching itself; we declare the stream's rate class in the sink envelope (plx found 60-declared 24p plays at 13 fps) | 4 |
| HDR (VP9.2 / AV1 HDR10) | A: engine B; VP9 PQ on webOS 5+ has no load key, needs device testing | 4 |
| Live streams (DASH/HLS live, live edge, DVR) | A: engine B or SABR; YouTube HLS is broken in LG's hlsdemux2 | 4 |
| Shorts player (vertical, quick skip up/down) | P | 3 |
| Background audio, PiP | ✗: webOS backgrounds the app and we must tear down; drop | – |
| Configurable player buttons, clocks, quality info, likes count | P | 3 |

### Third-party services
| Feature | | Ph |
|---|---|---|
| SponsorBlock (9 categories, per-category action, seek-bar markers, skip once, alternate server, channel exclusions) | P | 2 (basic) / 3 |
| DeArrow (titles, thumbnails, fallback frame) | P | 3 |
| Return YouTube Dislike | P | 3 |
| Unlocalized titles (oEmbed) | P | 5 |

### Remote, system, settings
| Feature | | Ph |
|---|---|---|
| "Link with TV code" (Lounge screen), phone remote control, play-on-TV | P (wire protocol is documented) | 6 |
| DIAL/SSDP discovery | A: the jail may block multicast; test before building | 6 |
| Settings (every SmartTube screen: General, UI, Player, Subtitles, Search, SponsorBlock, DeArrow, Accounts, Remote, Backup, Language, About) | P as declarative forms | 5 |
| Theming, UI scale, grid scale, card styles | P | 5 |
| Localization (SmartTube has ~48 locales) | A: reuse SmartTube's strings (MIT) through plx's i18n | 5 |
| Key remapping, exit behaviour (BACK at root → Home) | A | 2 / 5 |
| Web proxy, DoH DNS, IPv4 preference | A: libcurl options | 5 |
| Updates | A: publish to a Homebrew Channel repo, which handles updates | 7 |
| Leanback launcher channels, Android bridges, Cronet/OkHttp choice, tunneled playback and device tweaks | ✗ | – |
| Downloads (NewTube) | ✗ for now: little storage, and it's a TV | – |

## 4. Phases

Estimates are **my active working time**. Calendar time also depends on how often you can look at
the TV. Phases 0–2 give you a daily driver, phase 4 gets close to SmartTube, and phase 7 never
really ends.

### Phase 0: Foundations (≈3–5 days)
- Repo layout: Cargo workspace (`app`, `yt-core`, `yt-ext`, `playback`, `platform`, vendored
  `ui`/`gfx`/`machine`/`base`), CI on GitHub Actions (Windows runner + zig).
- Sysroot-lite: script that pulls the needed `.so` files from the TV once, plus a symbol checker.
- IPK writer (Rust port of `mkipk.py`), `make`-style tasks: `build`, `ipk`, `install`, `deploy`
  (scp the binary into the installed app directory, as plx does), `run`, `logs`, `close`.
- TV dev tooling modelled on plx `tv-session.sh`:
  - an in-app remote FIFO (`/tmp/ytnative-remote`) so I can press keys myself;
  - screenshots via `com.webos.service.tv.capture`, which composites the video and UI planes, so I
    can check the screen without you;
  - a crash tracer.
- Vendor and trim the plx crates. Get a GLES2 window with text, an image and focus moving with the
  D-pad on the C3 and in the Windows simulator.
- Lifecycle done right:
  - set `SDL_WEBOS_ACCESS_POLICY_KEYS_BACK/EXIT` before `SDL_Init`;
  - read keys at the shifted offsets (+16/+20/+24, BACK = wcode 482);
  - handle background/foreground (events 0x103–0x106);
  - punch through the video plane via `wl_surface.set_opaque_region`;
  - read launch params from `argv[1]`;
  - BACK at root opens Home.

**Exit:** our UI shell runs on the C3, survives HOME→reopen, and I can drive and screenshot it
without you.

### Phase 1: Engine core, `yt-core` (≈4–6 days, mostly testable on the PC)
- Grow the PoC into a library:
  - HTTP via libcurl on the TV (TLS roots fallback as in plx) and reqwest/rustls on Windows;
  - bootstrap (`youtube.com/tv`: client id/secret, version, visitorData);
  - auth with **multiple accounts**.
- Endpoints:
  - content: `browse` (all sections, continuations), `next` (suggestions, chapters, likes,
    comments entry), `search` + suggestions + filters, `guide`;
  - account and history: `account/accounts_list`, history pause/resume/clear;
  - actions: like/subscribe/playlist/feedback/notifications.
- TV response parsers (`tileRenderer`, `lockupViewModel`, shelves, continuations) into our own
  model types, with recorded-fixture tests.
- Player: client ring as data, MPD builder (all codecs, audio tracks), storyboards, caption tracks,
  playback/watchtime pings.
- Runs as a CLI on Windows first, so most of it is verified without the TV.

**Exit:** the CLI lists Home, Subscriptions and search results and builds playable MPDs for 50
sample videos, including music, VEVO, age-gated, live and Shorts, with a pass/fail report.

### Phase 2: MVP, the daily driver (≈1.5–2.5 weeks)
- Screens:
  - Sidebar + Home / Subscriptions / History / Playlists / Channels grids, with thumbnails, image
    cache and infinite scroll;
  - Channel page;
  - Search with on-screen keyboard and suggestions;
  - Sign-in screen (code + QR).
- Player on engine A:
  - HUD with seek bar, play/pause, ±seek, quality cap, title/channel, BACK to close;
  - resume position, history sync, autoplay next;
  - basic SponsorBlock skipping.
- Robustness: client ring fallback, error screens, URL expiry refresh, relaunch handling.

**Exit:** you use it instead of the LG YouTube app for a week.

### Phase 3: SmartTube player and content depth (≈2–3 weeks)
- Player:
  - subtitles (own renderer, styles, auto-translate) and audio tracks;
  - chapters and storyboard seek preview;
  - speed, zoom/aspect, sleep timer;
  - repeat/shuffle/queue;
  - Shorts player with up/down skip.
- Content:
  - the remaining sections, sidebar customisation, full context menu, playlist management;
  - like/dislike/subscribe/bell, notifications, comments, live chat;
  - DeArrow, RYD, full SponsorBlock (markers, per-category actions, exclusions).

### Phase 4: Engine B, 4K/HDR, live, SABR (≈2–4 weeks, highest risk)
- Port plx's Feed engine:
  - `starfish.c`/`.h` (C++ ABI shim, lifecycle safety, quarantine);
  - payload builders and sink envelope;
  - Annex-B/AV1/VP9/ADTS framers;
  - feed loop (1.6 s throttle, priming, `BufferFull` retry, EOS);
  - NativeSeek.
- Plus YouTube-specific work:
  - two demuxers (one file each for video and audio, fMP4 sidx / WebM Cues) over bounded range
    requests, via our own LGPL FFmpeg build or a Rust demuxer;
  - Opus with `opusInfo`;
  - rendition switches at sidx-aligned keyframes.
- Device validation on the C3 first:
  - VP9/AV1 4K loads at the right envelope;
  - HDR10 reaches HDR mode;
  - 24p is presented at 24p.
- Live: DASH live (OTF/segment templates) through engine B, with HLS as a fallback via our own
  TS/fMP4 handling.
- SABR client in Rust: protobuf requests and UMP response parsing. Ported from SmartTube's
  `library/sabr` and NewTube's `sabr-media3`; it feeds engine B. This keeps signed-in TV playback
  and the anonymous clients working once YouTube moves everyone to SABR.

### Phase 5: Settings, accounts and personalisation parity (≈1.5–2 weeks)
- Every SmartTube settings screen as declarative forms.
- Multiple accounts with password and separate settings per account, child mode.
- Content filters, blocked channels, channel groups + imports.
- Theming, scales, card styles.
- Localization via SmartTube's strings.
- Proxy/DoH.
- Backup/restore (local first, then Drive).

### Phase 6: Remote control (≈1 week)
- Lounge screen: pairing code, `bc/bind` long-poll, commands (setPlaylist, play/pause/seek, volume,
  next/prev, subtitles, now playing).
- DIAL server, if the jail allows SSDP.

**Exit:** your phone's YouTube app can cast to ytnative.

### Phase 7: Hardening and release (ongoing)
- Extraction-break playbook:
  - client profiles in a remote JSON;
  - the JS helper service (n-sig) enabled automatically when no JS-less client works;
  - the BotGuard hidden web app if PO tokens are required.
- A daily canary on the PC: the 50-video suite against live YouTube, alerting when something breaks.
- Memory and frame-time budgets on the C3; release builds; Homebrew Channel repo; README/licence
  (GPL-3.0 with source next to the binary).
- Later: voice search, other webOS versions (5–11) through a firmware compatibility check like
  plx's `fwcompat`.

**Totals:**
- MVP (phases 0–2): ≈3–5 weeks of my work.
- Close to SmartTube (phases 0–5): ≈2–3 months.
- With remote control and hardening: ≈3–4 months.

This is more than my earlier "1–2 months" guess. The inventory shows about 40k lines of engine,
30k of app logic and 25k of TV UI in SmartTube, and the plx study shows how much webOS-specific
care each piece needs.

## 5. Risks, worst first

| Risk | Effect | Mitigation |
|---|---|---|
| YouTube closes the JS-less VISIONOS route | No streams without n-sig/PO token | Client ring as data; Node 16 JS service with yt-dlp EJS; SABR client; daily canary |
| PO tokens become mandatory everywhere | Playback breaks for all clients | Hidden web app running BotGuard on WebAppMgr; worst case a companion PO-token server on the PC/NAS |
| YouTube moves everything to SABR | Engine A (MPD) stops working | Phase 4 SABR → engine B; also the signed-in path |
| VP9/AV1/HDR via Feed misbehaves on webOS 10 | No 4K/HDR | Probe first in phase 4; H.264 1080p stays as baseline; HEVC is not offered by YouTube |
| plx `ui` too heavy or too Plex-shaped to vendor | Phase 0/2 slip | Fallback: `gfx` + `focus` + `motion` only, our own small UI (+2–3 weeks) |
| Jail limits (multicast for DIAL, spawning, writable paths) | Remote/JS features blocked | Test each with a throwaway probe app before building the feature |
| Firmware updates on your C3 | Breaks things silently | Pin the symbol checker to the TV's libraries; re-run the suite after an update |
| One maintainer vs YouTube's pace | Breakage lasts days | Keep extraction logic small and data-driven; follow yt-dlp/SmartTube releases |

## 6. Working together

- **You:** look at the TV when I ask, try builds as a daily user from phase 2, decide when two
  routes are possible. You don't need to type commands; I deploy over SSH (key-based), and only to
  `/tmp`, the app's own directory and the user app area. System partitions stay untouched.
- **Me:** everything else. Each phase ends with a short demo on the TV and a written result.
- **Repo:** initialise git now, private at first. Publish (GPL-3.0) when there is something worth
  sharing.

## 7. Immediate next steps (phase 0, week 1)

1. `git init`, Cargo workspace skeleton, licence, CI.
2. Pull the TV's `.so` set; write the symbol checker and the IPK writer.
3. Vendor the plx `base`/`machine`/`gfx`/`ui` crates and get them compiling for armv7 with zig.
4. Fix `tvplay`'s lifecycle and key handling per the plx findings, and fold it into the app shell.
5. TV tooling: remote FIFO, screen capture, logs, crash tracer.

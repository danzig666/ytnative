# Playback engine B ("Feed")

Engine A hands a DASH MPD to `luna://com.webos.media` and lets the firmware's dashdemux2 do the
rest. On the C3 that is H.264 ≤ 1080p in practice: webOS 10 refuses H.264 declared at 4K,
YouTube's 1440p/2160p formats are VP9/AV1 only, and LG's hlsdemux2 stalls on YouTube's TS.

Engine B demuxes YouTube's DASH formats in the app and feeds elementary streams to LG's
in-process `StarfishMediaAPIs` (libplayerAPIs.so.1, `mediaTransportType: BUFFERSTREAM`), the
path plx-native ships for Plex direct play. That opens VP9 and AV1 (up to 2160p60), Opus, and
keeps H.264/AAC.

Status: **built and host-verified; not yet run on a TV.** Everything below the "PC" line in the
checklist at the end is unverified on our set.

## Layout

| Piece | Where | Host-testable |
|---|---|---|
| fMP4 demuxer (`ftyp/moov/sidx/moof/traf/tfhd/tfdt/trun/mdat`, avcC/vpcC/av1C/esds/dOps, `colr`, `elst`) | `crates/yt-core/src/demux/mp4.rs` | yes |
| WebM/Matroska demuxer (EBML, Info, Tracks, Cues, Cluster, SimpleBlock/BlockGroup, Xiph/fixed/EBML lacing, CodecPrivate, CodecDelay, Colour) | `crates/yt-core/src/demux/mkv.rs` | yes |
| Framers: H.264 avcC→Annex-B (+SPS/PPS on IDR), AV1 temporal units (+av1C sequence header on keyframes), VP9 as-is (superframe splitter available), AAC→ADTS, Opus raw + OpusHead | `crates/yt-core/src/demux/framing.rs` | yes |
| Direct-URL access: bounded HTTP Range (≤ 4 MB per request, retries), sidx/Cues → segment byte ranges | `crates/yt-core/src/demux/ranged.rs` | yes (live) |
| Format choice | `crates/yt-core/src/demux/select.rs` | yes |
| Starfish C shim (dlopen'ed libplayerAPIs, callback gate, quarantine) | `crates/app/csrc/starfish.{c,h}` | no (TV) |
| Load payload builder | `crates/app/src/feed/payload.rs` | yes (`rustc --test`, std only) |
| FFI + event routing | `crates/app/src/feed/sys.rs` | no |
| Lanes + fetchers (ranged / SABR) | `crates/app/src/feed/source.rs` | no |
| Feed loop | `crates/app/src/feed/engine.rs` | no |
| Public API (`FeedPlayer`) | `crates/app/src/feed/mod.rs` | no |

Both demuxers are incremental (`Demuxer::push` any byte run, `pop` access units) and can
`reset_to(offset)` a segment boundary for seeks, so the same code serves ranged downloads and
SABR segments. Access units carry `{track, kind, pts_ns, dts_ns, duration_ns, keyframe, data}`.
H.264 keyframes are decided from the NAL types (IDR), not the sample flags: YouTube marks
mid-fragment IDRs as non-sync samples (seen in SABR segments).

## Public API

```rust
// crates/app/src/feed/mod.rs
pub struct Prefs { pub max_height: u32, pub codec: VideoCodec /* H264|Vp9|Av1 */, pub allow_hdr: bool,
                   pub transport: Transport /* Direct | Sabr(TvConfig) */ }
impl Prefs { pub fn new(max_height: u32, codec: VideoCodec) -> Prefs }

impl FeedPlayer {
    pub fn new() -> Self;
    pub fn available() -> bool;                       // libplayerAPIs ok + nothing quarantined
    pub fn choose(resolved: &Resolved, prefs: &Prefs) -> Option<select::Choice>;
    pub fn start(&mut self, video_id: String, resolved: Resolved, prefs: Prefs,
                 window_id: Option<String> /* None: create an exported window */,
                 start_ms: Option<u64>) -> anyhow::Result<()>;
    pub fn tick(&mut self);                           // main thread, per frame (window placement)
    pub fn active(&self) -> bool;
    pub fn toggle_pause(&mut self); pub fn pause(&mut self); pub fn resume(&mut self);
    pub fn seek_to(&mut self, ms: u64); pub fn seek_by(&mut self, delta_ms: i64);
    pub fn set_rate(&mut self, rate: f32) -> bool;    // always false: unsupported (below)
    pub fn set_volume(&mut self, v: i32);
    pub fn stop(&mut self);                           // unload → (destroy | quarantine) → window
    pub status: Arc<Mutex<crate::player::Status>>,    // the same Status engine A publishes
}

// crates/yt-core/src/demux/select.rs
pub fn choose(adaptive: &[Value], r: Request{max_height, codec, allow_hdr, need_url}) -> Option<Choice>
```

The host does not call engine B yet; wiring is one branch in `host.rs` (choose B when
`FeedPlayer::available()` and the wanted height is > 1080 or the codec is VP9/AV1, else A).
Extras (captions, SponsorBlock, ...) stay the host's job as with engine A.

## Format choice

`select::choose`: H.264 is capped at 1080p (webOS 10 refuses H.264 declared at 4K; YouTube has no
H.264 above 1080p worth having). Candidates in order: the requested codec, then VP9, AV1, H.264
(> 1080p requested) or H.264, VP9, AV1 (≤ 1080p). The highest height wins; at equal height the
earlier codec; within a codec, fps then bitrate. HDR formats only with `allow_hdr` (then preferred
at equal height). Audio: Opus (251) with VP9/AV1, AAC (140) with H.264, each falling back to the
other. `need_url` (ranged mode) skips formats without a direct URL.

## Load payload

plx-native's `PAYLOAD_AV` template, codecs and envelope spliced in (`payload::build`):

- `codec.video` = `H264` | `VP9` | `AV1`; `codec.audio` = `AAC` | `OPUS` (none: `needAudio:false`).
- `esInfo.pauseAtDecodeTime: false` — with `true` and no decode-time trigger nothing is decoded.
- `esInfo.videoFpsValue/videoFpsScale` (24000/1001 etc.) + `adaptiveResolution: true` when the
  rate is known (from the container's sample duration, else the format's `fps`).
- Sink envelope `adaptiveStreaming.maxWidth/maxHeight/maxFrameRate`: H.264 that fits FHD
  (≤ 1920x1088) declares **1920x1080** and its **rate class** (24/25/30/50/60; plx measured 601
  "Resource Allocation Error" for H.264 at 4K60 on webOS 10.3.1, and 13 fps presentation for 24p
  declared @60 on webOS 4.10). VP9/AV1 declare 3840x2160@60 (HEVC's verified rule, unmeasured for
  these codecs).
- Opus: `contents.opusInfo {channels, sampleRate: 48, streamHeader: base64(OpusHead)}` with the
  container's real OpusHead (WebM CodecPrivate / MP4 `dOps` converted).
- `option.windowId` = the SDL exported window id (webOS 5+ binding).
- `provider` is `"ytnative"` (plx sends `"plxnative"`; believed free-form).

## Feed loop (`engine.rs`)

One feed thread per session makes every Starfish call (Load, Feed, Play, Pause, Seek, pushEOS,
Unload), so a Seek never races a Feed. Fetcher threads fill two lanes (bounded queues of framed
units; read-ahead 30 s / 48 MB video, 6 MB audio).

1. Wait for both tracks and the first video keyframe (≤ 30 s), build the payload, Load, wait for
   `loadCompleted` (≤ 10 s; type-18 error before it = refused).
2. Fed timeline: `fed = pts - shift`, `shift` = the first keyframe's PTS, so feeding starts at 0.
3. Prime: feed (paused) until 700 ms video + 300 ms audio (or 2.5 s video alone, or the stream
   ends), then `Play`.
4. Throttle: video ≤ 1.6 s ahead of the presented position (FRAMEREADY's fed PTS, a ~5 Hz tick),
   audio ≤ 1.6 + 2 s; before the first position report, ≤ 4 s past the anchor (then a wall-clock
   estimate). `BufferFull` keeps the unit pending and retries next tick (5 ms idle sleep). A lane
   spends ≤ 3 ms per tick once it is ≥ 250 ms ahead.
5. Audio before the video anchor is dropped.
6. When both lanes are drained: `pushEOS`; callback type 28 → `Phase::Ended`.
7. Position = FRAMEREADY PTS + shift → `Status::position_ms`; first FRAMEREADY → `Playing`.
8. Errors: type 18 before any frame → `Failed` (e.g. 601 resource allocation); 600 after
   frames ("decoder revoked", plx) → `Failed`; others logged.

**Seek** (`seek_to`): Pause, bump the generation, reset both lanes; fetchers reposition to the
segment containing the target (ranged) or `SabrStream::seek` (SABR) and drop video before the
last keyframe ≤ target. On the first unit (the keyframe `K`): `Seek("<K ms>")` (LG's public seek:
flushes and re-anchors the segment at K; **no flush after it**), `shift = 0` (content time, as
plx's NativeSeek), re-prime, Play unless the user paused. If `Seek` is refused: unload and reload
per seek for the rest of the session. The picture lands on the keyframe at/before the target
(YouTube GOPs are ≤ ~4 s).

**Pause/resume**: `Pause()` / `Play()`; feeding continues under the throttle.

**Rate**: `StarfishMediaAPIs::SetPlayRate(const char*)` with `{"playRate":r,"audioOutput":true}`
(what webOS's browser sends for a `<video>` playbackRate), applied once playing and again after
every Play (a reload starts at 1x); the wall-clock position fallback is scaled by the rate. If the
TV refuses it (plx-native reported BUFFERSTREAM pipelines may), the engine sets
`Status::rate_refused`; the host then restarts the video at the same position on engine A (which
has `setPlayRate`) and sends later videos wanting a speed other than 1x there for the rest of the
run.

**Teardown** (`stop`): the feed thread `Unload`s, waits ≤ 3 s for callback type 23 (unload
completed), retires the callback gate (waits for in-flight callbacks), and lets the shim destruct
the object only with all proofs (gate retired, ≥ 1 intercepted callback, type 23); otherwise the
object is **quarantined** for the life of the process (never destructed, address never reused,
every further Load refused → `available()` turns false → use engine A). The exported window is
destroyed only after the feed thread has finished (never before Unload completes).

## Starfish shim (`csrc/starfish.c`)

plx-native's `src/starfish.c` (GPL-3.0-or-later, header and attribution kept), changed to:
- resolve every method with `dlopen("libplayerAPIs.so.1")` + `dlsym` (no import of any mangled
  method, so the Feed-ABI variants (cxx11 / COW) and missing methods are handled at runtime;
  `sf_available()` = 0 when something required is missing);
- keep only the public verbs (ctor/dtor, `Load` with callback context, `Feed` cxx11/COW, `Play`,
  `Pause`, `Seek`, `pushEOS`, `flush`, `setVolume`, `Unload`, `isLoadCompleted`,
  `notifyForeground`); the private-offset in-place seek and ACB code are not ported;
- log through Rust (`yt_sf_log`).

**Linking.** The callback gate needs the executable to export its interposer
`_ZN17StarfishMediaAPIs20callbackFunctionHookEixPKc` so it preempts the library's definition.
zig's linker driver rejects `--export-dynamic-symbol` / `--dynamic-list`, `-rdynamic` would
export compiler-rt/libm symbols (memcpy, sin, ...) over the TV's libc, and the driver ignores
`--as-needed` and `--whole-archive`. What works: `libplayerAPIs.so.1` on the link line (lld then
exports an executable definition of a symbol the DSO also defines) and a `#[used]` reference to
the shim (`feed/sys.rs`) so `starfish.o` is always pulled from its archive. Consequence:
**libplayerAPIs.so.1 is a DT_NEEDED of the app** (present on every webOS release plx-native
inventoried, 1.2 to 11.2); its methods are still only reached through `dlsym`. Verified on the
PC: the release binary's `.dynsym` has the interposer DEF, and `cargo xtask symcheck` passes
(199 imports, all provided by `sysroot/`). At runtime the shim logs whether the interposer is the
global definition (`interposer_exported=1`); if not, every object is quarantined after Unload.

The shim is compiled only for the TV target (zig via `tools/zigcc.py`); host builds use stubs.

## What is verified on the PC (2026-11-12)

- `cargo test -p yt-core demux`: 10 offline tests (synthetic fMP4 with sidx/elst/trun flags/ctts
  pushed in 7-byte chunks; synthetic WebM with unknown-size Segment, Cues, SimpleBlock,
  BlockGroup, Xiph and EBML lacing; AV1 TU framing; VP9 superframe/keyframe; ADTS; dOps→OpusHead;
  format choice rules).
- `cargo test -p yt-core demux::live -- --ignored --test-threads=1` (Big Buck Bunny `aqz-KE-bpKQ`,
  VISIONOS direct URLs; ~10–13 s each): for each format, our access units equal ffprobe's packets
  of the same downloaded bytes (count, every PTS, keyframe count); the framed elementary stream
  (`.h264` Annex-B / `.ivf` / `.aac` ADTS) is counted frame-for-frame by ffprobe and decodes with
  no ffmpeg error; one segment re-fetched by index (the seek path) equals the linear pass and
  starts on a keyframe. Formats: 299 H.264 1080p60, 303 VP9 1080p60, 315 VP9 2160p60, 399 AV1
  1080p60, 401 AV1 2160p60, 140 AAC, 251 Opus; SABR: 299+251 and 399+140 (~20 s).
- Payload builder: `rustc --edition 2021 --test crates/app/src/feed/payload.rs` (5 tests).
- `cargo check -p ytnative-app --target armv7-unknown-linux-gnueabi`, `cargo xtask build`,
  `cargo xtask symcheck` (OK: 199 imported symbols, all provided by `sysroot/`; new ones are
  `dlopen`/`dladdr` and pthread condvars, plus DT_NEEDED libplayerAPIs.so.1).
- H.264 keyframes: we flag IDRs only. ffprobe also counts recovery-point I-frames (SEI type 6 +
  non-IDR slice), which YouTube's SABR fragments contain (`sabr_h264_opus`: 4 IDR vs ffprobe 5);
  seeks land on IDRs.

## TV validation checklist (nothing here has run on a TV yet)

Log lines to look for are prefixed `feed:` / `starfish:`.

0. Boot / first play: `starfish: libplayerAPIs ok=1 feed=cxx11 ... interposer_exported=1 gate=1`.
   If `interposer_exported=0`, playback works once, then the object is quarantined. The app now
   needs libplayerAPIs.so.1 to start (DT_NEEDED): confirm it still launches.
1. **H.264 1080p60 + AAC** (itag 299+140): Load accepted (declared 1920x1080@60), `loadCompleted`,
   `primed`, picture + sound, position advances, A/V in sync.
2. **VP9 2160p60 + Opus** (315+251): Load accepted with `VP9`/`OPUS` + opusInfo; picture at 4K
   (check `smp_cb` videoInfo / TV info banner), no stutter at 60 fps; Opus audio audible and in sync.
3. **AV1 2160p** (401+251, and 401+140): decodes (the C3's AV1 decoder; temporal units with the
   sequence header in front of keyframes).
4. **Opus vs AAC** each with VP9 1080p (303+251, 303+140).
5. **Seek** forward/back (±10 s, to 0, near the end): `Seek(<ms>) -> 1`, lands on the keyframe,
   picture and audio resume, no stale-segment stall. If `Seek -> 0`, the reload fallback works.
   Check seeking while paused stays paused.
6. **Pause/resume**: freezes/continues; position stops/continues; long pause (screensaver) →
   type 18 num 600 handled.
7. **EOS**: play the last 20 s: `pushEOS`, callback type 28, `Phase::Ended`, no hang.
8. **Stop/teardown**: `callback gate ... proven=1`, `pipeline closed (destroyed=true)`; start a
   second video in the same app run (must Load again, i.e. no quarantine).
9. **HDR**: VP9.2 PQ (e.g. itag 337) and AV1 PQ (e.g. 701) with `allow_hdr`: does the TV switch to
   HDR (picture mode badge / `picture.dynamicRange`)? plx found that on webOS 4 a VP9 PQ stream
   stayed SDR because the bitstream has no transfer characteristics and the fix went through ACB
   (absent on webOS 5+). AV1 carries colour info in its sequence header, so it may switch on its
   own. Record which do.
10. **Envelope**: VP9/AV1 1080p declared at 3840x2160@60 — accepted? (If 601, declare the real
    raster for them too.) H.264 720p30 declared 1920x1080@30 — accepted?
11. **SABR transport** (`Transport::Sabr`): same as 1–2 with segments from SABR.
12. Memory: RSS stays bounded during a 10-minute 4K VP9 play (read-ahead caps).

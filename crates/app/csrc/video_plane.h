/* Copyright (c) 2026 Gleb Linnik
 * SPDX-License-Identifier: GPL-3.0-or-later
 * Own interoperability adapter declarations, written from the public ABI contract.
 * LG runtime binaries are not distributed with this file.
 *
 * Ported to ytnative from plx-native (github.com/GLinnik21/plx-native, src/starfish.h, the
 * video-plane half, GPL-3.0-or-later). Changes for ytnative: own header, logging through a Rust
 * callback (yt_sf_log) instead of the boot shim's FILE*. */
/* video_plane.h — the video-plane binding: decoded sink -> display plane, behind flat C verbs.
 * Rust mirror: vendor/plx/rust-modules/platform/src/webos/video_sink.rs (StarfishSink), driven by
 * crates/app/src/vplane.rs on the main thread. */
#ifndef YTNATIVE_VIDEO_PLANE_H
#define YTNATIVE_VIDEO_PLANE_H

/* Two mechanisms, never both, split at exactly webOS 5.0 where LG deleted libAcbAPI and gave SDL
 * an exported-window API instead. `vp_mode` resolves which this television has, once, by trying to
 * dlopen/dlsym each — neither is linked, because naming a library or symbol the device lacks makes
 * the loader kill the process before main. See the long comment in video_plane.c. */
#define VP_NONE     0   /* neither: video cannot be displayed, but the app still runs */
#define VP_ACB      1   /* webOS 2.2.3 .. 4.10.0 — libAcbAPI.so.1 */
#define VP_EXPORTED 2   /* webOS 5.3.1 .. 11.2.0 — SDL_webOS*ExportedWindow* */
int vp_mode(void);

/* VP_EXPORTED only (ytnative's own engines create the exported window themselves; these exist
 * because StarfishSink's vtable names them). */
const char *vp_create_window(void);
const char *vp_window_id(void);     /* "" when none was created; never NULL */
int  vp_place(int src_w, int src_h, int dst_x, int dst_y, int dst_w, int dst_h);
void vp_destroy_window(void);

/* ACB (App Common Binding) — VP_ACB only; every verb is a no-op without a handle. */
long acb_create(const char *appId, int playerType);  /* create+initialize; 0 = failed. At most two per process, none ever destroyed; the newest is current */
int  acb_select(long handle);                        /* make a handle acb_create returned the current one; 1 = done, 0 = not ours */
int  acb_initialized(void);                          /* 1 = the library confirmed AcbAPI_initialize for the current handle */
int  acb_probe_dass_available(void);                 /* developer arm only */
int  acb_media_id(const char *mediaId);              /* setSinkType(MAIN)+setMediaId; 1 = both accepted */
int  acb_loaded(void);                               /* setState(FOREGROUND, LOADED); setState's answer */
int  acb_probe_connect_dass(void);                   /* developer arm only */
/* setDisplayWindow(0,0,1280,720,fullscreen)+setState(PLAYING); 1 = both accepted. *displayTask and
 * *playingTask (either may be NULL) get each call's task id only when that call answered 1. */
int  acb_finish(long *displayTask, long *playingTask);
void acb_bind(const char *mediaId);                  /* setSinkType(MAIN)+setMediaId+setState(LOADED) */
int  acb_send_atmos(const char *mediaId);            /* setMediaAudioData Atmos descriptor */
int  acb_send_video_data(const char *sourceInfo);    /* setMediaVideoData; -1 = rejected client-side */
void acb_start(long x, long y, long w, long h);      /* setDisplayWindow + setState(PLAYING) */
/* setState(UNLOADED): 1 accepted (a completion carrying *taskOut follows), -2 not registered
 * (nothing sent, no callback), 0 no handle. *taskOut is set only when the return is 1, else 0. */
int  acb_unload(long *taskOut);
void acb_pause(void);                                /* setState(FOREGROUND, PAUSED) */
void acb_resume(void);                               /* setState(FOREGROUND, PLAYING) */

/* ---- consumer-defined (Rust) ---- */
/* Completion callback of any ACB call, on the library's thread (vendor video_sink.rs): `id` is the
 * handle acb_create returned for the core that issued the call, `task` that core's task id, `ev`
 * -2 timeout, -1 error, 0 cancel, 1 complete. */
void acb_on_event(long id, long task, long ev, const char *reply);
void yt_sf_log(const char *line);    /* crates/app/src/feed/sys.rs */

#endif /* YTNATIVE_VIDEO_PLANE_H */

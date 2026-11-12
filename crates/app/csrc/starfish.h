/* Copyright (c) 2026 Gleb Linnik
 * SPDX-License-Identifier: GPL-3.0-or-later
 * Own interoperability adapter declarations, written from the public ABI contract.
 * LG runtime binaries are not distributed with this file.
 *
 * Ported to ytnative from plx-native (github.com/GLinnik21/plx-native, src/starfish.h,
 * GPL-3.0-or-later). Changes for ytnative: every StarfishMediaAPIs method is resolved with
 * dlopen/dlsym (no strong reference to a mangled method), the ACB / video-plane / in-place-seek
 * (private offset) verbs are not ported, and logging goes through a Rust callback instead of the
 * boot shim's FILE*. */
/* starfish.h — flat ABI for starfish.c (buffer-feed). LG StarfishMediaAPIs (C++) behind flat C
 * verbs: hides the mangled symbols, the sret std::string in Feed, the never-reused 64 KB in-place
 * objects and the callback gate. Library-thread callbacks are forwarded to sf_on_event (Rust,
 * crates/app/src/feed/sys.rs). */
#ifndef YTNATIVE_STARFISH_H
#define YTNATIVE_STARFISH_H

/* 1 when libplayerAPIs.so.1 loaded and every required symbol resolved (dlopen once). */
int  sf_available(void);
int  sf_load(const char *payload, unsigned int epoch); /* Load callback context owns `epoch` */
/* 1 while the current object is dispatchable; 0 can retain a quarantined object. */
int  sf_ready(void);
/* 1 once a quarantine or a failed gate blocked every future Load in this process. */
int  sf_blocked(void);
int  sf_is_load_completed(void);
int  sf_play(void);
int  sf_pause(void);
int  sf_set_volume(int volume);   /* 0..100 (setVolume {"volume":n}); library bool */
int  sf_flush(void);
int  sf_seek_ms(long long ms);    /* public StarfishMediaAPIs::Seek(millis): flushes + re-anchors at ms; 0 = refused */
int  sf_push_eos(void);
char sf_feed(const unsigned char *p, unsigned size, long long pts, int esData); /* 'O'/'B'/'e' */
int  sf_feed_abi_is_cow(void);
void sf_unload(void);
/* After Unload: close native callback admission and wait for every admitted callbackFunctionHook
 * call. 1 proves this object's ELF interposer was actually crossed. */
int  sf_callback_gate_retire(void);
unsigned int sf_callback_intercepts(void);
int  sf_unload_completed(void);   /* 1 once callback type 23 arrived for the current object */
/* D1 only after gate retirement + intercepted callback + type 23; otherwise quarantines. */
int  sf_destroy(void);
/* Keep the current constructed object forever and permanently reject another Load. */
void sf_quarantine(void);

/* ---- consumer-defined (Rust) ---- */
void sf_on_event(unsigned int epoch, int type, long long num, const char *str);
void yt_sf_log(const char *line);

#endif /* YTNATIVE_STARFISH_H */

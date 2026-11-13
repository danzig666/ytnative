/* Copyright (c) 2026 Gleb Linnik
 * SPDX-License-Identifier: GPL-3.0-or-later
 * Own interoperability adapter, written from the public ABI contract.
 * LG runtime binaries are not distributed with this file.
 *
 * Ported to ytnative from plx-native (github.com/GLinnik21/plx-native, src/starfish.c,
 * GPL-3.0-or-later): the callback gate, the never-reused 64 KB in-place object slots, the
 * quarantine rules and Feed's sret std::string handling are plx-native's, unchanged in substance.
 * ytnative changes: every method is resolved with dlopen/dlsym (no strong reference to any mangled
 * method, so the cxx11/COW Feed variants and missing methods are handled at runtime; the library is
 * still DT_NEEDED, see build.rs, because that is what exports the interposer); the private-offset in-place seek verbs (setTimeToDecode / setContentInfo /
 * sendSegmentEvent) and the ACB/video-plane code are not ported; logging goes to Rust (yt_sf_log).
 *
 * starfish.c — buffer-feed StarfishMediaAPIs C++ ABI seam; see starfish.h. */
#define _GNU_SOURCE
#include "starfish.h"
#include <dlfcn.h>
#include <pthread.h>
#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void sf_log(const char *fmt, ...) {
    char line[512];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(line, sizeof line, fmt, ap);
    va_end(ap);
    yt_sf_log(line);
}

/* ---- LG StarfishMediaAPIs (libplayerAPIs.so.1), resolved at runtime ----
 * `this` is an over-allocated buffer we construct in place (object size unknown, so never
 * new/delete). Methods returning std::string use a hidden sret pointer (first arg); the char* at
 * offset 0 holds the reply ("Ok"/"BufferFull", short enough for the SSO buffer). */
#define SYM_CTOR      "_ZN17StarfishMediaAPIsC1EPKc"
#define SYM_DTOR      "_ZN17StarfishMediaAPIsD1Ev"
#define SYM_PLAY      "_ZN17StarfishMediaAPIs4PlayEv"
#define SYM_PAUSE     "_ZN17StarfishMediaAPIs5PauseEv"
#define SYM_UNLOAD    "_ZN17StarfishMediaAPIs6UnloadEv"
#define SYM_FOREGROUND "_ZN17StarfishMediaAPIs16notifyForegroundEv"
#define SYM_LOADED    "_ZN17StarfishMediaAPIs15isLoadCompletedEv"
#define SYM_FLUSH     "_ZN17StarfishMediaAPIs5flushEv"
#define SYM_SEEK      "_ZN17StarfishMediaAPIs4SeekEPKc"
#define SYM_VOLUME    "_ZN17StarfishMediaAPIs9setVolumeEPKc"
#define SYM_PLAYRATE  "_ZN17StarfishMediaAPIs11SetPlayRateEPKc"
#define SYM_EOS       "_ZN17StarfishMediaAPIs7pushEOSEv"
#define SYM_FEED_CXX11 "_ZN17StarfishMediaAPIs4FeedB5cxx11EPKc"
#define SYM_FEED_COW  "_ZN17StarfishMediaAPIs4FeedEPKc"
#define SYM_COW_DTOR  "_ZNSsD1Ev"
/* Callback-context Load overload, device-proven by plx-native on webOS 4.10.2: stores cb at
 * object+0x9c and ctx at +0xa0, then tail-calls the ordinary Load; sendCallbackEvent dispatches the
 * callback with that exact context. This is what lets Rust reject a late event from a retired Load. */
#define SMP_LOAD_WITH_CONTEXT_SYMBOL "_ZN17StarfishMediaAPIs4LoadEPKcPFvixS1_PvES2_"
#define SMP_CALLBACK_HOOK_SYMBOL "_ZN17StarfishMediaAPIs20callbackFunctionHookEixPKc"

typedef void (*SMP_CtorFn)(void *self, const char *appId);
typedef void (*SMP_VoidFn)(void *self);
typedef int  (*SMP_IntFn)(void *self);
typedef int  (*SMP_StrArgFn)(void *self, const char *arg);
typedef int  (*SMP_LoadWithContextFn)(void *self, const char *payload,
                                      void (*cb)(int, long long, const char *, void *), void *ctx);
typedef void (*SMP_FeedFn)(void *sret, void *self, const char *payload);
typedef void (*CowStringDtorFn)(void *str);
typedef void (*SMP_CallbackHook)(void *, int, long long, const char *);

static struct {
    void *lib;
    SMP_CtorFn ctor;
    SMP_VoidFn dtor;
    SMP_IntFn play, pause, unload, is_loaded, flush, push_eos;
    SMP_VoidFn foreground;
    SMP_StrArgFn seek, set_volume, set_play_rate;
    SMP_LoadWithContextFn load_ctx;
    SMP_FeedFn feed;
    CowStringDtorFn feed_reply_dtor;
    int feed_cow;
    SMP_CallbackHook real_hook;
    int ok;
} L;

static pthread_once_t g_resolve_once = PTHREAD_ONCE_INIT;
static volatile int g_hook_valid;
static volatile int g_lifecycle_blocked;

void sf_callback_hook_interposer(void *self, int type, long long num, const char *str)
    __asm__(SMP_CALLBACK_HOOK_SYMBOL);

static int owned_by_player_apis(void *fn) {
    Dl_info info;
    memset(&info, 0, sizeof info);
    return fn && dladdr(fn, &info) != 0 && info.dli_fname &&
           strstr(info.dli_fname, "libplayerAPIs.so") != NULL;
}

static void sf_resolve_once(void) {
    L.lib = dlopen("libplayerAPIs.so.1", RTLD_LAZY | RTLD_GLOBAL);
    if (!L.lib) {
        const char *e = dlerror();
        sf_log("starfish: dlopen libplayerAPIs.so.1 failed: %s", e ? e : "?");
        return;
    }
#define R(field, type, name) L.field = (type)dlsym(L.lib, name)
    R(ctor, SMP_CtorFn, SYM_CTOR);
    R(dtor, SMP_VoidFn, SYM_DTOR);
    R(play, SMP_IntFn, SYM_PLAY);
    R(pause, SMP_IntFn, SYM_PAUSE);
    R(unload, SMP_IntFn, SYM_UNLOAD);
    R(is_loaded, SMP_IntFn, SYM_LOADED);
    R(flush, SMP_IntFn, SYM_FLUSH);
    R(push_eos, SMP_IntFn, SYM_EOS);
    R(foreground, SMP_VoidFn, SYM_FOREGROUND);
    R(seek, SMP_StrArgFn, SYM_SEEK);
    R(set_volume, SMP_StrArgFn, SYM_VOLUME);
    R(set_play_rate, SMP_StrArgFn, SYM_PLAYRATE);
    R(load_ctx, SMP_LoadWithContextFn, SMP_LOAD_WITH_CONTEXT_SYMBOL);
    R(real_hook, SMP_CallbackHook, SMP_CALLBACK_HOOK_SYMBOL);
#undef R
    /* Feed's mangled name carries the string ABI: cxx11 on webOS 4.4+, copy-on-write before. */
    L.feed = (SMP_FeedFn)dlsym(L.lib, SYM_FEED_CXX11);
    if (!L.feed) {
        L.feed = (SMP_FeedFn)dlsym(L.lib, SYM_FEED_COW);
        if (L.feed) {
            L.feed_cow = 1;
            L.feed_reply_dtor = (CowStringDtorFn)dlsym(RTLD_DEFAULT, SYM_COW_DTOR);
        }
    }
    L.ok = L.ctor && L.dtor && L.play && L.pause && L.unload && L.is_loaded && L.push_eos &&
           L.seek && L.load_ctx && L.feed;
    /* The gate needs the library's own hook (owner-checked) and our interposer to be the one the
     * library's PLT resolves to: the executable exports it (lld exports an executable definition
     * of a symbol a linked DSO also defines; build.rs links libplayerAPIs.so.1 for that), and the
     * main program comes first in the global lookup scope. */
    void *global_hook = dlsym(RTLD_DEFAULT, SMP_CALLBACK_HOOK_SYMBOL);
    int exported = global_hook == (void *)sf_callback_hook_interposer;
    if (L.real_hook && (void *)L.real_hook != (void *)sf_callback_hook_interposer &&
        owned_by_player_apis((void *)L.real_hook) && owned_by_player_apis((void *)L.load_ctx)) {
        __atomic_store_n(&g_hook_valid, 1, __ATOMIC_RELEASE);
    } else {
        __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
    }
    sf_log("starfish: libplayerAPIs ok=%d feed=%s hook=%p interposer_exported=%d gate=%d",
           L.ok, !L.feed ? "NONE" : L.feed_cow ? "cow" : "cxx11", (void *)L.real_hook, exported,
           __atomic_load_n(&g_hook_valid, __ATOMIC_ACQUIRE));
    if (!exported)
        sf_log("starfish: WARNING callbackFunctionHook interposer is not the global definition; "
               "objects will be quarantined after Unload (no destructor proof)");
}

static int sf_resolved(void) {
    pthread_once(&g_resolve_once, sf_resolve_once);
    return L.ok;
}

int sf_available(void) {
    return sf_resolved() && !__atomic_load_n(&g_lifecycle_blocked, __ATOMIC_ACQUIRE);
}

int sf_blocked(void) {
    return __atomic_load_n(&g_lifecycle_blocked, __ATOMIC_ACQUIRE);
}

/* ---- the callback gate (plx-native) ----
 * callbackFunctionHook's ARM32/AAPCS ABI: r0 = StarfishMediaAPIs *this, r1 = int, r2/r3 = int64,
 * stack[0] = const char *. libplayerAPIs' libpf thunk reaches the symbol through its PLT, so the
 * executable's exported definition preempts the library's. The gate counts an admitted call as
 * in-flight across the complete real hook without holding a mutex during it; after `active` is
 * cleared new calls drop before dereferencing the object and teardown waits for in-flight to reach
 * zero. A slot (and its 64 KB object ADDRESS) is never freed or reused: a late libpf thunk carries
 * only that address. */
#define SMP_STORAGE_SIZE 65536u
_Static_assert(sizeof(void *) == 4, "Starfish seam requires the 32-bit ARM ABI");
_Static_assert(sizeof(long long) == 8, "Rust callback i64 width mismatch");

typedef struct SfSlot {
    struct SfSlot *next;
    pthread_mutex_t gate;
    pthread_cond_t drained;
    unsigned int epoch;
    unsigned int intercepted;
    unsigned int dropped;
    unsigned int inflight;
    int active;
    int evidence_armed;
    int gate_retired;
    volatile int unload_completed;
    volatile int destroyed;
    unsigned char object[SMP_STORAGE_SIZE] __attribute__((aligned(16)));
} SfSlot;

static pthread_mutex_t g_slots_lock = PTHREAD_MUTEX_INITIALIZER;
static SfSlot *g_slots;
static SfSlot *g_current;

static SfSlot *sf_find_slot(void *object, int *lookup_ok) {
    *lookup_ok = 0;
    if (pthread_mutex_lock(&g_slots_lock) != 0) return NULL;
    SfSlot *slot = g_slots;
    while (slot && slot->object != (unsigned char *)object) slot = slot->next;
    *lookup_ok = pthread_mutex_unlock(&g_slots_lock) == 0;
    return *lookup_ok ? slot : NULL;
}

#define BLOCK() __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE)

__attribute__((visibility("default"), used))
void sf_callback_hook_interposer(void *self, int type, long long num, const char *str) {
    pthread_once(&g_resolve_once, sf_resolve_once);
    if (!__atomic_load_n(&g_hook_valid, __ATOMIC_ACQUIRE) || !L.real_hook) return;
    int lookup_ok = 0;
    SfSlot *slot = sf_find_slot(self, &lookup_ok);
    if (!lookup_ok) { BLOCK(); return; }
    if (!slot) { L.real_hook(self, type, num, str); return; }   /* someone else's object */
    if (pthread_mutex_lock(&slot->gate) != 0) { BLOCK(); return; }
    if (slot->active) {
        if (slot->evidence_armed) __atomic_add_fetch(&slot->intercepted, 1u, __ATOMIC_RELAXED);
        ++slot->inflight;
    } else {
        __atomic_add_fetch(&slot->dropped, 1u, __ATOMIC_RELAXED);
        if (pthread_mutex_unlock(&slot->gate) != 0) BLOCK();
        return;
    }
    if (pthread_mutex_unlock(&slot->gate) != 0) { BLOCK(); return; }

    L.real_hook(self, type, num, str);

    if (pthread_mutex_lock(&slot->gate) != 0) { BLOCK(); return; }
    if (slot->inflight == 0) BLOCK();
    else if (--slot->inflight == 0 && pthread_cond_broadcast(&slot->drained) != 0) BLOCK();
    if (pthread_mutex_unlock(&slot->gate) != 0) BLOCK();
}

static volatile int g_smp_ready = 0;
#define SMP_READY()      __atomic_load_n(&g_smp_ready, __ATOMIC_ACQUIRE)
#define SMP_SET_READY(v) __atomic_store_n(&g_smp_ready, (v), __ATOMIC_RELEASE)
/* Cleared while the (synchronous, possibly slow) real Load call runs, so no other verb dispatches
 * into an object whose Load is still in progress (plx-native issue #74). */
static volatile int g_load_returned = 0;
#define LOAD_RETURNED()      __atomic_load_n(&g_load_returned, __ATOMIC_ACQUIRE)
#define SET_LOAD_RETURNED(v) __atomic_store_n(&g_load_returned, (v), __ATOMIC_RELEASE)

static SfSlot *sf_current_slot(void) { return __atomic_load_n(&g_current, __ATOMIC_ACQUIRE); }

static void *sf_ready_object(void) {
    SfSlot *slot = sf_current_slot();
    return SMP_READY() && LOAD_RETURNED() && slot ? slot->object : NULL;
}

static void sf_cb(int type, long long num, const char *str, void *ctx) {
    unsigned int epoch = (unsigned int)(uintptr_t)ctx;
    SfSlot *slot = sf_current_slot();
    if (type == 23 && slot && slot->epoch == epoch)
        __atomic_store_n(&slot->unload_completed, 1, __ATOMIC_RELEASE);
    sf_on_event(epoch, type, num, str);
}

int sf_load(const char *payload, unsigned int epoch) {
    if (!sf_resolved() || !payload || epoch == 0 ||
        __atomic_load_n(&g_lifecycle_blocked, __ATOMIC_ACQUIRE) || SMP_READY() ||
        sf_current_slot() || !__atomic_load_n(&g_hook_valid, __ATOMIC_ACQUIRE)) {
        sf_log("starfish: Load refused resolved=%d blocked=%d ready=%d current=%p epoch=%u",
               L.ok, __atomic_load_n(&g_lifecycle_blocked, __ATOMIC_ACQUIRE), SMP_READY(),
               (void *)sf_current_slot(), epoch);
        return 0;
    }
    SfSlot *slot = NULL;
    if (posix_memalign((void **)&slot, 16, sizeof *slot) == 0) memset(slot, 0, sizeof *slot);
    if (!slot || pthread_mutex_init(&slot->gate, NULL) != 0 ||
        pthread_cond_init(&slot->drained, NULL) != 0) {
        BLOCK();
        sf_log("starfish: Load refused (callback gate allocation/init failed)");
        return 0;
    }
    slot->epoch = epoch;
    slot->active = 1;
    if (pthread_mutex_lock(&g_slots_lock) != 0) { BLOCK(); return 0; }
    slot->next = g_slots;
    g_slots = slot;
    if (pthread_mutex_unlock(&g_slots_lock) != 0) { BLOCK(); return 0; }
    __atomic_store_n(&g_current, slot, __ATOMIC_RELEASE);

    L.ctor(slot->object, NULL);   /* uid=NULL: registers on the pre-authorized uMS namespace */
    SET_LOAD_RETURNED(0);
    SMP_SET_READY(1);
    if (L.foreground) L.foreground(slot->object);
    if (pthread_mutex_lock(&slot->gate) != 0) { BLOCK(); SMP_SET_READY(0); return 0; }
    slot->evidence_armed = 1;
    if (pthread_mutex_unlock(&slot->gate) != 0) { BLOCK(); SMP_SET_READY(0); return 0; }
    int ok = L.load_ctx(slot->object, payload, sf_cb, (void *)(uintptr_t)epoch);
    SET_LOAD_RETURNED(1);
    return ok & 0xff;
}

int sf_ready(void) { return SMP_READY(); }

int sf_is_load_completed(void) {
    void *o = sf_ready_object();
    return o ? L.is_loaded(o) & 0xff : 0;
}
int sf_play(void) {
    void *o = sf_ready_object();
    return o ? L.play(o) & 0xff : 0;
}
int sf_pause(void) {
    void *o = sf_ready_object();
    return o ? L.pause(o) & 0xff : 0;
}
int sf_set_volume(int volume) {
    void *o = sf_ready_object();
    if (!o || !L.set_volume) return 0;
    char json[32];
    snprintf(json, sizeof json, "{\"volume\":%d}", volume);
    return L.set_volume(o, json) & 0xff;
}
/* Playback speed: SetPlayRate {"playRate":r,"audioOutput":true} (what webOS's browser sends for a
 * <video> playbackRate; audio keeps playing, pitch-corrected by the pipeline). */
int sf_set_play_rate(double rate) {
    void *o = sf_ready_object();
    if (!o || !L.set_play_rate || !(rate > 0.0)) return 0;
    char json[64];
    snprintf(json, sizeof json, "{\"playRate\":%.2f,\"audioOutput\":true}", rate);
    return L.set_play_rate(o, json) & 0xff;
}
int sf_flush(void) {
    void *o = sf_ready_object();
    return o && L.flush ? L.flush(o) & 0xff : 0;
}
int sf_push_eos(void) {
    void *o = sf_ready_object();
    return o ? L.push_eos(o) & 0xff : 0;
}
/* Public seek: a flushing gst_element_seek whose segment starts at `ms`. Feed PTS on that timeline
 * afterwards and do NOT flush after it (a flush is itself a seek to 0). Not safe against a
 * concurrent sf_feed: the caller quiesces its feeder (ytnative feeds from the same thread). */
int sf_seek_ms(long long ms) {
    void *o = sf_ready_object();
    if (!o || ms < 0) return 0;
    char arg[32];
    snprintf(arg, sizeof arg, "%lld", ms);
    int rv = L.seek(o, arg) & 0xff;
    sf_log("starfish: Seek(%s) -> %d", arg, rv);
    return rv;
}
void sf_unload(void) {
    void *o = sf_ready_object();
    if (o) L.unload(o);
}

int sf_unload_completed(void) {
    SfSlot *slot = sf_current_slot();
    return slot ? __atomic_load_n(&slot->unload_completed, __ATOMIC_ACQUIRE) : 0;
}

int sf_callback_gate_retire(void) {
    SfSlot *slot = sf_current_slot();
    if (!slot || pthread_mutex_lock(&slot->gate) != 0) { BLOCK(); return 0; }
    slot->active = 0;
    int drained = 1;
    while (slot->inflight != 0) {
        if (pthread_cond_wait(&slot->drained, &slot->gate) != 0) { BLOCK(); drained = 0; break; }
    }
    slot->gate_retired = drained && slot->inflight == 0;
    unsigned int intercepted = __atomic_load_n(&slot->intercepted, __ATOMIC_RELAXED);
    unsigned int dropped = __atomic_load_n(&slot->dropped, __ATOMIC_RELAXED);
    int proven = slot->gate_retired && __atomic_load_n(&g_hook_valid, __ATOMIC_ACQUIRE) &&
                 intercepted != 0 && !__atomic_load_n(&g_lifecycle_blocked, __ATOMIC_ACQUIRE);
    if (pthread_mutex_unlock(&slot->gate) != 0) { BLOCK(); proven = 0; }
    sf_log("starfish: callback gate epoch=%u intercepted=%u dropped=%u proven=%d",
           slot->epoch, intercepted, dropped, proven);
    return proven;
}

unsigned int sf_callback_intercepts(void) {
    SfSlot *slot = sf_current_slot();
    return slot ? __atomic_load_n(&slot->intercepted, __ATOMIC_RELAXED) : 0;
}

void sf_quarantine(void) {
    SfSlot *slot = sf_current_slot();
    if (slot) (void)sf_callback_gate_retire();
    BLOCK();
    SMP_SET_READY(0);
    SET_LOAD_RETURNED(0);
    sf_log("starfish: QUARANTINED epoch=%u object=%p; destructor and future Loads disabled",
           slot ? slot->epoch : 0, slot ? (void *)slot->object : NULL);
}

int sf_destroy(void) {
    SfSlot *slot = sf_current_slot();
    if (!slot || !SMP_READY()) return 0;
    if (pthread_mutex_lock(&slot->gate) != 0) { sf_quarantine(); return 0; }
    int safe = !slot->active && slot->gate_retired &&
               __atomic_load_n(&slot->intercepted, __ATOMIC_RELAXED) != 0 &&
               __atomic_load_n(&slot->unload_completed, __ATOMIC_ACQUIRE) &&
               __atomic_load_n(&g_hook_valid, __ATOMIC_ACQUIRE) &&
               !__atomic_load_n(&g_lifecycle_blocked, __ATOMIC_ACQUIRE);
    if (pthread_mutex_unlock(&slot->gate) != 0) safe = 0;
    if (!safe) { sf_quarantine(); return 0; }
    L.dtor(slot->object);
    __atomic_store_n(&slot->destroyed, 1, __ATOMIC_RELEASE);
    SMP_SET_READY(0);
    SET_LOAD_RETURNED(0);
    __atomic_store_n(&g_current, NULL, __ATOMIC_RELEASE);
    return 1;
}

/* Feed one access unit; hides the sret std::string. 'O' Ok, 'B' BufferFull, 'e' error. */
char sf_feed(const unsigned char *p, unsigned size, long long pts, int esData) {
    void *o = sf_ready_object();
    if (!o || !L.feed) return 'e';
    char j[160];
    snprintf(j, sizeof j, "{\"bufferAddr\":\"%p\",\"bufferSize\":%u,\"pts\":%lld,\"esData\":%d}",
             (const void *)p, size, pts, esData);
    unsigned char ret[32];
    memset(ret, 0, sizeof ret);
    L.feed(ret, o, j);
    char *s = *(char **)ret;             /* std::string _M_p at offset 0 */
    static int logged = 0;
    if (logged < 3) { logged++; sf_log("starfish: feed reply=\"%s\"", s ? s : "(null)"); }
    char verdict = !s ? 'e' : strstr(s, "BufferFull") ? 'B' : strstr(s, "Ok") ? 'O' : 'e';
    if (s && L.feed_reply_dtor) L.feed_reply_dtor(ret);
    /* cxx11 string without a resolved destructor: a reply longer than the SSO buffer (at offset 8
       on 32-bit libstdc++) was heap-allocated by operator new (malloc); free it. */
    else if (s && !L.feed_cow && s != (char *)(ret + 8)) free(s);
    return verdict;
}

int sf_feed_abi_is_cow(void) {
    sf_resolved();
    return L.feed_cow;
}

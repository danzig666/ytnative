/* One-shot JavaScript evaluation on QuickJS (vendor/quickjs) for yt-core's player challenge
 * solver: plain C entry points, so Rust needs no bindings to QuickJS's inline JSValue helpers. */
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include "quickjs.h"

struct deadline {
    time_t end;
};

static int interrupt(JSContext *ctx, void *opaque) {
    (void)ctx;
    const struct deadline *d = opaque;
    return d->end != 0 && time(NULL) > d->end;
}

/* Copies `s` (len bytes) to a fresh malloc'd, NUL-terminated buffer. */
static char *dup_len(const char *s, size_t len) {
    char *p = malloc(len + 1);
    if (p) {
        memcpy(p, s, len);
        p[len] = 0;
    }
    return p;
}

static char *value_string(JSContext *ctx, JSValueConst v, size_t *out_len) {
    size_t len = 0;
    const char *s = JS_ToCStringLen(ctx, &len, v);
    if (!s) {
        *out_len = 0;
        return NULL;
    }
    char *p = dup_len(s, len);
    JS_FreeCString(ctx, s);
    *out_len = p ? len : 0;
    return p;
}

/* Evaluates `code` (len bytes; code[len] must be 0) as a global script and returns the string
 * value of its last expression in *out (free with ytjs_free). Returns 0 on success; 1 on a JS
 * exception, *out then holding its message and stack; 2 when the engine could not be set up.
 * `timeout_secs` > 0 interrupts a script still running after that many seconds; `mem_limit`
 * (bytes, 0 = none) and `stack_limit` (bytes) bound the runtime. */
int ytjs_eval(const char *code, size_t len, unsigned timeout_secs, size_t mem_limit, size_t stack_limit,
              char **out, size_t *out_len) {
    *out = NULL;
    *out_len = 0;
    JSRuntime *rt = JS_NewRuntime();
    if (!rt)
        return 2;
    if (mem_limit)
        JS_SetMemoryLimit(rt, mem_limit);
    JS_SetMaxStackSize(rt, stack_limit);
    struct deadline d = { timeout_secs ? time(NULL) + (time_t)timeout_secs : 0 };
    JS_SetInterruptHandler(rt, interrupt, &d);
    JSContext *ctx = JS_NewContext(rt);
    if (!ctx) {
        JS_FreeRuntime(rt);
        return 2;
    }
    int rc;
    JSValue v = JS_Eval(ctx, code, len, "<solver>", JS_EVAL_TYPE_GLOBAL);
    if (JS_IsException(v)) {
        JSValue e = JS_GetException(ctx);
        size_t ml = 0, sl = 0;
        char *msg = value_string(ctx, e, &ml);
        char *stack = NULL;
        if (JS_IsError(e)) {
            JSValue st = JS_GetPropertyStr(ctx, e, "stack");
            if (!JS_IsUndefined(st))
                stack = value_string(ctx, st, &sl);
            JS_FreeValue(ctx, st);
        }
        JS_FreeValue(ctx, e);
        const char *m = msg ? msg : "unknown exception";
        size_t mlen = msg ? ml : strlen(m);
        size_t total = mlen + (stack ? 1 + sl : 0);
        char *p = malloc(total + 1);
        if (p) {
            memcpy(p, m, mlen);
            if (stack) {
                p[mlen] = '\n';
                memcpy(p + mlen + 1, stack, sl);
            }
            p[total] = 0;
            *out = p;
            *out_len = total;
        }
        free(msg);
        free(stack);
        rc = 1;
    } else {
        *out = value_string(ctx, v, out_len);
        rc = *out ? 0 : 1;
    }
    JS_FreeValue(ctx, v);
    JS_FreeContext(ctx);
    JS_FreeRuntime(rt);
    return rc;
}

void ytjs_free(char *p) {
    free(p);
}

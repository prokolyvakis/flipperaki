#include "buddy_rpc.h"

#include <string.h>

// Lifetime and ordering, from Momentum mntm-012 applications/services/rpc
// (rpc_app.c, rpc.c at e1784e7):
//
// - DataExchange/AppExit/Button*/LoadFile callbacks run on the RPC session worker
//   thread, one command at a time; the firmware records last_command_id before the
//   callback and rpc_system_app_confirm() must answer it exactly once (a second
//   confirm trips furi_check). We confirm inside the callback, so a later command
//   can never overwrite an unanswered one.
// - The DataExchange payload points into the decoded PB_Main, released after the
//   handler returns: it is copied into owned storage before returning.
// - SessionClose comes from rpc_system_app_free() on the timer service thread
//   (furi_timer_pending_callback) after the worker thread has stopped, so it never
//   overlaps another callback. rpc_system_app_free() then spins until the callback
//   is NULL and frees the RpcAppSystem: after unregistering we never touch it again.
// - rpc_system_app_set_callback() is a plain store; it does NOT wait for a callback
//   already running. Hence every use of `rpc` outside a callback, and every
//   unregister, happens under `lock`, and the callback checks `closing` under the
//   same lock. Lock order is always ours -> the session's callbacks_mutex (taken in
//   rpc_send), on every thread, so it cannot invert.
// - Residual risk (local exit only): a worker callback that loaded the old callback
//   pointer just before a local unregister could still enter after the unregister.
//   The firmware offers no barrier for this; buddy_rpc_free() waits a grace period
//   before freeing to make the window practically unreachable. Host-initiated
//   exit (AppExit) and SessionClose unregister from inside the callback and are free
//   of it.

#define INGRESS_DEPTH 4

struct BuddyRpc {
    RpcAppSystem* rpc; // valid while !closing, and inside any callback invocation
    FuriEventLoop* loop;
    FuriMessageQueue* queue;
    FuriMutex* lock;
    bool closing;
    bool closed_locally; // unregistered outside a callback: needs the grace period
    BuddyIngress scratch; // callback-side copy, off the 3 KiB RPC worker stack
};

// Caller holds lock. Order: no new sends, tell the host, then unregister last.
static void unregister_locked(BuddyRpc* r, bool report_exit) {
    if(r->closing) return;
    r->closing = true;
    if(report_exit) rpc_system_app_send_exited(r->rpc);
    rpc_system_app_set_callback(r->rpc, NULL, NULL);
}

static void buddy_rpc_callback(const RpcAppSystemEvent* event, void* context) {
    BuddyRpc* r = context;
    furi_check(furi_mutex_acquire(r->lock, FuriWaitForever) == FuriStatusOk);

    switch(event->type) {
    case RpcAppEventTypeSessionClose:
        // Priority path: the transport is gone and nothing may be confirmed or sent.
        unregister_locked(r, false);
        furi_event_loop_stop(r->loop);
        break;
    case RpcAppEventTypeAppExit:
        rpc_system_app_confirm(r->rpc, true);
        unregister_locked(r, true);
        furi_event_loop_stop(r->loop);
        break;
    case RpcAppEventTypeDataExchange: {
        bool accepted = false;
        size_t size = event->data.bytes.size;
        if(!r->closing && event->data.type == RpcAppSystemEventDataTypeBytes &&
           event->data.bytes.ptr != NULL && size > 0 && size <= BUDDY_MESSAGE_MAX_BYTES) {
            r->scratch.size = (uint16_t)size;
            memcpy(r->scratch.bytes, event->data.bytes.ptr, size);
            // Never block the RPC worker: a full queue is an explicit reject.
            accepted = furi_message_queue_put(r->queue, &r->scratch, 0) == FuriStatusOk;
        }
        rpc_system_app_confirm(r->rpc, accepted);
        break;
    }
    case RpcAppEventTypeLoadFile:
    case RpcAppEventTypeButtonPress:
    case RpcAppEventTypeButtonRelease:
    case RpcAppEventTypeButtonPressRelease:
        rpc_system_app_confirm(r->rpc, false); // not supported; still answered once
        break;
    default:
        break; // not a command: nothing to confirm
    }

    furi_mutex_release(r->lock);
}

BuddyRpc* buddy_rpc_alloc(
    RpcAppSystem* rpc,
    FuriEventLoop* loop,
    FuriEventLoopEventCallback on_ingress,
    void* context) {
    BuddyRpc* r = malloc(sizeof(BuddyRpc));
    memset(r, 0, sizeof(*r));
    r->rpc = rpc;
    r->loop = loop;
    r->queue = furi_message_queue_alloc(INGRESS_DEPTH, sizeof(BuddyIngress));
    r->lock = furi_mutex_alloc(FuriMutexTypeNormal);
    furi_event_loop_subscribe_message_queue(
        loop, r->queue, FuriEventLoopEventIn, on_ingress, context);
    // Register last, once everything the callback touches exists.
    rpc_system_app_set_callback(rpc, buddy_rpc_callback, r);
    rpc_system_app_send_started(rpc);
    return r;
}

bool buddy_rpc_receive(BuddyRpc* r, BuddyIngress* out) {
    return furi_message_queue_get(r->queue, out, 0) == FuriStatusOk;
}

bool buddy_rpc_send(BuddyRpc* r, const uint8_t* data, size_t size) {
    furi_check(furi_mutex_acquire(r->lock, FuriWaitForever) == FuriStatusOk);
    bool ok = !r->closing;
    if(ok) rpc_system_app_exchange_data(r->rpc, data, size);
    furi_mutex_release(r->lock);
    return ok;
}

bool buddy_rpc_is_closing(BuddyRpc* r) {
    furi_check(furi_mutex_acquire(r->lock, FuriWaitForever) == FuriStatusOk);
    bool closing = r->closing;
    furi_mutex_release(r->lock);
    return closing;
}

void buddy_rpc_close(BuddyRpc* r) {
    furi_check(furi_mutex_acquire(r->lock, FuriWaitForever) == FuriStatusOk);
    if(!r->closing) r->closed_locally = true;
    unregister_locked(r, true);
    furi_mutex_release(r->lock);
}

// shortcut: the firmware has no barrier for a callback that loaded the old
// pointer just before a local unregister, so a local close waits this long
// before freeing; the RPC worker would have to be starved for the whole period
// to still enter. Replace with a synchronous unregister if the SDK gains one.
#define LOCAL_UNREGISTER_GRACE_MS 100

void buddy_rpc_free(BuddyRpc* r) {
    buddy_rpc_close(r);
    if(r->closed_locally) furi_delay_ms(LOCAL_UNREGISTER_GRACE_MS);
    furi_event_loop_unsubscribe(r->loop, r->queue);
    furi_message_queue_free(r->queue);
    furi_mutex_free(r->lock);
    free(r);
}

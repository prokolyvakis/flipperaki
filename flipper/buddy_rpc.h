#pragma once
// Ownership boundary between the firmware RPC app system and the app thread.

#include <furi.h>
#include <rpc/rpc_app.h>

#include "buddy_protocol.h"

typedef struct {
    uint16_t size;
    uint8_t bytes[BUDDY_MESSAGE_MAX_BYTES];
} BuddyIngress;

typedef struct BuddyRpc BuddyRpc;

// Subscribes the ingress queue to `loop` (calling on_ingress on the loop thread when
// a message is waiting), registers the RPC callback and reports the app started.
// Must be called on the thread that owns `loop`.
BuddyRpc* buddy_rpc_alloc(
    RpcAppSystem* rpc,
    FuriEventLoop* loop,
    FuriEventLoopEventCallback on_ingress,
    void* context);

// Takes one queued host message without blocking.
bool buddy_rpc_receive(BuddyRpc* rpc, BuddyIngress* out);

// Sends one encoded Envelope unless the link is closing. Returns false if dropped.
bool buddy_rpc_send(BuddyRpc* rpc, const uint8_t* data, size_t size);

bool buddy_rpc_is_closing(BuddyRpc* rpc);

// Local exit: reports the app exited and unregisters. Idempotent.
void buddy_rpc_close(BuddyRpc* rpc);

// After the event loop has stopped: closes, unsubscribes and frees.
void buddy_rpc_free(BuddyRpc* rpc);

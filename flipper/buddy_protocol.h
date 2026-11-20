#pragma once
// nanopb encode/decode boundary for the Buddy Envelope. Portable: no SDK headers.

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "buddy.pb.h"

#define BUDDY_PROTOCOL_MAJOR 1u
// Maximum encoded Envelope, checked on both sides (src/limits.rs).
#define BUDDY_MESSAGE_MAX_BYTES 512u

// Decodes into *out (zeroed first). Fails on empty, oversized or malformed input.
bool buddy_protocol_decode(const uint8_t* data, size_t size, flipperaki_buddy_Envelope* out);

// Encodes into buf; *size receives the length. Fails if it would exceed
// BUDDY_MESSAGE_MAX_BYTES or buf_size.
bool buddy_protocol_encode(
    const flipperaki_buddy_Envelope* env,
    uint8_t* buf,
    size_t buf_size,
    size_t* size);

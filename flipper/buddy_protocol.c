#include "buddy_protocol.h"

#include <pb_decode.h>
#include <pb_encode.h>
#include <string.h>

_Static_assert(
    flipperaki_buddy_Envelope_size <= BUDDY_MESSAGE_MAX_BYTES,
    "generated Envelope max size exceeds the protocol limit");

bool buddy_protocol_decode(const uint8_t* data, size_t size, flipperaki_buddy_Envelope* out) {
    memset(out, 0, sizeof(*out));
    if(data == NULL || size == 0 || size > BUDDY_MESSAGE_MAX_BYTES) return false;
    pb_istream_t stream = pb_istream_from_buffer(data, size);
    if(!pb_decode(&stream, &flipperaki_buddy_Envelope_msg, out)) {
        memset(out, 0, sizeof(*out));
        return false;
    }
    return true;
}

bool buddy_protocol_encode(
    const flipperaki_buddy_Envelope* env,
    uint8_t* buf,
    size_t buf_size,
    size_t* size) {
    size_t limit = buf_size < BUDDY_MESSAGE_MAX_BYTES ? buf_size : BUDDY_MESSAGE_MAX_BYTES;
    pb_ostream_t stream = pb_ostream_from_buffer(buf, limit);
    if(!pb_encode(&stream, &flipperaki_buddy_Envelope_msg, env)) return false;
    *size = stream.bytes_written;
    return true;
}

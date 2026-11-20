// C-encoded cross-language fixtures (protocol/fixtures/c/), decoded by the Rust tests.
// Usage: buddy_fixtures --write DIR | --check DIR
// Semantics of each file are listed in protocol/fixtures/README.md.

#include <stdio.h>
#include <string.h>

#include "buddy_protocol.h"

typedef flipperaki_buddy_Envelope Envelope;

static Envelope env(uint32_t sequence, pb_size_t which) {
    Envelope e = flipperaki_buddy_Envelope_init_zero;
    e.protocol_major = BUDDY_PROTOCOL_MAJOR;
    e.epoch = 7;
    e.sequence = sequence;
    e.which_payload = which;
    return e;
}

static Envelope decision(flipperaki_buddy_Verdict verdict) {
    Envelope e = env(2, flipperaki_buddy_Envelope_decision_tag);
    e.payload.decision.request_id = 42;
    e.payload.decision.verdict = verdict;
    return e;
}

static size_t encode(const Envelope* e, uint8_t* buf) {
    size_t n = 0;
    return buddy_protocol_encode(e, buf, BUDDY_MESSAGE_MAX_BYTES, &n) ? n : 0;
}

static size_t make(const char* name, uint8_t* buf) {
    if(strcmp(name, "hello_buddy") == 0) {
        Envelope e = env(1, flipperaki_buddy_Envelope_hello_tag);
        e.payload.hello.role = flipperaki_buddy_Role_ROLE_BUDDY;
        e.payload.hello.max_message_bytes = BUDDY_MESSAGE_MAX_BYTES;
        return encode(&e, buf);
    }
    if(strcmp(name, "decision_approve") == 0) {
        Envelope e = decision(flipperaki_buddy_Verdict_VERDICT_APPROVE);
        return encode(&e, buf);
    }
    if(strcmp(name, "decision_deny") == 0) {
        Envelope e = decision(flipperaki_buddy_Verdict_VERDICT_DENY);
        return encode(&e, buf);
    }
    if(strcmp(name, "decision_unspecified") == 0) {
        Envelope e = decision(flipperaki_buddy_Verdict_VERDICT_UNSPECIFIED);
        return encode(&e, buf);
    }
    if(strcmp(name, "invalid_version") == 0) {
        Envelope e = decision(flipperaki_buddy_Verdict_VERDICT_DENY);
        e.protocol_major = 2;
        return encode(&e, buf);
    }
    if(strcmp(name, "invalid_truncated") == 0) {
        Envelope e = decision(flipperaki_buddy_Verdict_VERDICT_DENY);
        size_t n = encode(&e, buf);
        return n - 3; // cuts inside the Decision submessage
    }
    if(strcmp(name, "invalid_label_too_long") == 0) {
        // Snapshot WORKING with a 30-byte session_label; the schema bound is 24.
        const uint8_t head[] = {0x08, 0x01, 0x10, 0x07, 0x18, 0x02, 0x5a, 0x22,
                                0x08, 0x02, 0x12, 0x1e};
        memcpy(buf, head, sizeof(head));
        memset(buf + sizeof(head), 'a', 30);
        return sizeof(head) + 30;
    }
    if(strcmp(name, "invalid_oversized") == 0) {
        // hello_buddy plus an unknown length-delimited field 100: 513 bytes total.
        size_t n = make("hello_buddy", buf);
        size_t pad = BUDDY_MESSAGE_MAX_BYTES + 1 - n - 4;
        buf[n++] = 0xa2;
        buf[n++] = 0x06;
        buf[n++] = (uint8_t)(0x80 | (pad & 0x7f));
        buf[n++] = (uint8_t)(pad >> 7);
        memset(buf + n, 0, pad);
        return n + pad;
    }
    return 0;
}

static const char* const names[] = {
    "hello_buddy",
    "decision_approve",
    "decision_deny",
    "decision_unspecified",
    "invalid_version",
    "invalid_truncated",
    "invalid_label_too_long",
    "invalid_oversized",
};

static int handle(const char* mode, const char* dir, const char* name, const uint8_t* b, size_t n) {
    char path[512];
    snprintf(path, sizeof(path), "%s/%s.bin", dir, name);
    if(strcmp(mode, "--write") == 0) {
        FILE* f = fopen(path, "wb");
        if(f == NULL || fwrite(b, 1, n, f) != n || fclose(f) != 0) {
            fprintf(stderr, "cannot write %s\n", path);
            return 1;
        }
        return 0;
    }
    uint8_t have[BUDDY_MESSAGE_MAX_BYTES + 2];
    FILE* f = fopen(path, "rb");
    size_t got = f ? fread(have, 1, sizeof(have), f) : 0;
    if(f) fclose(f);
    if(got != n || memcmp(have, b, n) != 0) {
        fprintf(stderr, "fixture %s is stale or missing (run --write)\n", path);
        return 1;
    }
    return 0;
}

int main(int argc, char** argv) {
    if(argc != 3 || (strcmp(argv[1], "--write") != 0 && strcmp(argv[1], "--check") != 0)) {
        fprintf(stderr, "usage: %s --write|--check DIR\n", argv[0]);
        return 2;
    }
    int failures = 0;
    uint8_t buf[BUDDY_MESSAGE_MAX_BYTES + 1];
    for(size_t i = 0; i < sizeof(names) / sizeof(names[0]); i++) {
        size_t n = make(names[i], buf);
        if(n == 0) {
            fprintf(stderr, "cannot encode %s\n", names[i]);
            return 1;
        }
        failures += handle(argv[1], argv[2], names[i], buf, n);
    }
    return failures == 0 ? 0 : 1;
}

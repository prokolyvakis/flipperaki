#include "buddy_core.h"

#include <string.h>

#include "buddy_protocol.h"

#define KEY_BIT(key) ((uint8_t)(1u << (key)))

typedef flipperaki_buddy_Envelope Envelope;

void buddy_core_init(BuddyCore* core) {
    memset(core, 0, sizeof(*core));
    core->link = BuddyLinkWaitingHello;
    core->armed = true;
}

static void clear_prompt(BuddyCore* core) {
    core->prompt_active = false;
    memset(&core->prompt, 0, sizeof(core->prompt));
    core->prompt_deadline_ms = 0;
    core->fresh = 0;
}

static void expire(BuddyCore* core, uint32_t now_ms) {
    // Wrap-safe: the deadline is at most BUDDY_PROMPT_MAX_SECONDS ahead.
    if(core->prompt_active && (int32_t)(now_ms - core->prompt_deadline_ms) >= 0) {
        clear_prompt(core);
    }
}

// Next outgoing envelope, or NULL when nothing may be sent (no epoch, closed).
static Envelope* push(BuddyCore* core, BuddyActions* out, pb_size_t which) {
    if(core->link != BuddyLinkReady || out->send_count >= BUDDY_ACTIONS_MAX_SENDS) return NULL;
    Envelope* env = &out->send[out->send_count++];
    memset(env, 0, sizeof(*env));
    env->protocol_major = BUDDY_PROTOCOL_MAJOR;
    env->epoch = core->epoch;
    env->sequence = ++core->tx_sequence;
    env->which_payload = which;
    return env;
}

static void receipt(
    BuddyCore* core,
    BuddyActions* out,
    uint32_t sequence,
    flipperaki_buddy_RejectReason reason) {
    Envelope* env = push(core, out, flipperaki_buddy_Envelope_receipt_tag);
    if(env == NULL) return;
    env->payload.receipt.acknowledged_sequence = sequence;
    env->payload.receipt.status = reason == flipperaki_buddy_RejectReason_REJECT_REASON_UNSPECIFIED ?
                                      flipperaki_buddy_ReceiptStatus_RECEIPT_STATUS_ACCEPTED :
                                      flipperaki_buddy_ReceiptStatus_RECEIPT_STATUS_REJECTED;
    env->payload.receipt.reason = reason;
}

// NUL-terminated within cap, printable ASCII only (the display font is ASCII;
// this also rules out control characters and look-alike bytes).
static bool text_ok(const char* s, size_t cap, bool required) {
    const char* end = memchr(s, '\0', cap);
    if(end == NULL || (required && end == s)) return false;
    for(const unsigned char* p = (const unsigned char*)s; p < (const unsigned char*)end; p++) {
        if(*p < 0x20 || *p > 0x7e) return false;
    }
    return true;
}

static bool snapshot_ok(const flipperaki_buddy_Snapshot* s) {
    return s->activity > flipperaki_buddy_Activity_ACTIVITY_UNSPECIFIED &&
           s->activity <= _flipperaki_buddy_Activity_MAX &&
           text_ok(s->session_label, sizeof(s->session_label), false) &&
           text_ok(s->detail, sizeof(s->detail), false);
}

#define APPS_ROOT "/ext/apps/"
#define FAP_SUFFIX ".fap"

// The prompt shows app_label, so it must be exactly the file name the host will
// launch: an installed FAP under /ext/apps/ whose stem equals the label. This
// removes any gap between what the operator approves and what runs.
static bool app_matches_label(const char* path, const char* label) {
    size_t path_len = strlen(path);
    size_t root_len = strlen(APPS_ROOT);
    size_t suffix_len = strlen(FAP_SUFFIX);
    if(path_len <= root_len + suffix_len || strncmp(path, APPS_ROOT, root_len) != 0) return false;
    if(strcmp(path + path_len - suffix_len, FAP_SUFFIX) != 0 || strstr(path, "..") != NULL) {
        return false;
    }
    // Exactly one non-empty, non-dot category directory: /ext/apps/<dir>/<name>.fap
    const char* dir = path + root_len;
    const char* slash = strchr(dir, '/');
    if(slash == NULL || slash == dir || strchr(slash + 1, '/') != NULL || dir[0] == '.') {
        return false;
    }
    const char* name = slash + 1;
    size_t stem_len = (size_t)(path + path_len - suffix_len - name);
    return strlen(label) == stem_len && strncmp(name, label, stem_len) == 0;
}

static bool arm_request_ok(const flipperaki_buddy_ArmRequest* r) {
    return r->request_id != 0 && r->max_actions != 0 && r->expires_in_seconds != 0 &&
           r->expires_in_seconds <= BUDDY_PROMPT_MAX_SECONDS &&
           text_ok(r->app_label, sizeof(r->app_label), true) &&
           text_ok(r->app_path, sizeof(r->app_path), true) &&
           text_ok(r->client_label, sizeof(r->client_label), true) &&
           app_matches_label(r->app_path, r->app_label);
}

static void on_hello(BuddyCore* core, const Envelope* env, BuddyActions* out) {
    const flipperaki_buddy_Hello* hello = &env->payload.hello;
    if(hello->role != flipperaki_buddy_Role_ROLE_HOST || hello->max_message_bytes == 0) {
        clear_prompt(core);
        receipt(core, out, env->sequence, flipperaki_buddy_RejectReason_REJECT_REASON_MALFORMED);
        return;
    }
    if(core->link != BuddyLinkReady || core->epoch != env->epoch) {
        // New connection epoch: nothing from the previous one remains valid.
        clear_prompt(core);
        core->has_snapshot = false;
        memset(&core->snapshot, 0, sizeof(core->snapshot));
        core->last_request_id = 0;
        core->outcome = BuddyOutcomeNone;
        core->epoch = env->epoch;
        core->tx_sequence = 0;
        core->link = BuddyLinkReady;
    }
    receipt(core, out, env->sequence, flipperaki_buddy_RejectReason_REJECT_REASON_UNSPECIFIED);
    Envelope* reply = push(core, out, flipperaki_buddy_Envelope_hello_tag);
    if(reply != NULL) {
        reply->payload.hello.role = flipperaki_buddy_Role_ROLE_BUDDY;
        reply->payload.hello.max_message_bytes = BUDDY_MESSAGE_MAX_BYTES;
    }
}

static flipperaki_buddy_RejectReason
    on_arm_request(BuddyCore* core, uint32_t now_ms, const flipperaki_buddy_ArmRequest* r) {
    // Any ArmRequest supersedes the current prompt, valid or not: the host has moved on.
    clear_prompt(core);
    if(!arm_request_ok(r)) return flipperaki_buddy_RejectReason_REJECT_REASON_MALFORMED;
    if(r->request_id == core->last_request_id) {
        return flipperaki_buddy_RejectReason_REJECT_REASON_STATE; // ids are immutable
    }
    core->prompt = *r;
    core->prompt_active = true;
    core->prompt_deadline_ms = now_ms + r->expires_in_seconds * 1000u;
    core->last_request_id = r->request_id;
    core->armed = core->keys_down == 0; // a key held across the change must be released first
    core->outcome = BuddyOutcomeNone;
    return flipperaki_buddy_RejectReason_REJECT_REASON_UNSPECIFIED;
}

static flipperaki_buddy_RejectReason on_arm_cancel(BuddyCore* core, uint32_t request_id) {
    if(request_id == 0) return flipperaki_buddy_RejectReason_REJECT_REASON_MALFORMED;
    if(!core->prompt_active) return flipperaki_buddy_RejectReason_REJECT_REASON_UNSPECIFIED;
    bool match = core->prompt.request_id == request_id;
    clear_prompt(core); // a mismatched cancel means the two sides disagree: disable
    return match ? flipperaki_buddy_RejectReason_REJECT_REASON_UNSPECIFIED :
                   flipperaki_buddy_RejectReason_REJECT_REASON_STATE;
}

static void on_receipt(BuddyCore* core, const flipperaki_buddy_Receipt* r) {
    bool accepted = r->status == flipperaki_buddy_ReceiptStatus_RECEIPT_STATUS_ACCEPTED;
    bool rejected = r->status == flipperaki_buddy_ReceiptStatus_RECEIPT_STATUS_REJECTED;
    if(core->outcome != BuddyOutcomeSent || r->acknowledged_sequence != core->outcome_sequence ||
       !(accepted || rejected)) {
        return; // unknown status or unrelated sequence: the outcome stays unknown
    }
    core->outcome = accepted ? BuddyOutcomeAccepted : BuddyOutcomeRejected;
}

void buddy_core_on_message(
    BuddyCore* core,
    uint32_t now_ms,
    const uint8_t* data,
    size_t size,
    BuddyActions* out) {
    memset(out, 0, sizeof(*out));
    if(core->link == BuddyLinkClosed) return;
    expire(core, now_ms);

    Envelope env; // fresh temporary: nothing live changes until it validates
    if(!buddy_protocol_decode(data, size, &env)) {
        clear_prompt(core); // it may have been a cancel or replacement we cannot read
        receipt(core, out, 0, flipperaki_buddy_RejectReason_REJECT_REASON_MALFORMED);
        return;
    }
    if(env.protocol_major != BUDDY_PROTOCOL_MAJOR) {
        clear_prompt(core);
        receipt(core, out, env.sequence, flipperaki_buddy_RejectReason_REJECT_REASON_VERSION);
        return;
    }
    if(env.epoch == 0 || env.sequence == 0) {
        clear_prompt(core);
        receipt(core, out, env.sequence, flipperaki_buddy_RejectReason_REJECT_REASON_MALFORMED);
        return;
    }
    if(env.which_payload == flipperaki_buddy_Envelope_hello_tag) {
        on_hello(core, &env, out);
        return;
    }
    if(core->link != BuddyLinkReady) return; // no epoch yet, so no receipt can be addressed
    if(env.epoch != core->epoch) {
        clear_prompt(core);
        receipt(core, out, env.sequence, flipperaki_buddy_RejectReason_REJECT_REASON_EPOCH);
        return;
    }

    flipperaki_buddy_RejectReason reason = flipperaki_buddy_RejectReason_REJECT_REASON_UNSPECIFIED;
    switch(env.which_payload) {
    case flipperaki_buddy_Envelope_snapshot_tag:
        if(snapshot_ok(&env.payload.snapshot)) {
            core->snapshot = env.payload.snapshot;
            core->has_snapshot = true;
        } else {
            reason = flipperaki_buddy_RejectReason_REJECT_REASON_MALFORMED;
        }
        break;
    case flipperaki_buddy_Envelope_arm_request_tag:
        reason = on_arm_request(core, now_ms, &env.payload.arm_request);
        break;
    case flipperaki_buddy_Envelope_arm_cancel_tag:
        reason = on_arm_cancel(core, env.payload.arm_cancel.request_id);
        break;
    case flipperaki_buddy_Envelope_receipt_tag:
        on_receipt(core, &env.payload.receipt);
        return; // receipts are never acknowledged
    case flipperaki_buddy_Envelope_decision_tag:
        reason = flipperaki_buddy_RejectReason_REJECT_REASON_STATE; // Buddy -> host only
        break;
    default:
        reason = flipperaki_buddy_RejectReason_REJECT_REASON_MALFORMED; // no/unknown payload
        break;
    }
    receipt(core, out, env.sequence, reason);
}

static void decide(BuddyCore* core, flipperaki_buddy_Verdict verdict, BuddyActions* out) {
    Envelope* env = push(core, out, flipperaki_buddy_Envelope_decision_tag);
    if(env == NULL) return;
    env->payload.decision.request_id = core->prompt.request_id;
    env->payload.decision.verdict = verdict;
    core->outcome = BuddyOutcomeSent;
    core->outcome_request_id = core->prompt.request_id;
    core->outcome_verdict = verdict;
    core->outcome_sequence = env->sequence;
    clear_prompt(core);
}

void buddy_core_on_input(
    BuddyCore* core,
    uint32_t now_ms,
    BuddyOrigin origin,
    BuddyKey key,
    BuddyInputType type,
    BuddyActions* out) {
    memset(out, 0, sizeof(*out));
    if(origin != BuddyOriginHardware || key >= BuddyKeyCount) return;
    expire(core, now_ms);

    uint8_t bit = KEY_BIT(key);
    switch(type) {
    case BuddyInputPress:
        core->keys_down |= bit;
        if(core->armed) core->fresh |= bit;
        break;
    case BuddyInputRelease:
        core->keys_down &= (uint8_t)~bit;
        if(core->keys_down == 0) core->armed = true;
        break;
    case BuddyInputLong:
        // Approve: OK held past the long-press threshold, pressed after the prompt
        // appeared and all keys were released, with no other key held.
        if(key == BuddyKeyOk && core->prompt_active && (core->fresh & bit) &&
           core->keys_down == bit) {
            decide(core, flipperaki_buddy_Verdict_VERDICT_APPROVE, out);
        } else if(key == BuddyKeyBack && !core->prompt_active && core->keys_down == bit) {
            out->exit = true;
        }
        break;
    case BuddyInputShort:
        if(key == BuddyKeyBack && core->prompt_active && (core->fresh & bit)) {
            decide(core, flipperaki_buddy_Verdict_VERDICT_DENY, out);
        }
        break;
    case BuddyInputRepeat:
        break;
    }
}

void buddy_core_on_tick(BuddyCore* core, uint32_t now_ms) {
    expire(core, now_ms);
}

void buddy_core_on_session_close(BuddyCore* core) {
    clear_prompt(core);
    core->link = BuddyLinkClosed;
}

uint32_t buddy_core_prompt_seconds_left(const BuddyCore* core, uint32_t now_ms) {
    if(!core->prompt_active) return 0;
    int32_t left = (int32_t)(core->prompt_deadline_ms - now_ms);
    return left <= 0 ? 0 : ((uint32_t)left + 999u) / 1000u;
}

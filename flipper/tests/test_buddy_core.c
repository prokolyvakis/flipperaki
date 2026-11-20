#include <stdio.h>
#include <string.h>
#include <unity.h>

#include "buddy_core.h"
#include "buddy_protocol.h"

typedef flipperaki_buddy_Envelope Envelope;

#define EPOCH 7u

static BuddyCore core;
static BuddyActions act;
static uint32_t now;
static uint32_t host_seq;

void setUp(void) {
    buddy_core_init(&core);
    memset(&act, 0, sizeof(act));
    now = 1000;
    host_seq = 0;
}

void tearDown(void) {
}

static Envelope host(pb_size_t which) {
    Envelope e = flipperaki_buddy_Envelope_init_zero;
    e.protocol_major = BUDDY_PROTOCOL_MAJOR;
    e.epoch = EPOCH;
    e.sequence = ++host_seq;
    e.which_payload = which;
    return e;
}

static void deliver(const Envelope* e) {
    uint8_t buf[BUDDY_MESSAGE_MAX_BYTES];
    size_t n = 0;
    TEST_ASSERT_TRUE(buddy_protocol_encode(e, buf, sizeof(buf), &n));
    buddy_core_on_message(&core, now, buf, n, &act);
}

static void hello(void) {
    Envelope e = host(flipperaki_buddy_Envelope_hello_tag);
    e.payload.hello.role = flipperaki_buddy_Role_ROLE_HOST;
    e.payload.hello.max_message_bytes = 512;
    deliver(&e);
}

static Envelope snapshot_msg(const char* label) {
    Envelope e = host(flipperaki_buddy_Envelope_snapshot_tag);
    e.payload.snapshot.activity = flipperaki_buddy_Activity_ACTIVITY_WORKING;
    strcpy(e.payload.snapshot.session_label, label);
    e.payload.snapshot.active_sessions = 1;
    return e;
}

static Envelope arm_msg(uint32_t id) {
    Envelope e = host(flipperaki_buddy_Envelope_arm_request_tag);
    e.payload.arm_request.request_id = id;
    strcpy(e.payload.arm_request.app_label, "subghz");
    strcpy(e.payload.arm_request.app_path, "/ext/apps/Sub-GHz/subghz.fap");
    strcpy(e.payload.arm_request.client_label, "Claude Code");
    e.payload.arm_request.max_actions = 40;
    e.payload.arm_request.expires_in_seconds = 30;
    return e;
}

static void key(BuddyOrigin origin, BuddyKey k, BuddyInputType type) {
    buddy_core_on_input(&core, now, origin, k, type, &act);
}

// Physical OK long press: press, long, release. Returns the actions of the Long event.
static BuddyActions hold_ok(BuddyOrigin origin) {
    key(origin, BuddyKeyOk, BuddyInputPress);
    key(origin, BuddyKeyOk, BuddyInputLong);
    BuddyActions at_long = act;
    key(origin, BuddyKeyOk, BuddyInputRelease);
    return at_long;
}

static void assert_receipt(
    const BuddyActions* a,
    uint32_t seq,
    flipperaki_buddy_ReceiptStatus status,
    flipperaki_buddy_RejectReason reason) {
    TEST_ASSERT_GREATER_OR_EQUAL(1, a->send_count);
    const Envelope* r = &a->send[0];
    TEST_ASSERT_EQUAL(flipperaki_buddy_Envelope_receipt_tag, r->which_payload);
    TEST_ASSERT_EQUAL_UINT32(EPOCH, r->epoch);
    TEST_ASSERT_EQUAL_UINT32(seq, r->payload.receipt.acknowledged_sequence);
    TEST_ASSERT_EQUAL(status, r->payload.receipt.status);
    TEST_ASSERT_EQUAL(reason, r->payload.receipt.reason);
}

static void assert_rejected(flipperaki_buddy_RejectReason reason) {
    assert_receipt(
        &act, host_seq, flipperaki_buddy_ReceiptStatus_RECEIPT_STATUS_REJECTED, reason);
}

static void ready_with_prompt(uint32_t id) {
    hello();
    Envelope s = snapshot_msg("main");
    deliver(&s);
    Envelope a = arm_msg(id);
    deliver(&a);
    TEST_ASSERT_TRUE(core.prompt_active);
}

static void test_hello_establishes_epoch_and_replies(void) {
    hello();
    TEST_ASSERT_EQUAL(BuddyLinkReady, core.link);
    TEST_ASSERT_EQUAL_UINT32(EPOCH, core.epoch);
    TEST_ASSERT_EQUAL(2, act.send_count);
    assert_receipt(
        &act,
        1,
        flipperaki_buddy_ReceiptStatus_RECEIPT_STATUS_ACCEPTED,
        flipperaki_buddy_RejectReason_REJECT_REASON_UNSPECIFIED);
    TEST_ASSERT_EQUAL(flipperaki_buddy_Envelope_hello_tag, act.send[1].which_payload);
    TEST_ASSERT_EQUAL(flipperaki_buddy_Role_ROLE_BUDDY, act.send[1].payload.hello.role);
    TEST_ASSERT_NOT_EQUAL(0, act.send[1].sequence);
}

static void test_message_before_hello_is_ignored(void) {
    Envelope s = snapshot_msg("main");
    deliver(&s);
    TEST_ASSERT_FALSE(core.has_snapshot);
    TEST_ASSERT_EQUAL(0, act.send_count);
}

static void test_valid_snapshot_commits(void) {
    hello();
    Envelope s = snapshot_msg("main");
    deliver(&s);
    TEST_ASSERT_TRUE(core.has_snapshot);
    TEST_ASSERT_EQUAL_STRING("main", core.snapshot.session_label);
    assert_receipt(
        &act,
        host_seq,
        flipperaki_buddy_ReceiptStatus_RECEIPT_STATUS_ACCEPTED,
        flipperaki_buddy_RejectReason_REJECT_REASON_UNSPECIFIED);
}

static void test_unspecified_activity_rejected_and_snapshot_kept(void) {
    hello();
    Envelope good = snapshot_msg("main");
    deliver(&good);
    Envelope bad = snapshot_msg("other");
    bad.payload.snapshot.activity = flipperaki_buddy_Activity_ACTIVITY_UNSPECIFIED;
    deliver(&bad);
    assert_rejected(flipperaki_buddy_RejectReason_REJECT_REASON_MALFORMED);
    TEST_ASSERT_EQUAL_STRING("main", core.snapshot.session_label);
}

static void test_out_of_range_activity_rejected(void) {
    hello();
    Envelope bad = snapshot_msg("main");
    bad.payload.snapshot.activity = (flipperaki_buddy_Activity)99;
    deliver(&bad);
    assert_rejected(flipperaki_buddy_RejectReason_REJECT_REASON_MALFORMED);
    TEST_ASSERT_FALSE(core.has_snapshot);
}

static void test_host_decision_unspecified_verdict_never_approves(void) {
    ready_with_prompt(42);
    Envelope d = host(flipperaki_buddy_Envelope_decision_tag);
    d.payload.decision.request_id = 42;
    d.payload.decision.verdict = flipperaki_buddy_Verdict_VERDICT_UNSPECIFIED;
    deliver(&d);
    assert_rejected(flipperaki_buddy_RejectReason_REJECT_REASON_STATE);
    TEST_ASSERT_EQUAL(1, act.send_count); // only the receipt, never a decision
    TEST_ASSERT_EQUAL(BuddyOutcomeNone, core.outcome);
}

static void test_wrong_version_rejected_and_prompt_disabled(void) {
    ready_with_prompt(42);
    Envelope s = snapshot_msg("v2");
    s.protocol_major = 2;
    deliver(&s);
    assert_rejected(flipperaki_buddy_RejectReason_REJECT_REASON_VERSION);
    TEST_ASSERT_FALSE(core.prompt_active);
    TEST_ASSERT_EQUAL_STRING("main", core.snapshot.session_label);
}

static void test_wrong_epoch_rejected_and_prompt_disabled(void) {
    ready_with_prompt(42);
    Envelope s = snapshot_msg("stale");
    s.epoch = EPOCH + 1;
    deliver(&s);
    assert_rejected(flipperaki_buddy_RejectReason_REJECT_REASON_EPOCH);
    TEST_ASSERT_FALSE(core.prompt_active);
    TEST_ASSERT_EQUAL_UINT32(EPOCH, core.epoch);
    TEST_ASSERT_EQUAL_STRING("main", core.snapshot.session_label);
}

static void test_zero_sequence_rejected(void) {
    hello();
    Envelope s = snapshot_msg("x");
    s.sequence = 0;
    deliver(&s);
    TEST_ASSERT_FALSE(core.has_snapshot);
    assert_receipt(
        &act,
        0,
        flipperaki_buddy_ReceiptStatus_RECEIPT_STATUS_REJECTED,
        flipperaki_buddy_RejectReason_REJECT_REASON_MALFORMED);
}

static void test_new_hello_epoch_clears_prompt_and_snapshot(void) {
    ready_with_prompt(42);
    Envelope e = host(flipperaki_buddy_Envelope_hello_tag);
    e.epoch = EPOCH + 1;
    e.payload.hello.role = flipperaki_buddy_Role_ROLE_HOST;
    e.payload.hello.max_message_bytes = 512;
    deliver(&e);
    TEST_ASSERT_EQUAL_UINT32(EPOCH + 1, core.epoch);
    TEST_ASSERT_FALSE(core.prompt_active);
    TEST_ASSERT_FALSE(core.has_snapshot);
}

static void test_oversized_input_rejected(void) {
    ready_with_prompt(42);
    uint8_t big[BUDDY_MESSAGE_MAX_BYTES + 1];
    memset(big, 0, sizeof(big));
    buddy_core_on_message(&core, now, big, sizeof(big), &act);
    assert_receipt(
        &act,
        0,
        flipperaki_buddy_ReceiptStatus_RECEIPT_STATUS_REJECTED,
        flipperaki_buddy_RejectReason_REJECT_REASON_MALFORMED);
    TEST_ASSERT_FALSE(core.prompt_active);
    TEST_ASSERT_EQUAL_STRING("main", core.snapshot.session_label);
}

static void test_label_longer_than_bound_fails_decode(void) {
    // Snapshot with a 30-byte session_label (bound 24): nanopb refuses it.
    uint8_t msg[12 + 30] = {0x08, 0x01, 0x10, 0x07, 0x18, 0x02, 0x5a, 0x22, 0x08, 0x02, 0x12, 0x1e};
    memset(msg + 12, 'a', 30);
    Envelope out;
    TEST_ASSERT_FALSE(buddy_protocol_decode(msg, sizeof(msg), &out));
}

static void test_decode_failure_keeps_snapshot_and_never_partially_replaces_prompt(void) {
    ready_with_prompt(42);
    Envelope replacement = arm_msg(43);
    strcpy(replacement.payload.arm_request.app_label, "Evil");
    uint8_t buf[BUDDY_MESSAGE_MAX_BYTES];
    size_t n = 0;
    TEST_ASSERT_TRUE(buddy_protocol_encode(&replacement, buf, sizeof(buf), &n));
    buddy_core_on_message(&core, now, buf, n - 3, &act); // truncated
    TEST_ASSERT_EQUAL_STRING("main", core.snapshot.session_label);
    TEST_ASSERT_FALSE(core.prompt_active); // disabled, not half-replaced
    TEST_ASSERT_EQUAL_UINT32(0, core.prompt.request_id);
    TEST_ASSERT_EQUAL_STRING("", core.prompt.app_label);
}

static void test_invalid_snapshot_leaves_live_prompt_intact(void) {
    ready_with_prompt(42);
    flipperaki_buddy_ArmRequest before = core.prompt;
    Envelope bad = snapshot_msg("ctl\x01");
    deliver(&bad);
    assert_rejected(flipperaki_buddy_RejectReason_REJECT_REASON_MALFORMED);
    TEST_ASSERT_TRUE(core.prompt_active);
    TEST_ASSERT_EQUAL_MEMORY(&before, &core.prompt, sizeof(before));
}

static void test_arm_request_validation(void) {
    hello();
    Envelope a = arm_msg(0);
    deliver(&a);
    assert_rejected(flipperaki_buddy_RejectReason_REJECT_REASON_MALFORMED);
    a = arm_msg(5);
    a.payload.arm_request.max_actions = 0;
    deliver(&a);
    assert_rejected(flipperaki_buddy_RejectReason_REJECT_REASON_MALFORMED);
    a = arm_msg(5);
    a.payload.arm_request.expires_in_seconds = BUDDY_PROMPT_MAX_SECONDS + 1;
    deliver(&a);
    assert_rejected(flipperaki_buddy_RejectReason_REJECT_REASON_MALFORMED);
    a = arm_msg(5);
    strcpy(a.payload.arm_request.app_label, "Sub\nGHz");
    deliver(&a);
    assert_rejected(flipperaki_buddy_RejectReason_REJECT_REASON_MALFORMED);
    TEST_ASSERT_FALSE(core.prompt_active);
}

// The approved label must be exactly what runs: an /ext/apps FAP of that name.
static void test_prompt_label_must_name_the_launched_fap(void) {
    hello();
    const char* paths[] = {
        "/ext/apps/Sub-GHz/other.fap", // label differs from file name
        "/ext/apps/../int/subghz.fap", // traversal
        "/int/apps/Tools/subghz.fap", // outside /ext/apps
        "/ext/apps/Tools/subghz.elf", // not a FAP
        "subghz", // not a path
    };
    for(size_t i = 0; i < sizeof(paths) / sizeof(paths[0]); i++) {
        Envelope a = arm_msg(5 + (uint32_t)i);
        strcpy(a.payload.arm_request.app_path, paths[i]);
        deliver(&a);
        assert_rejected(flipperaki_buddy_RejectReason_REJECT_REASON_MALFORMED);
        TEST_ASSERT_FALSE(core.prompt_active);
    }
}

static void test_reused_request_id_rejected(void) {
    ready_with_prompt(42);
    Envelope again = arm_msg(42);
    deliver(&again);
    assert_rejected(flipperaki_buddy_RejectReason_REJECT_REASON_STATE);
    TEST_ASSERT_FALSE(core.prompt_active);
}

static void test_physical_ok_long_press_approves_with_request_id(void) {
    ready_with_prompt(42);
    BuddyActions a = hold_ok(BuddyOriginHardware);
    TEST_ASSERT_EQUAL(1, a.send_count);
    TEST_ASSERT_EQUAL(flipperaki_buddy_Envelope_decision_tag, a.send[0].which_payload);
    TEST_ASSERT_EQUAL_UINT32(42, a.send[0].payload.decision.request_id);
    TEST_ASSERT_EQUAL(
        flipperaki_buddy_Verdict_VERDICT_APPROVE, a.send[0].payload.decision.verdict);
    TEST_ASSERT_EQUAL_UINT32(EPOCH, a.send[0].epoch);
    TEST_ASSERT_FALSE(core.prompt_active);
    TEST_ASSERT_EQUAL(BuddyOutcomeSent, core.outcome);
}

static void test_ok_short_press_does_not_approve(void) {
    ready_with_prompt(42);
    key(BuddyOriginHardware, BuddyKeyOk, BuddyInputPress);
    key(BuddyOriginHardware, BuddyKeyOk, BuddyInputRelease);
    key(BuddyOriginHardware, BuddyKeyOk, BuddyInputShort);
    TEST_ASSERT_EQUAL(0, act.send_count);
    TEST_ASSERT_TRUE(core.prompt_active);
}

static void test_back_short_denies(void) {
    ready_with_prompt(42);
    key(BuddyOriginHardware, BuddyKeyBack, BuddyInputPress);
    key(BuddyOriginHardware, BuddyKeyBack, BuddyInputRelease);
    key(BuddyOriginHardware, BuddyKeyBack, BuddyInputShort);
    TEST_ASSERT_EQUAL(1, act.send_count);
    TEST_ASSERT_EQUAL(flipperaki_buddy_Verdict_VERDICT_DENY, act.send[0].payload.decision.verdict);
    TEST_ASSERT_EQUAL_UINT32(42, act.send[0].payload.decision.request_id);
}

static void test_software_origin_cannot_approve(void) {
    ready_with_prompt(42);
    BuddyActions a = hold_ok(BuddyOriginSoftware);
    TEST_ASSERT_EQUAL(0, a.send_count);
    TEST_ASSERT_TRUE(core.prompt_active);
    // Software release must not complete a hardware press either.
    key(BuddyOriginHardware, BuddyKeyOk, BuddyInputPress);
    key(BuddyOriginSoftware, BuddyKeyOk, BuddyInputRelease);
    key(BuddyOriginSoftware, BuddyKeyOk, BuddyInputLong);
    TEST_ASSERT_EQUAL(0, act.send_count);
    TEST_ASSERT_TRUE(core.prompt_active);
}

static void test_held_key_across_prompt_replacement_cannot_approve(void) {
    ready_with_prompt(42);
    key(BuddyOriginHardware, BuddyKeyOk, BuddyInputPress); // held while prompt changes
    Envelope next = arm_msg(43);
    deliver(&next);
    TEST_ASSERT_TRUE(core.prompt_active);
    key(BuddyOriginHardware, BuddyKeyOk, BuddyInputLong);
    TEST_ASSERT_EQUAL(0, act.send_count);
    key(BuddyOriginHardware, BuddyKeyOk, BuddyInputRelease);
    TEST_ASSERT_TRUE(core.prompt_active);
    // A fresh, deliberate long press after release does approve request 43.
    BuddyActions a = hold_ok(BuddyOriginHardware);
    TEST_ASSERT_EQUAL(1, a.send_count);
    TEST_ASSERT_EQUAL_UINT32(43, a.send[0].payload.decision.request_id);
}

static void test_held_back_across_prompt_does_not_deny(void) {
    hello();
    key(BuddyOriginHardware, BuddyKeyBack, BuddyInputPress);
    Envelope a = arm_msg(42);
    deliver(&a);
    key(BuddyOriginHardware, BuddyKeyBack, BuddyInputRelease);
    key(BuddyOriginHardware, BuddyKeyBack, BuddyInputShort);
    TEST_ASSERT_EQUAL(0, act.send_count);
    TEST_ASSERT_TRUE(core.prompt_active);
}

static void test_other_key_held_blocks_approve(void) {
    ready_with_prompt(42);
    key(BuddyOriginHardware, BuddyKeyUp, BuddyInputPress);
    BuddyActions a = hold_ok(BuddyOriginHardware);
    TEST_ASSERT_EQUAL(0, a.send_count);
}

static void test_expiry_invalidates_prompt(void) {
    ready_with_prompt(42);
    TEST_ASSERT_EQUAL_UINT32(30, buddy_core_prompt_seconds_left(&core, now));
    now += 30 * 1000;
    buddy_core_on_tick(&core, now);
    TEST_ASSERT_FALSE(core.prompt_active);
    BuddyActions a = hold_ok(BuddyOriginHardware);
    TEST_ASSERT_EQUAL(0, a.send_count);
}

static void test_expiry_checked_at_input_time(void) {
    ready_with_prompt(42);
    key(BuddyOriginHardware, BuddyKeyOk, BuddyInputPress);
    now += 30 * 1000; // no tick in between
    key(BuddyOriginHardware, BuddyKeyOk, BuddyInputLong);
    TEST_ASSERT_EQUAL(0, act.send_count);
    TEST_ASSERT_FALSE(core.prompt_active);
}

static void test_expiry_is_wrap_safe(void) {
    now = 0xFFFFF000u;
    ready_with_prompt(42);
    now += 10 * 1000; // wraps past zero
    buddy_core_on_tick(&core, now);
    TEST_ASSERT_TRUE(core.prompt_active);
    now += 20 * 1000;
    buddy_core_on_tick(&core, now);
    TEST_ASSERT_FALSE(core.prompt_active);
}

static void test_arm_cancel_invalidates(void) {
    ready_with_prompt(42);
    Envelope c = host(flipperaki_buddy_Envelope_arm_cancel_tag);
    c.payload.arm_cancel.request_id = 42;
    deliver(&c);
    assert_receipt(
        &act,
        host_seq,
        flipperaki_buddy_ReceiptStatus_RECEIPT_STATUS_ACCEPTED,
        flipperaki_buddy_RejectReason_REJECT_REASON_UNSPECIFIED);
    TEST_ASSERT_FALSE(core.prompt_active);
    BuddyActions a = hold_ok(BuddyOriginHardware);
    TEST_ASSERT_EQUAL(0, a.send_count);
}

static void test_mismatched_arm_cancel_disables_prompt(void) {
    ready_with_prompt(42);
    Envelope c = host(flipperaki_buddy_Envelope_arm_cancel_tag);
    c.payload.arm_cancel.request_id = 41;
    deliver(&c);
    assert_rejected(flipperaki_buddy_RejectReason_REJECT_REASON_STATE);
    TEST_ASSERT_FALSE(core.prompt_active);
}

static void test_session_close_invalidates_and_silences(void) {
    ready_with_prompt(42);
    buddy_core_on_session_close(&core);
    TEST_ASSERT_FALSE(core.prompt_active);
    BuddyActions a = hold_ok(BuddyOriginHardware);
    TEST_ASSERT_EQUAL(0, a.send_count);
    Envelope s = snapshot_msg("late");
    deliver(&s);
    TEST_ASSERT_EQUAL(0, act.send_count);
}

static void test_receipt_resolves_decision_outcome(void) {
    ready_with_prompt(42);
    BuddyActions a = hold_ok(BuddyOriginHardware);
    uint32_t decision_seq = a.send[0].sequence;

    Envelope r = host(flipperaki_buddy_Envelope_receipt_tag);
    r.payload.receipt.acknowledged_sequence = decision_seq;
    r.payload.receipt.status = flipperaki_buddy_ReceiptStatus_RECEIPT_STATUS_UNSPECIFIED;
    deliver(&r);
    TEST_ASSERT_EQUAL(0, act.send_count); // receipts are not acknowledged
    TEST_ASSERT_EQUAL(BuddyOutcomeSent, core.outcome); // unknown stays unknown

    r = host(flipperaki_buddy_Envelope_receipt_tag);
    r.payload.receipt.acknowledged_sequence = decision_seq + 100;
    r.payload.receipt.status = flipperaki_buddy_ReceiptStatus_RECEIPT_STATUS_ACCEPTED;
    deliver(&r);
    TEST_ASSERT_EQUAL(BuddyOutcomeSent, core.outcome);

    r = host(flipperaki_buddy_Envelope_receipt_tag);
    r.payload.receipt.acknowledged_sequence = decision_seq;
    r.payload.receipt.status = flipperaki_buddy_ReceiptStatus_RECEIPT_STATUS_REJECTED;
    r.payload.receipt.reason = flipperaki_buddy_RejectReason_REJECT_REASON_STATE;
    deliver(&r);
    TEST_ASSERT_EQUAL(BuddyOutcomeRejected, core.outcome);
}

static void test_back_long_without_prompt_requests_exit(void) {
    hello();
    key(BuddyOriginHardware, BuddyKeyBack, BuddyInputPress);
    key(BuddyOriginHardware, BuddyKeyBack, BuddyInputLong);
    TEST_ASSERT_TRUE(act.exit);
    key(BuddyOriginSoftware, BuddyKeyBack, BuddyInputLong);
    TEST_ASSERT_FALSE(act.exit);
}

static size_t read_fixture(const char* rel, uint8_t* buf, size_t cap) {
    char path[512];
    snprintf(path, sizeof(path), "%s/%s.bin", FIXTURES_DIR, rel);
    FILE* f = fopen(path, "rb");
    TEST_ASSERT_NOT_NULL_MESSAGE(f, path);
    size_t n = fread(buf, 1, cap, f);
    fclose(f);
    return n;
}

static Envelope decode_fixture(const char* rel) {
    uint8_t buf[BUDDY_MESSAGE_MAX_BYTES + 8];
    size_t n = read_fixture(rel, buf, sizeof(buf));
    Envelope e;
    TEST_ASSERT_TRUE_MESSAGE(buddy_protocol_decode(buf, n, &e), rel);
    TEST_ASSERT_EQUAL_UINT32(BUDDY_PROTOCOL_MAJOR, e.protocol_major);
    TEST_ASSERT_EQUAL_UINT32(EPOCH, e.epoch);
    return e;
}

static void deliver_fixture(const char* rel, flipperaki_buddy_ReceiptStatus status) {
    uint8_t buf[BUDDY_MESSAGE_MAX_BYTES + 8];
    size_t n = read_fixture(rel, buf, sizeof(buf));
    buddy_core_on_message(&core, now, buf, n, &act);
    TEST_ASSERT_GREATER_OR_EQUAL_MESSAGE(1, act.send_count, rel);
    TEST_ASSERT_EQUAL_MESSAGE(flipperaki_buddy_Envelope_receipt_tag, act.send[0].which_payload, rel);
    TEST_ASSERT_EQUAL_MESSAGE(status, act.send[0].payload.receipt.status, rel);
}

// Rust-encoded host fixtures (protocol/fixtures/rust/, owned by the host side).
static void test_rust_fixtures_decode_with_expected_semantics(void) {
    Envelope e = decode_fixture("rust/hello_host");
    TEST_ASSERT_EQUAL_UINT32(1, e.sequence);
    TEST_ASSERT_EQUAL(flipperaki_buddy_Envelope_hello_tag, e.which_payload);
    TEST_ASSERT_EQUAL(flipperaki_buddy_Role_ROLE_HOST, e.payload.hello.role);
    TEST_ASSERT_EQUAL_UINT32(512, e.payload.hello.max_message_bytes);

    e = decode_fixture("rust/snapshot_working");
    TEST_ASSERT_EQUAL_UINT32(2, e.sequence);
    TEST_ASSERT_EQUAL(flipperaki_buddy_Envelope_snapshot_tag, e.which_payload);
    TEST_ASSERT_EQUAL(flipperaki_buddy_Activity_ACTIVITY_WORKING, e.payload.snapshot.activity);
    TEST_ASSERT_EQUAL_STRING("flipperaki", e.payload.snapshot.session_label);
    TEST_ASSERT_EQUAL_UINT32(2, e.payload.snapshot.active_sessions);
    TEST_ASSERT_EQUAL_UINT32(5, e.payload.snapshot.age_seconds);
    TEST_ASSERT_EQUAL_STRING("", e.payload.snapshot.detail);

    e = decode_fixture("rust/arm_request");
    TEST_ASSERT_EQUAL_UINT32(3, e.sequence);
    TEST_ASSERT_EQUAL(flipperaki_buddy_Envelope_arm_request_tag, e.which_payload);
    TEST_ASSERT_EQUAL_UINT32(42, e.payload.arm_request.request_id);
    TEST_ASSERT_EQUAL_STRING("clock", e.payload.arm_request.app_label);
    TEST_ASSERT_EQUAL_STRING("/ext/apps/Tools/clock.fap", e.payload.arm_request.app_path);
    TEST_ASSERT_EQUAL_STRING("client 1", e.payload.arm_request.client_label);
    TEST_ASSERT_EQUAL_UINT32(40, e.payload.arm_request.max_actions);
    TEST_ASSERT_EQUAL_UINT32(30, e.payload.arm_request.expires_in_seconds);

    e = decode_fixture("rust/arm_cancel");
    TEST_ASSERT_EQUAL_UINT32(4, e.sequence);
    TEST_ASSERT_EQUAL(flipperaki_buddy_Envelope_arm_cancel_tag, e.which_payload);
    TEST_ASSERT_EQUAL_UINT32(42, e.payload.arm_cancel.request_id);

    e = decode_fixture("rust/invalid_snapshot_unspecified_activity");
    TEST_ASSERT_EQUAL_UINT32(5, e.sequence);
    TEST_ASSERT_EQUAL(flipperaki_buddy_Envelope_snapshot_tag, e.which_payload);
    TEST_ASSERT_EQUAL(
        flipperaki_buddy_Activity_ACTIVITY_UNSPECIFIED, e.payload.snapshot.activity);
}

static void test_rust_fixtures_replay_through_core(void) {
    const flipperaki_buddy_ReceiptStatus ok = flipperaki_buddy_ReceiptStatus_RECEIPT_STATUS_ACCEPTED;
    const flipperaki_buddy_ReceiptStatus no = flipperaki_buddy_ReceiptStatus_RECEIPT_STATUS_REJECTED;
    deliver_fixture("rust/hello_host", ok);
    TEST_ASSERT_EQUAL(2, act.send_count);
    TEST_ASSERT_EQUAL(flipperaki_buddy_Role_ROLE_BUDDY, act.send[1].payload.hello.role);
    TEST_ASSERT_EQUAL_UINT32(EPOCH, act.send[1].epoch); // Buddy adopts the host epoch
    deliver_fixture("rust/snapshot_working", ok);
    deliver_fixture("rust/arm_request", ok);
    TEST_ASSERT_TRUE(core.prompt_active);
    TEST_ASSERT_EQUAL_UINT32(42, core.prompt.request_id);
    deliver_fixture("rust/arm_cancel", ok);
    TEST_ASSERT_FALSE(core.prompt_active);
    deliver_fixture("rust/invalid_snapshot_unspecified_activity", no);
    TEST_ASSERT_EQUAL_STRING("flipperaki", core.snapshot.session_label);
}

// C-encoded negative fixtures (protocol/fixtures/c/) are rejected by the core too.
static void test_c_invalid_fixtures_rejected(void) {
    const flipperaki_buddy_ReceiptStatus no = flipperaki_buddy_ReceiptStatus_RECEIPT_STATUS_REJECTED;
    hello();
    deliver_fixture("c/invalid_version", no);
    deliver_fixture("c/invalid_truncated", no);
    deliver_fixture("c/invalid_label_too_long", no);
    deliver_fixture("c/invalid_oversized", no);
    Envelope e = decode_fixture("c/decision_unspecified");
    TEST_ASSERT_EQUAL(flipperaki_buddy_Verdict_VERDICT_UNSPECIFIED, e.payload.decision.verdict);
}

static void test_encode_respects_limit(void) {
    Envelope e = arm_msg(1);
    uint8_t small[8];
    size_t n = 0;
    TEST_ASSERT_FALSE(buddy_protocol_encode(&e, small, sizeof(small), &n));
}

int main(void) {
    UNITY_BEGIN();
    RUN_TEST(test_hello_establishes_epoch_and_replies);
    RUN_TEST(test_message_before_hello_is_ignored);
    RUN_TEST(test_valid_snapshot_commits);
    RUN_TEST(test_unspecified_activity_rejected_and_snapshot_kept);
    RUN_TEST(test_out_of_range_activity_rejected);
    RUN_TEST(test_host_decision_unspecified_verdict_never_approves);
    RUN_TEST(test_wrong_version_rejected_and_prompt_disabled);
    RUN_TEST(test_wrong_epoch_rejected_and_prompt_disabled);
    RUN_TEST(test_zero_sequence_rejected);
    RUN_TEST(test_new_hello_epoch_clears_prompt_and_snapshot);
    RUN_TEST(test_oversized_input_rejected);
    RUN_TEST(test_label_longer_than_bound_fails_decode);
    RUN_TEST(test_decode_failure_keeps_snapshot_and_never_partially_replaces_prompt);
    RUN_TEST(test_invalid_snapshot_leaves_live_prompt_intact);
    RUN_TEST(test_arm_request_validation);
    RUN_TEST(test_reused_request_id_rejected);
    RUN_TEST(test_physical_ok_long_press_approves_with_request_id);
    RUN_TEST(test_ok_short_press_does_not_approve);
    RUN_TEST(test_back_short_denies);
    RUN_TEST(test_software_origin_cannot_approve);
    RUN_TEST(test_held_key_across_prompt_replacement_cannot_approve);
    RUN_TEST(test_held_back_across_prompt_does_not_deny);
    RUN_TEST(test_other_key_held_blocks_approve);
    RUN_TEST(test_expiry_invalidates_prompt);
    RUN_TEST(test_expiry_checked_at_input_time);
    RUN_TEST(test_expiry_is_wrap_safe);
    RUN_TEST(test_arm_cancel_invalidates);
    RUN_TEST(test_mismatched_arm_cancel_disables_prompt);
    RUN_TEST(test_session_close_invalidates_and_silences);
    RUN_TEST(test_receipt_resolves_decision_outcome);
    RUN_TEST(test_back_long_without_prompt_requests_exit);
    RUN_TEST(test_rust_fixtures_decode_with_expected_semantics);
    RUN_TEST(test_rust_fixtures_replay_through_core);
    RUN_TEST(test_c_invalid_fixtures_rejected);
    RUN_TEST(test_encode_respects_limit);
    RUN_TEST(test_prompt_label_must_name_the_launched_fap);
    return UNITY_END();
}

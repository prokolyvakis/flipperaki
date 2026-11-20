#pragma once
// Buddy state machine. Portable (no SDK headers), single writer: every call
// comes from the app thread with an explicit monotonic time in milliseconds,
// and returns a small bounded set of actions for the caller to perform.

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "buddy.pb.h"

// Longest ArmRequest lifetime accepted; longer requests are rejected, not clamped.
#define BUDDY_PROMPT_MAX_SECONDS 300u

typedef enum {
    BuddyOriginHardware, // physical button (InputEvent sequence_source HARDWARE)
    BuddyOriginSoftware, // injected (RPC GUI input, anything not provably physical)
} BuddyOrigin;

typedef enum {
    BuddyKeyUp,
    BuddyKeyDown,
    BuddyKeyRight,
    BuddyKeyLeft,
    BuddyKeyOk,
    BuddyKeyBack,
    BuddyKeyCount,
} BuddyKey;

typedef enum {
    BuddyInputPress,
    BuddyInputRelease,
    BuddyInputShort,
    BuddyInputLong,
    BuddyInputRepeat,
} BuddyInputType;

typedef enum {
    BuddyLinkWaitingHello,
    BuddyLinkReady,
    BuddyLinkClosed,
} BuddyLink;

typedef enum {
    BuddyOutcomeNone,
    BuddyOutcomeSent, // decision sent, no receipt yet: outcome unknown
    BuddyOutcomeAccepted,
    BuddyOutcomeRejected,
} BuddyOutcome;

typedef struct {
    BuddyLink link;
    uint32_t epoch; // established by the host Hello; 0 before
    uint32_t tx_sequence; // last sequence Buddy sent in this epoch

    bool has_snapshot;
    flipperaki_buddy_Snapshot snapshot;

    bool prompt_active;
    flipperaki_buddy_ArmRequest prompt; // never modified while active
    uint32_t prompt_deadline_ms;
    uint32_t last_request_id; // most recent ArmRequest id this epoch

    // Physical gesture tracking (hardware events only), bit per BuddyKey.
    uint8_t keys_down;
    uint8_t fresh; // keys pressed after all keys were released under the current prompt
    bool armed; // all keys have been released since the current prompt appeared

    BuddyOutcome outcome;
    uint32_t outcome_request_id;
    flipperaki_buddy_Verdict outcome_verdict;
    uint32_t outcome_sequence;
} BuddyCore;

#define BUDDY_ACTIONS_MAX_SENDS 2

typedef struct {
    uint8_t send_count;
    flipperaki_buddy_Envelope send[BUDDY_ACTIONS_MAX_SENDS];
    bool exit; // operator asked to leave the app
} BuddyActions;

void buddy_core_init(BuddyCore* core);

// One complete inner message from the host (already copied out of the RPC callback).
void buddy_core_on_message(
    BuddyCore* core,
    uint32_t now_ms,
    const uint8_t* data,
    size_t size,
    BuddyActions* out);

void buddy_core_on_input(
    BuddyCore* core,
    uint32_t now_ms,
    BuddyOrigin origin,
    BuddyKey key,
    BuddyInputType type,
    BuddyActions* out);

// Periodic: enforces prompt expiry in core-local time.
void buddy_core_on_tick(BuddyCore* core, uint32_t now_ms);

// The RPC session is gone: drop the prompt, never send again.
void buddy_core_on_session_close(BuddyCore* core);

// Seconds left on the active prompt (0 if none).
uint32_t buddy_core_prompt_seconds_left(const BuddyCore* core, uint32_t now_ms);

//! Every resource bound in the host, in one place. These are project policies,
//! not firmware maxima; change them only with a test that exercises the bound.

use std::time::Duration;

/// Maximum encoded Buddy `Envelope`, checked on both sides.
pub const BUDDY_MESSAGE_MAX_BYTES: usize = 512;
/// Buddy protocol major version spoken by this host.
pub const BUDDY_PROTOCOL_MAJOR: u32 = 1;
/// Maximum single native RPC frame accepted from the device.
pub const RPC_FRAME_MAX_BYTES: usize = 64 * 1024;
/// Maximum aggregate bytes of one multipart RPC response.
pub const RPC_RESPONSE_MAX_BYTES: usize = 256 * 1024;
/// Maximum RPC requests awaiting a reply at once.
pub const RPC_PENDING_MAX: usize = 8;
/// Maximum queued unsolicited device events before the session fails.
pub const RPC_EVENT_QUEUE: usize = 64;
/// Default deadline for one device operation, from admission to reply.
pub const RPC_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum bytes of one local IPC line (request or response).
pub const IPC_LINE_MAX_BYTES: usize = 64 * 1024;
/// Maximum bytes read from a Claude Code hook payload on stdin.
pub const HOOK_INPUT_MAX_BYTES: usize = 256 * 1024;
/// Maximum concurrently tracked Claude sessions for status aggregation.
pub const TRACKED_SESSIONS_MAX: usize = 16;
/// Maximum taps in one `flipper_input` call.
pub const INPUT_TAPS_MAX: usize = 3;
/// Longest permitted hold.
pub const INPUT_HOLD_MAX: Duration = Duration::from_millis(1500);
/// How long an ArmRequest waits for a physical decision.
pub const ARM_REQUEST_TTL: Duration = Duration::from_secs(30);
/// How long an approved control grant lasts.
pub const GRANT_TTL: Duration = Duration::from_secs(300);
/// Action budget of one approved control grant.
pub const GRANT_ACTION_BUDGET: u32 = 40;

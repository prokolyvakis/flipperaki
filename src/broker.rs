//! Control authorization: pure decisions about who may drive the device.
//!
//! Nothing in this module performs I/O or reads a clock. The daemon passes the
//! current time and connection epoch in, applies the returned state and only
//! then talks to the device.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::limits::{
    ARM_REQUEST_TTL, GRANT_ACTION_BUDGET, GRANT_TTL, INPUT_HOLD_MAX, INPUT_TAPS_MAX,
};

/// Server-assigned identity of one IPC connection. Never taken from a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClientId(pub u64);

/// Increments on every new device session, including transport changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConnectionEpoch(pub u32);

/// Identity of one ArmRequest shown on Buddy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestId(pub u32);

/// The exact app a workflow targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppTarget {
    /// Loader name or FAP path passed to AppStart.
    pub path: String,
    /// User-facing name shown on the approval prompt.
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    Up,
    Down,
    Left,
    Right,
    Ok,
    Back,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum InputStep {
    Tap { button: Button },
    Hold { button: Button, millis: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRequest {
    pub id: RequestId,
    pub client: ClientId,
    pub epoch: ConnectionEpoch,
    pub app: AppTarget,
    pub expires_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlGrant {
    pub request: RequestId,
    pub client: ClientId,
    pub epoch: ConnectionEpoch,
    pub app: AppTarget,
    pub expires_at: Instant,
    pub remaining_actions: u32,
}

/// Who, if anyone, is entitled to control the device right now.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Control {
    #[default]
    None,
    Pending(PendingRequest),
    Granted(ControlGrant),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Denial {
    #[error("control is paused on the Mac")]
    Paused,
    #[error("no device is connected")]
    Disconnected,
    #[error("another workflow is pending or active")]
    Busy,
    #[error("this client does not own the active workflow")]
    NotOwner,
    #[error("no approved workflow is active; call flipper_open first")]
    NoGrant,
    #[error("the connection changed since approval")]
    StaleEpoch,
    #[error("the approval or request expired")]
    Expired,
    #[error("the approved action budget is used up")]
    BudgetExhausted,
    #[error("the decision does not match the pending request")]
    WrongRequest,
    #[error("the operator denied the request on the device")]
    DeniedByOperator,
    #[error("input sequence is empty or longer than allowed")]
    BadSequence,
    #[error("hold is longer than allowed")]
    HoldTooLong,
}

/// Admits a new control request from `client` for `app`.
pub fn begin_request(
    control: &Control,
    client: ClientId,
    epoch: Option<ConnectionEpoch>,
    paused: bool,
    app: AppTarget,
    id: RequestId,
    now: Instant,
) -> Result<PendingRequest, Denial> {
    if paused {
        return Err(Denial::Paused);
    }
    let epoch = epoch.ok_or(Denial::Disconnected)?;
    let occupied = match control {
        Control::None => false,
        Control::Pending(p) => now < p.expires_at,
        Control::Granted(g) => now < g.expires_at && g.epoch == epoch,
    };
    if occupied {
        return Err(Denial::Busy);
    }
    Ok(PendingRequest {
        id,
        client,
        epoch,
        app,
        expires_at: now + ARM_REQUEST_TTL,
    })
}

/// Turns a physical decision received from Buddy into a grant.
pub fn resolve_decision(
    pending: &PendingRequest,
    request: RequestId,
    approved: bool,
    epoch: ConnectionEpoch,
    now: Instant,
) -> Result<ControlGrant, Denial> {
    if request != pending.id {
        return Err(Denial::WrongRequest);
    }
    if epoch != pending.epoch {
        return Err(Denial::StaleEpoch);
    }
    if now >= pending.expires_at {
        return Err(Denial::Expired);
    }
    if !approved {
        return Err(Denial::DeniedByOperator);
    }
    Ok(ControlGrant {
        request: pending.id,
        client: pending.client,
        epoch: pending.epoch,
        app: pending.app.clone(),
        expires_at: now + GRANT_TTL,
        remaining_actions: GRANT_ACTION_BUDGET,
    })
}

/// Checks that `client` still owns a live grant on this connection.
pub fn check_owner(
    control: &Control,
    client: ClientId,
    epoch: Option<ConnectionEpoch>,
    now: Instant,
) -> Result<&ControlGrant, Denial> {
    let Control::Granted(grant) = control else {
        return Err(Denial::NoGrant);
    };
    if grant.client != client {
        return Err(Denial::NotOwner);
    }
    if epoch != Some(grant.epoch) {
        return Err(Denial::StaleEpoch);
    }
    if now >= grant.expires_at {
        return Err(Denial::Expired);
    }
    Ok(grant)
}

/// Approves a bounded input sequence and returns the grant with its budget
/// reduced. The caller must store the returned grant before sending input.
pub fn authorize_input(
    control: &Control,
    client: ClientId,
    epoch: Option<ConnectionEpoch>,
    paused: bool,
    now: Instant,
    steps: &[InputStep],
) -> Result<ControlGrant, Denial> {
    if paused {
        return Err(Denial::Paused);
    }
    let grant = check_owner(control, client, epoch, now)?;
    if steps.is_empty() || steps.len() > INPUT_TAPS_MAX {
        return Err(Denial::BadSequence);
    }
    let too_long = steps.iter().any(|step| match step {
        InputStep::Hold { millis, .. } => {
            Duration::from_millis(u64::from(*millis)) > INPUT_HOLD_MAX
        }
        InputStep::Tap { .. } => false,
    });
    if too_long {
        return Err(Denial::HoldTooLong);
    }
    let cost = u32::try_from(steps.len()).unwrap_or(u32::MAX);
    let remaining_actions = grant
        .remaining_actions
        .checked_sub(cost)
        .ok_or(Denial::BudgetExhausted)?;
    Ok(ControlGrant {
        remaining_actions,
        ..grant.clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPOCH: ConnectionEpoch = ConnectionEpoch(1);
    const ALICE: ClientId = ClientId(1);
    const BOB: ClientId = ClientId(2);

    fn app() -> AppTarget {
        AppTarget {
            path: "Clock".into(),
            label: "Clock".into(),
        }
    }

    fn granted(now: Instant) -> Control {
        let pending = begin_request(
            &Control::None,
            ALICE,
            Some(EPOCH),
            false,
            app(),
            RequestId(1),
            now,
        )
        .expect("admitted");
        Control::Granted(
            resolve_decision(&pending, RequestId(1), true, EPOCH, now).expect("approved"),
        )
    }

    fn tap() -> InputStep {
        InputStep::Tap { button: Button::Ok }
    }

    #[test]
    fn requests_need_a_connection_and_no_pause() {
        let now = Instant::now();
        assert_eq!(
            begin_request(&Control::None, ALICE, None, false, app(), RequestId(1), now),
            Err(Denial::Disconnected)
        );
        assert_eq!(
            begin_request(
                &Control::None,
                ALICE,
                Some(EPOCH),
                true,
                app(),
                RequestId(1),
                now
            ),
            Err(Denial::Paused)
        );
    }

    #[test]
    fn a_second_client_cannot_share_or_steal_a_grant() {
        let now = Instant::now();
        let control = granted(now);
        assert_eq!(
            begin_request(&control, BOB, Some(EPOCH), false, app(), RequestId(2), now),
            Err(Denial::Busy)
        );
        assert_eq!(
            authorize_input(&control, BOB, Some(EPOCH), false, now, &[tap()]),
            Err(Denial::NotOwner)
        );
        assert!(authorize_input(&control, ALICE, Some(EPOCH), false, now, &[tap()]).is_ok());
    }

    #[test]
    fn stale_wrong_expired_and_denied_decisions_fail() {
        let now = Instant::now();
        let pending = begin_request(
            &Control::None,
            ALICE,
            Some(EPOCH),
            false,
            app(),
            RequestId(5),
            now,
        )
        .unwrap();
        assert_eq!(
            resolve_decision(&pending, RequestId(4), true, EPOCH, now),
            Err(Denial::WrongRequest)
        );
        assert_eq!(
            resolve_decision(&pending, RequestId(5), true, ConnectionEpoch(2), now),
            Err(Denial::StaleEpoch)
        );
        assert_eq!(
            resolve_decision(&pending, RequestId(5), true, EPOCH, now + ARM_REQUEST_TTL),
            Err(Denial::Expired)
        );
        assert_eq!(
            resolve_decision(&pending, RequestId(5), false, EPOCH, now),
            Err(Denial::DeniedByOperator)
        );
    }

    #[test]
    fn reconnect_invalidates_the_grant() {
        let now = Instant::now();
        let control = granted(now);
        assert_eq!(
            authorize_input(
                &control,
                ALICE,
                Some(ConnectionEpoch(2)),
                false,
                now,
                &[tap()]
            ),
            Err(Denial::StaleEpoch)
        );
        assert_eq!(
            authorize_input(&control, ALICE, None, false, now, &[tap()]),
            Err(Denial::StaleEpoch)
        );
        // A new epoch frees the device for a fresh request.
        assert!(
            begin_request(
                &control,
                BOB,
                Some(ConnectionEpoch(2)),
                false,
                app(),
                RequestId(2),
                now
            )
            .is_ok()
        );
    }

    #[test]
    fn input_is_bounded_by_length_hold_budget_pause_and_expiry() {
        let now = Instant::now();
        let control = granted(now);
        let input =
            |steps: &[InputStep]| authorize_input(&control, ALICE, Some(EPOCH), false, now, steps);
        assert_eq!(input(&[]), Err(Denial::BadSequence));
        assert_eq!(
            input(&[tap(); INPUT_TAPS_MAX + 1]),
            Err(Denial::BadSequence)
        );
        let hold = InputStep::Hold {
            button: Button::Ok,
            millis: 60_000,
        };
        assert_eq!(input(&[hold]), Err(Denial::HoldTooLong));
        assert_eq!(
            authorize_input(&control, ALICE, Some(EPOCH), true, now, &[tap()]),
            Err(Denial::Paused)
        );
        assert_eq!(
            authorize_input(
                &control,
                ALICE,
                Some(EPOCH),
                false,
                now + GRANT_TTL,
                &[tap()]
            ),
            Err(Denial::Expired)
        );

        let mut control = control.clone();
        for _ in 0..GRANT_ACTION_BUDGET {
            let next = authorize_input(&control, ALICE, Some(EPOCH), false, now, &[tap()]).unwrap();
            control = Control::Granted(next);
        }
        assert_eq!(
            authorize_input(&control, ALICE, Some(EPOCH), false, now, &[tap()]),
            Err(Denial::BudgetExhausted)
        );
    }
}

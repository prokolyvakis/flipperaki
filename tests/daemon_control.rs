//! The daemon's control workflow, end to end against a fake Flipper running
//! Buddy. No hardware: the daemon's device stream is a `tokio::io::duplex`.

mod support;

use std::time::Duration;

use flipperaki::broker::{Button, Denial, InputStep};
use flipperaki::buddy::{Activity, ActivityReport, pb as buddy};
use flipperaki::ipc::{Delivery, Failure, FinishMode, Reply, Request, Response};
use flipperaki::limits::{ARM_REQUEST_TTL, GRANT_ACTION_BUDGET};
use flipperaki::rpc::pb_gui;
use support::Content;
use support::daemon::{
    ALICE, BOB, BUDDY_PATH, Behaviour, CLOCK, Fake, Harness, is_app_start, is_input,
};
use tokio::sync::oneshot;

const APPROVE: i32 = buddy::Verdict::Approve as i32;
const DENY: i32 = buddy::Verdict::Deny as i32;

fn open(app: &str) -> Request {
    Request::Open { app: app.into() }
}

fn tap(button: Button) -> Request {
    Request::Input {
        steps: vec![InputStep::Tap { button }],
    }
}

fn denied(response: Response) -> Denial {
    match response {
        Err(Failure::Denied(denial)) => denial,
        other => panic!("expected a denial, got {other:?}"),
    }
}

async fn settle() {
    tokio::time::sleep(Duration::from_secs(2)).await;
}

/// Connected, Buddy greeted and on screen (a Snapshot proves it is ready).
async fn ready() -> (Harness, Fake, u32) {
    let mut daemon = Harness::start();
    let mut fake = daemon.next_fake().await;
    let epoch = fake.buddy_up().await;
    snapshot(&mut fake).await;
    (daemon, fake, epoch)
}

async fn snapshot(fake: &mut Fake) -> buddy::Snapshot {
    loop {
        if let Some(buddy::envelope::Payload::Snapshot(s)) = fake.next_buddy().await.payload {
            return s;
        }
    }
}

/// Opens the clock and waits for its ArmRequest.
async fn request_clock(
    daemon: &Harness,
    fake: &mut Fake,
) -> (oneshot::Receiver<Response>, buddy::ArmRequest) {
    let reply = daemon.submit(ALICE, open(CLOCK)).await;
    let request = fake.arm_request().await;
    (reply, request)
}

async fn reply(answer: oneshot::Receiver<Response>) -> Response {
    tokio::time::timeout(Duration::from_secs(60), answer)
        .await
        .expect("open answered")
        .expect("reply sent")
}

/// ALICE holds an approved grant on the running clock app.
async fn controlling() -> (Harness, Fake, u32) {
    let (daemon, mut fake, epoch) = ready().await;
    let (answer, request) = request_clock(&daemon, &mut fake).await;
    fake.decide(epoch, request.request_id, APPROVE);
    assert!(matches!(
        reply(answer).await,
        Ok(Reply::Opened {
            delivery: Delivery::Acknowledged,
            observation: Some(_),
            ..
        })
    ));
    (daemon, fake, epoch)
}

async fn control_is_none(daemon: &Harness) {
    for _ in 0..20 {
        if daemon.status(ALICE).await.control == "none" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("control was never released");
}

#[tokio::test(start_paused = true)]
async fn buddy_is_launched_in_rpc_mode_and_greeted_only_after_it_starts() {
    let mut daemon = Harness::start_with(Behaviour {
        announce_app_state: false,
        ..Behaviour::default()
    });
    let mut fake = daemon.next_fake().await;
    let start = fake
        .expect("AppStart(Buddy)", |c| is_app_start(c, BUDDY_PATH))
        .await;
    let Content::AppStartRequest(start) = start else {
        unreachable!()
    };
    assert_eq!(start.args, "RPC");

    // Buddy has not reported STARTED: nothing may be sent to it yet.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        fake.count(|c| matches!(c, Content::AppDataExchangeRequest(_))),
        0
    );

    // The daemon gives up on this launch; the next one is announced.
    fake.set(|b| {
        b.announce_app_state = true;
        b.loader_locked = false;
    });
    let hello = fake.next_buddy().await;
    let status = daemon.status(ALICE).await;
    assert_eq!(Some(hello.epoch), status.epoch);
    let Some(buddy::envelope::Payload::Hello(hello_body)) = hello.payload else {
        panic!("first Buddy message must be Hello, got {hello:?}");
    };
    assert_eq!(hello_body.role, buddy::Role::Host as i32);
    let last_start = fake
        .drain()
        .iter()
        .rposition(|c| is_app_start(c, BUDDY_PATH));
    let first_data = fake
        .seen
        .iter()
        .position(|c| matches!(c, Content::AppDataExchangeRequest(_)));
    assert!(first_data.is_some() && last_start < first_data);

    // Buddy answered Hello (the fake does so): the board follows.
    let snap = snapshot(&mut fake).await;
    assert_eq!(snap.activity, buddy::Activity::Idle as i32);
    assert_eq!(daemon.status(ALICE).await.foreground, "buddy");
}

#[tokio::test(start_paused = true)]
async fn activity_reports_reach_buddy_as_snapshots() {
    let (daemon, mut fake, _) = ready().await;
    let report = ActivityReport {
        session_key: "s1".into(),
        label: "repo".into(),
        activity: Some(Activity::Working),
    };
    assert_eq!(
        daemon.call(ALICE, Request::Activity { report }).await,
        Ok(Reply::Done)
    );
    let snap = snapshot(&mut fake).await;
    assert_eq!(snap.activity, buddy::Activity::Working as i32);
    assert_eq!(snap.session_label, "repo");
    assert_eq!(snap.active_sessions, 1);
}

#[tokio::test(start_paused = true)]
async fn approved_open_hands_off_from_buddy_to_the_exact_app() {
    let (daemon, mut fake, epoch) = ready().await;
    let (answer, request) = request_clock(&daemon, &mut fake).await;
    assert_eq!(request.app_label, "clock");
    assert_eq!(request.app_path, CLOCK);
    assert_eq!(request.max_actions, GRANT_ACTION_BUDGET);
    // Nothing happens before the physical decision.
    settle().await;
    assert_eq!(fake.count(|c| is_app_start(c, CLOCK)), 0);

    fake.decide(epoch, request.request_id, APPROVE);
    let opened = reply(answer).await;
    let Ok(Reply::Opened {
        delivery: Delivery::Acknowledged,
        observation: Some(observation),
        ..
    }) = opened
    else {
        panic!("unexpected {opened:?}");
    };
    assert_eq!(observation.epoch, epoch);

    let seen = fake.drain();
    let at = |wanted: &dyn Fn(&Content) -> bool| seen.iter().position(wanted).unwrap();
    let exit = at(&|c| matches!(c, Content::AppExitRequest(_)));
    let start = at(&|c| is_app_start(c, CLOCK));
    let stream = at(&|c| matches!(c, Content::GuiStartScreenStreamRequest(_)));
    let free = seen[exit..start]
        .iter()
        .any(|c| matches!(c, Content::AppLockStatusRequest(_)));
    assert!(exit < start && start < stream && free);
    let Content::AppStartRequest(start) = &seen[start] else {
        unreachable!()
    };
    assert_eq!(start.args, "", "targets never get the RPC argument");

    let status = daemon.status(ALICE).await;
    assert!(status.you_own_control);
    assert_eq!(status.foreground, "app: clock");
}

#[tokio::test(start_paused = true)]
async fn wrong_denied_or_unspecified_decisions_never_launch_the_app() {
    let (daemon, mut fake, epoch) = ready().await;

    let (answer, request) = request_clock(&daemon, &mut fake).await;
    fake.decide(epoch, request.request_id + 100, APPROVE);
    assert_eq!(denied(reply(answer).await), Denial::WrongRequest);
    // The stale prompt is withdrawn from Buddy, not left on screen.
    assert_eq!(fake.arm_cancel().await, request.request_id);

    let (answer, request) = request_clock(&daemon, &mut fake).await;
    fake.decide(epoch, request.request_id, DENY);
    assert_eq!(denied(reply(answer).await), Denial::DeniedByOperator);

    // A decision from another epoch is rejected like noise.
    let (mut answer, request) = request_clock(&daemon, &mut fake).await;
    fake.decide(epoch + 1, request.request_id, APPROVE);
    fake.decide(epoch, request.request_id, 0);
    settle().await;
    assert!(answer.try_recv().is_err(), "still pending");
    tokio::time::sleep(ARM_REQUEST_TTL).await;
    assert_eq!(denied(reply(answer).await), Denial::Expired);
    assert_eq!(fake.arm_cancel().await, request.request_id);

    // A late approval of the expired request changes nothing.
    fake.decide(epoch, request.request_id, APPROVE);
    settle().await;
    assert_eq!(fake.count(|c| is_app_start(c, CLOCK)), 0);
    assert_eq!(daemon.status(ALICE).await.control, "none");
}

#[tokio::test(start_paused = true)]
async fn a_second_client_cannot_use_or_steal_the_grant() {
    let (daemon, mut fake, _) = controlling().await;
    assert_eq!(
        denied(daemon.call(BOB, tap(Button::Ok)).await),
        Denial::NotOwner
    );
    assert_eq!(
        denied(daemon.call(BOB, Request::Observe).await),
        Denial::NotOwner
    );
    assert_eq!(
        denied(
            daemon
                .call(
                    BOB,
                    Request::Finish {
                        mode: FinishMode::LeaveRunning
                    }
                )
                .await
        ),
        Denial::NotOwner
    );
    assert_eq!(denied(daemon.call(BOB, open(CLOCK)).await), Denial::Busy);
    assert_eq!(fake.count(is_input), 0);
    assert!(daemon.status(ALICE).await.you_own_control);
    assert!(!daemon.status(BOB).await.you_own_control);
}

#[tokio::test(start_paused = true)]
async fn input_under_a_grant_reaches_the_device_and_spends_the_budget() {
    let (daemon, mut fake, _) = controlling().await;
    let result = daemon
        .call(
            ALICE,
            Request::Input {
                steps: vec![
                    InputStep::Tap { button: Button::Up },
                    InputStep::Tap { button: Button::Ok },
                ],
            },
        )
        .await;
    let Ok(Reply::InputResult {
        delivery: Delivery::Acknowledged,
        steps_delivered: 2,
        remaining_actions,
        observation: Some(_),
        ..
    }) = result
    else {
        panic!("unexpected {result:?}");
    };
    assert_eq!(remaining_actions, GRANT_ACTION_BUDGET - 2);
    let events: Vec<(i32, i32)> = fake
        .drain()
        .iter()
        .filter_map(|c| match c {
            Content::GuiSendInputEventRequest(e) => Some((e.key, e.r#type)),
            _ => None,
        })
        .collect();
    let (press, short, release) = (
        pb_gui::InputType::Press as i32,
        pb_gui::InputType::Short as i32,
        pb_gui::InputType::Release as i32,
    );
    let (up, ok) = (pb_gui::InputKey::Up as i32, pb_gui::InputKey::Ok as i32);
    assert_eq!(
        events,
        [
            (up, press),
            (up, short),
            (up, release),
            (ok, press),
            (ok, short),
            (ok, release)
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn input_after_the_app_exits_sends_nothing_and_revokes() {
    let (daemon, mut fake, _) = controlling().await;
    // The app quit without notice: nothing holds the loader any more.
    fake.set(|b| b.loader_locked = false);
    assert!(matches!(
        daemon.call(ALICE, tap(Button::Ok)).await,
        Err(Failure::Device(_))
    ));
    assert_eq!(fake.count(is_input), 0);
    assert_eq!(daemon.status(ALICE).await.control, "none");
    assert_eq!(
        denied(daemon.call(ALICE, tap(Button::Ok)).await),
        Denial::NoGrant
    );
    assert_eq!(fake.count(is_input), 0);
}

#[tokio::test(start_paused = true)]
async fn locking_the_device_revokes_control() {
    let (daemon, mut fake, _) = controlling().await;
    fake.lock(true);
    control_is_none(&daemon).await;
    assert_eq!(
        denied(daemon.call(ALICE, tap(Button::Ok)).await),
        Denial::NoGrant
    );
    assert_eq!(fake.count(is_input), 0);
}

#[tokio::test(start_paused = true)]
async fn pause_revokes_and_resume_restores_nothing() {
    let (daemon, mut fake, _) = controlling().await;
    let pause = |paused| Request::Pause { paused };
    assert_eq!(daemon.call(ALICE, pause(true)).await, Ok(Reply::Done));
    assert_eq!(
        denied(daemon.call(ALICE, tap(Button::Ok)).await),
        Denial::Paused
    );
    assert_eq!(daemon.call(BOB, pause(false)).await, Ok(Reply::Done));
    let status = daemon.status(ALICE).await;
    assert_eq!((status.control.as_str(), status.paused), ("none", false));
    assert_eq!(
        denied(daemon.call(ALICE, tap(Button::Ok)).await),
        Denial::NoGrant
    );
    assert_eq!(fake.count(is_input), 0);
}

#[tokio::test(start_paused = true)]
async fn a_departed_client_withdraws_its_pending_request() {
    let (daemon, mut fake, epoch) = ready().await;
    let (_answer, request) = request_clock(&daemon, &mut fake).await;
    daemon.gone(ALICE).await;
    assert_eq!(fake.arm_cancel().await, request.request_id);
    fake.decide(epoch, request.request_id, APPROVE);
    settle().await;
    assert_eq!(fake.count(|c| is_app_start(c, CLOCK)), 0);
    assert_eq!(daemon.status(ALICE).await.control, "none");
}

#[tokio::test(start_paused = true)]
async fn a_lost_device_ends_control_and_reconnects_disarmed() {
    let (mut daemon, fake, epoch) = controlling().await;
    fake.close();
    control_is_none(&daemon).await;
    assert_eq!(
        denied(daemon.call(ALICE, tap(Button::Ok)).await),
        Denial::NoGrant
    );

    let mut fake = daemon.next_fake().await;
    let new_epoch = fake.buddy_up().await;
    assert_eq!(new_epoch, epoch + 1);
    let status = daemon.status(ALICE).await;
    assert_eq!(status.epoch, Some(new_epoch));
    assert_eq!(status.control, "none");
    assert!(!status.you_own_control);
    assert_eq!(
        denied(daemon.call(ALICE, tap(Button::Ok)).await),
        Denial::NoGrant
    );
    assert_eq!(fake.count(is_input), 0);
}

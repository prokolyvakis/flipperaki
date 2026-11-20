//! Session executor contracts against a scripted fake Flipper.

mod support;

use std::time::Duration;

use flipperaki::rpc::{
    AppState, Delivery, DeviceEvent, Failure, Reason, Reply, RequestKind, SessionLimits,
    SessionState, Stage, pb, pb_app, pb_desktop, pb_gui,
};
use support::{Content, after, connect, input, ping, start};
use tokio::task::JoinHandle;

const SECOND: Duration = Duration::from_secs(1);

fn spawn_request(
    session: &flipperaki::rpc::RpcSession,
    content: Content,
    kind: RequestKind,
    timeout: Duration,
) -> JoinHandle<Delivery> {
    let session = session.clone();
    let deadline = after(timeout);
    tokio::spawn(async move { session.request(content, kind, deadline).await })
}

fn press() -> Content {
    input(pb_gui::InputKey::Ok, pb_gui::InputType::Press)
}

#[tokio::test(start_paused = true)]
async fn aggregates_multipart_replies_by_command_id() {
    let (session, _events, mut fake) = start();
    let first = spawn_request(&session, ping(b"a"), RequestKind::Query, SECOND);
    let a = fake.recv().await.command_id;
    let second = spawn_request(&session, ping(b"b"), RequestKind::Query, SECOND);
    let b = fake.recv().await.command_id;
    assert_ne!(a, b);

    // Interleave: b's two parts around a's single part.
    fake.send(b, 0, true, ping(b"b1")).await;
    fake.reply(a, ping(b"a1")).await;
    fake.reply(b, ping(b"b2")).await;

    assert_eq!(
        first.await.unwrap(),
        Delivery::Acknowledged(Reply(vec![ping(b"a1")]))
    );
    assert_eq!(
        second.await.unwrap(),
        Delivery::Acknowledged(Reply(vec![ping(b"b1"), ping(b"b2")]))
    );
}

#[tokio::test(start_paused = true)]
async fn non_ok_status_is_rejected() {
    let (session, _events, mut fake) = start();
    let request = spawn_request(&session, press(), RequestKind::Mutation, SECOND);
    let id = fake.recv().await.command_id;
    fake.reject(id, pb::CommandStatus::ErrorBusy as i32).await;
    assert_eq!(request.await.unwrap(), Delivery::Rejected { status: 4 });
}

#[tokio::test(start_paused = true)]
async fn unsolicited_frames_become_ordered_events() {
    let (_session, mut events, mut fake) = start();
    let frame = screen();
    fake.reply(0, frame.clone()).await;
    fake.reply(0, frame).await;
    fake.reply(
        0,
        Content::AppDataExchangeRequest(pb_app::DataExchangeRequest {
            data: b"hi".to_vec(),
        }),
    )
    .await;
    fake.reply(
        0,
        Content::AppStateResponse(pb_app::AppStateResponse {
            state: pb_app::AppState::AppStarted as i32,
        }),
    )
    .await;
    fake.reply(
        0,
        Content::DesktopStatus(pb_desktop::Status { locked: true }),
    )
    .await;

    let mut seqs = Vec::new();
    for _ in 0..2 {
        match events.recv().await {
            Some(DeviceEvent::Screen { seq, frame, .. }) => {
                assert_eq!(frame.data.len(), 1024);
                seqs.push(seq);
            }
            other => panic!("expected a screen frame, got {other:?}"),
        }
    }
    assert_eq!(seqs, [1, 2]);
    assert_eq!(
        events.recv().await,
        Some(DeviceEvent::AppData(b"hi".to_vec()))
    );
    assert_eq!(
        events.recv().await,
        Some(DeviceEvent::AppState(AppState::Started))
    );
    assert_eq!(
        events.recv().await,
        Some(DeviceEvent::Lock { locked: true })
    );
}

#[tokio::test(start_paused = true)]
async fn event_overflow_fails_the_session_and_pending_requests() {
    let limits = SessionLimits {
        event_queue: 2,
        ..SessionLimits::default()
    };
    let (session, mut events, mut fake) = connect(limits, 64 * 1024);
    let request = spawn_request(&session, ping(b"x"), RequestKind::Query, SECOND);
    fake.recv().await;
    for _ in 0..3 {
        fake.reply(0, screen()).await;
    }

    let failed = Reason::Failed(Failure::EventOverflow);
    assert_eq!(
        request.await.unwrap(),
        Delivery::Unknown {
            reason: failed.clone(),
            stage: Stage::Written
        }
    );
    assert_eq!(
        *session.state().borrow(),
        SessionState::Failed(Failure::EventOverflow)
    );
    // The two queued events are still delivered; nothing is silently lost.
    assert!(matches!(
        events.recv().await,
        Some(DeviceEvent::Screen { seq: 1, .. })
    ));
    assert!(matches!(
        events.recv().await,
        Some(DeviceEvent::Screen { seq: 2, .. })
    ));
    assert_eq!(
        session
            .request(ping(b"y"), RequestKind::Query, after(SECOND))
            .await,
        Delivery::NotSent(failed)
    );
}

#[tokio::test(start_paused = true)]
async fn expired_admission_sends_zero_bytes() {
    let (session, _events, mut fake) = start();
    let delivery = session
        .request(press(), RequestKind::Mutation, after(Duration::ZERO))
        .await;
    assert_eq!(delivery, Delivery::NotSent(Reason::Deadline));
    assert_eq!(fake.try_recv(SECOND).await, None);
}

#[tokio::test(start_paused = true)]
async fn queued_mutation_that_expires_is_never_sent() {
    let (session, _events, mut fake) = start();
    let first = spawn_request(&session, press(), RequestKind::Mutation, 10 * SECOND);
    let first_id = fake.recv().await.command_id;
    let second = spawn_request(&session, press(), RequestKind::Mutation, SECOND);

    assert_eq!(second.await.unwrap(), Delivery::NotSent(Reason::Deadline));
    fake.ack(first_id).await;
    assert!(matches!(first.await.unwrap(), Delivery::Acknowledged(_)));
    assert_eq!(fake.try_recv(20 * SECOND).await, None);
}

#[tokio::test(start_paused = true)]
async fn only_one_mutation_is_in_flight_but_queries_pass() {
    let (session, _events, mut fake) = start();
    let first = spawn_request(&session, press(), RequestKind::Mutation, 5 * SECOND);
    let first_id = fake.recv().await.command_id;
    let app = Content::AppExitRequest(pb_app::AppExitRequest {});
    // App transactions are serialized even if the caller calls them queries.
    let second = spawn_request(&session, app, RequestKind::Query, 5 * SECOND);
    let query = spawn_request(&session, ping(b"q"), RequestKind::Query, 5 * SECOND);

    let next = fake.recv().await;
    assert!(matches!(next.content, Some(Content::SystemPingRequest(_))));
    fake.reply(next.command_id, ping(b"q")).await;
    assert!(matches!(query.await.unwrap(), Delivery::Acknowledged(_)));
    assert_eq!(fake.try_recv(SECOND).await, None);

    fake.ack(first_id).await;
    let exit = fake.recv().await;
    assert!(matches!(exit.content, Some(Content::AppExitRequest(_))));
    fake.ack(exit.command_id).await;
    assert!(matches!(first.await.unwrap(), Delivery::Acknowledged(_)));
    assert!(matches!(second.await.unwrap(), Delivery::Acknowledged(_)));
}

#[tokio::test(start_paused = true)]
async fn late_reply_never_completes_another_request() {
    let (session, _events, mut fake) = start();
    let first = spawn_request(&session, ping(b"old"), RequestKind::Query, SECOND);
    let old = fake.recv().await.command_id;
    assert_eq!(
        first.await.unwrap(),
        Delivery::Unknown {
            reason: Reason::Deadline,
            stage: Stage::Written
        }
    );

    let second = spawn_request(&session, ping(b"new"), RequestKind::Query, SECOND);
    let new = fake.recv().await.command_id;
    assert_ne!(old, new);
    fake.reply(old, ping(b"old")).await;
    fake.reply(new, ping(b"new")).await;
    assert_eq!(
        second.await.unwrap(),
        Delivery::Acknowledged(Reply(vec![ping(b"new")]))
    );
}

#[tokio::test(start_paused = true)]
async fn pending_requests_are_bounded() {
    let limits = SessionLimits {
        pending_max: 2,
        ..SessionLimits::default()
    };
    let (session, _events, mut fake) = connect(limits, 64 * 1024);
    let _a = spawn_request(&session, ping(b"a"), RequestKind::Query, SECOND);
    let _b = spawn_request(&session, ping(b"b"), RequestKind::Query, SECOND);
    fake.recv().await;
    fake.recv().await;
    assert_eq!(
        session
            .request(ping(b"c"), RequestKind::Query, after(SECOND))
            .await,
        Delivery::NotSent(Reason::Busy)
    );
}

#[tokio::test(start_paused = true)]
async fn oversized_request_is_not_sent() {
    let limits = SessionLimits {
        frame_max_bytes: 16,
        ..SessionLimits::default()
    };
    let (session, _events, mut fake) = connect(limits, 64 * 1024);
    let delivery = session
        .request(ping(&[0; 64]), RequestKind::Query, after(SECOND))
        .await;
    assert_eq!(delivery, Delivery::NotSent(Reason::TooLarge));
    assert_eq!(fake.try_recv(SECOND).await, None);
}

#[tokio::test(start_paused = true)]
async fn oversized_multipart_reply_fails_the_session() {
    let limits = SessionLimits {
        response_max_bytes: 64,
        ..SessionLimits::default()
    };
    let (session, _events, mut fake) = connect(limits, 64 * 1024);
    let request = spawn_request(&session, ping(b"x"), RequestKind::Query, SECOND);
    let id = fake.recv().await.command_id;
    for _ in 0..3 {
        fake.send(id, 0, true, ping(&[7; 30])).await;
    }
    assert!(matches!(
        request.await.unwrap(),
        Delivery::Unknown {
            reason: Reason::Failed(Failure::ResponseTooLarge(64)),
            ..
        }
    ));
}

#[tokio::test(start_paused = true)]
async fn blocked_writer_does_not_starve_receive_and_fails_at_deadline() {
    // A 64-byte pipe that the fake never drains: the write blocks mid-frame.
    let (session, mut events, mut fake) = connect(SessionLimits::default(), 64);
    let request = spawn_request(&session, ping(&[1; 1000]), RequestKind::Query, SECOND);
    tokio::time::sleep(SECOND / 10).await;

    fake.reply(0, screen()).await;
    assert!(matches!(
        events.recv().await,
        Some(DeviceEvent::Screen { .. })
    ));

    assert_eq!(
        request.await.unwrap(),
        Delivery::Unknown {
            reason: Reason::Deadline,
            stage: Stage::Writing
        }
    );
    assert_eq!(
        *session.state().borrow(),
        SessionState::Failed(Failure::WriteStalled)
    );
    assert_eq!(
        events.recv().await,
        Some(DeviceEvent::Failed(Failure::WriteStalled))
    );
}

#[tokio::test(start_paused = true)]
async fn close_while_writing_resolves_everything() {
    let (session, _events, _fake) = connect(SessionLimits::default(), 64);
    let writing = spawn_request(&session, ping(&[1; 1000]), RequestKind::Query, SECOND);
    let queued = spawn_request(&session, press(), RequestKind::Mutation, SECOND);
    tokio::time::sleep(SECOND / 10).await;
    session.close().await;

    assert_eq!(
        writing.await.unwrap(),
        Delivery::Unknown {
            reason: Reason::Closed,
            stage: Stage::Writing
        }
    );
    assert_eq!(queued.await.unwrap(), Delivery::NotSent(Reason::Closed));
    assert_eq!(*session.state().borrow(), SessionState::Closed);
}

#[tokio::test(start_paused = true)]
async fn disconnect_after_send_is_unknown_never_replayed() {
    let (session, mut events, mut fake) = start();
    let request = spawn_request(&session, press(), RequestKind::Mutation, SECOND);
    fake.recv().await;
    drop(fake);
    assert_eq!(
        request.await.unwrap(),
        Delivery::Unknown {
            reason: Reason::Failed(Failure::Eof),
            stage: Stage::Written
        }
    );
    assert_eq!(events.recv().await, Some(DeviceEvent::Failed(Failure::Eof)));
}

#[tokio::test(start_paused = true)]
async fn malformed_frame_fails_the_session() {
    let (session, _events, mut fake) = start();
    // Length 3 followed by an invalid protobuf tag.
    fake.raw(&[3, 0xFF, 0xFF, 0xFF]).await;
    let mut state = session.state();
    let state = state
        .wait_for(|state| *state != SessionState::Open)
        .await
        .unwrap()
        .clone();
    assert!(matches!(state, SessionState::Failed(Failure::Frame(_))));
}

#[tokio::test(start_paused = true)]
async fn oversized_app_data_from_the_fap_fails_the_session() {
    let (session, _events, mut fake) = start();
    let data = vec![0; 513];
    fake.reply(
        0,
        Content::AppDataExchangeRequest(pb_app::DataExchangeRequest { data }),
    )
    .await;
    let mut state = session.state();
    let state = state
        .wait_for(|state| *state != SessionState::Open)
        .await
        .unwrap()
        .clone();
    assert!(matches!(state, SessionState::Failed(Failure::Protocol(_))));
}

fn screen() -> Content {
    Content::GuiScreenFrame(pb_gui::ScreenFrame {
        data: vec![0xAA; 1024],
        orientation: pb_gui::ScreenOrientation::Horizontal as i32,
    })
}

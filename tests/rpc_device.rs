//! Named device operations against the fake Flipper.

mod support;

use std::time::Duration;

use flipperaki::device::{Button, Device, DeviceError, EntryKind};
use flipperaki::rpc::{Reason, pb, pb_app, pb_gui, pb_property, pb_storage, pb_system};
use support::{Content, input, start};

fn device() -> (Device, support::FakeFlipper) {
    let (session, _events, fake) = start();
    (Device::new(session), fake)
}

#[tokio::test(start_paused = true)]
async fn tap_sends_press_short_release_in_order() {
    let (device, mut fake) = device();
    let tap = tokio::spawn(async move { device.tap(Button::Left).await });
    for kind in [
        pb_gui::InputType::Press,
        pb_gui::InputType::Short,
        pb_gui::InputType::Release,
    ] {
        let frame = fake.recv().await;
        assert_eq!(
            frame.content,
            Some(input(pb_gui::InputKey::Left, kind)),
            "step {kind:?}"
        );
        fake.ack(frame.command_id).await;
    }
    assert_eq!(tap.await.unwrap(), Ok(()));
}

#[tokio::test(start_paused = true)]
async fn hold_stops_at_first_failure() {
    let (device, mut fake) = device();
    let hold =
        tokio::spawn(async move { device.hold(Button::Ok, Duration::from_millis(800)).await });
    let press = fake.recv().await;
    fake.reject(press.command_id, pb::CommandStatus::Error as i32)
        .await;
    assert_eq!(
        hold.await.unwrap(),
        Err(DeviceError::Rejected { status: 1 })
    );
    assert_eq!(fake.try_recv(Duration::from_secs(10)).await, None);
}

#[tokio::test(start_paused = true)]
async fn hold_does_not_release_after_the_session_ends() {
    let (device, mut fake) = device();
    let session = device.session().clone();
    let hold =
        tokio::spawn(async move { device.hold(Button::Ok, Duration::from_millis(1500)).await });
    let press = fake.recv().await;
    fake.ack(press.command_id).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    session.close().await;
    assert_eq!(
        hold.await.unwrap(),
        Err(DeviceError::NotSent(Reason::Closed))
    );
    assert_eq!(fake.try_recv(Duration::from_secs(10)).await, None);
}

#[tokio::test(start_paused = true)]
async fn hold_is_bounded() {
    let (device, mut fake) = device();
    assert!(matches!(
        device.hold(Button::Ok, Duration::from_secs(2)).await,
        Err(DeviceError::InvalidArgument(_))
    ));
    assert_eq!(fake.try_recv(Duration::from_secs(1)).await, None);
}

#[tokio::test(start_paused = true)]
async fn lock_status_reads_the_command_status() {
    let (device, mut fake) = device();
    let device2 = device.clone();
    let locked = tokio::spawn(async move { device2.lock_status().await });
    let id = fake.recv().await.command_id;
    fake.ack(id).await;
    assert_eq!(locked.await.unwrap(), Ok(true));

    let unlocked = tokio::spawn(async move { device.lock_status().await });
    let id = fake.recv().await.command_id;
    fake.reject(id, pb::CommandStatus::Error as i32).await;
    assert_eq!(unlocked.await.unwrap(), Ok(false));
}

#[tokio::test(start_paused = true)]
async fn device_info_collects_multipart_properties() {
    let (device, mut fake) = device();
    let info = tokio::spawn(async move { device.device_info().await });
    let id = fake.recv().await.command_id;
    for (key, value, more) in [
        ("firmware.version", "mntm-010", true),
        ("firmware.branch", "release", false),
    ] {
        let pair = pb_property::GetResponse {
            key: key.into(),
            value: value.into(),
        };
        fake.send(id, 0, more, Content::PropertyGetResponse(pair))
            .await;
    }
    let info = info.await.unwrap().unwrap();
    assert_eq!(info.get("firmware.version"), Some("mntm-010"));
    assert_eq!(info.0.len(), 2);
}

#[tokio::test(start_paused = true)]
async fn ping_checks_the_echo() {
    let (device, mut fake) = device();
    let ping = tokio::spawn(async move { device.ping().await });
    let id = fake.recv().await.command_id;
    let wrong = pb_system::PingResponse {
        data: b"nope".to_vec(),
    };
    fake.reply(id, Content::SystemPingResponse(wrong)).await;
    assert_eq!(ping.await.unwrap(), Err(DeviceError::UnexpectedReply));
}

#[tokio::test(start_paused = true)]
async fn storage_list_maps_entries() {
    let (device, mut fake) = device();
    let list = tokio::spawn(async move { device.storage_list("/ext/apps").await });
    let frame = fake.recv().await;
    assert!(matches!(
        frame.content,
        Some(Content::StorageListRequest(ref request)) if request.path == "/ext/apps"
    ));
    let file = pb_storage::File {
        r#type: pb_storage::file::FileType::Dir as i32,
        name: "Tools".into(),
        ..Default::default()
    };
    let response = pb_storage::ListResponse { file: vec![file] };
    fake.reply(frame.command_id, Content::StorageListResponse(response))
        .await;
    let entries = list.await.unwrap().unwrap();
    assert_eq!(entries[0].name, "Tools");
    assert_eq!(entries[0].kind, EntryKind::Dir);
}

#[tokio::test(start_paused = true)]
async fn app_data_exchange_is_bounded_and_sent_as_mutation() {
    let (device, mut fake) = device();
    assert!(matches!(
        device.app_data_exchange(&[0; 513]).await,
        Err(DeviceError::InvalidArgument(_))
    ));
    let send = tokio::spawn(async move { device.app_data_exchange(b"hello").await });
    let frame = fake.recv().await;
    assert_eq!(
        frame.content,
        Some(Content::AppDataExchangeRequest(
            pb_app::DataExchangeRequest {
                data: b"hello".to_vec()
            }
        ))
    );
    fake.ack(frame.command_id).await;
    assert_eq!(send.await.unwrap(), Ok(()));
}

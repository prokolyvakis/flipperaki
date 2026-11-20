//! Flipper BLE serial service over btleplug (CoreBluetooth on macOS).
//!
//! GATT layout and flow control follow Momentum firmware
//! `targets/f7/ble_glue/services/serial_service{.c,.h,_uuid.inc}` at commit
//! `d3f89dfe2ef6b01839201598e9be1590cba80322`:
//! - TX (device to host) is indicated, up to 486 bytes per update.
//! - RX (host to device) is written, at most 486 bytes per write.
//! - Flow control is a big-endian `u32`: the bytes the device can accept.
//!   The device decrements it per write and notifies the full buffer size
//!   again only after its buffer drained, so the host spends that credit
//!   and waits at zero for the next notification.
//!
//! Unverified on hardware: pairing/bonding prompts, MTU, write pacing and
//! indication throughput.

use std::future::Future;
use std::io;
use std::time::Duration;

use btleplug::api::{
    Central, CentralEvent, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use bytes::Bytes;
use futures::{SinkExt, Stream, StreamExt, stream};
use tokio::sync::{mpsc, watch};
use tokio::time::timeout;
use tokio_util::io::{CopyToBytes, SinkWriter, StreamReader};
use tokio_util::sync::PollSender;
use uuid::Uuid;

use crate::transport::{RpcStream, TransportError};

pub const SERIAL_SERVICE: Uuid = Uuid::from_u128(0x8fe5b3d5_2e7f_4a98_2a48_7acc60fe0000);
/// Device to host (indicate).
pub const SERIAL_TX: Uuid = Uuid::from_u128(0x19ed82ae_ed21_4c9d_4145_228e61fe0000);
/// Host to device (write).
pub const SERIAL_RX: Uuid = Uuid::from_u128(0x19ed82ae_ed21_4c9d_4145_228e62fe0000);
/// Device receive credit (notify, big-endian u32).
pub const SERIAL_FLOW_CONTROL: Uuid = Uuid::from_u128(0x19ed82ae_ed21_4c9d_4145_228e63fe0000);
pub const SERIAL_RPC_STATUS: Uuid = Uuid::from_u128(0x19ed82ae_ed21_4c9d_4145_228e64fe0000);

/// Firmware `BLE_SVC_SERIAL_DATA_LEN_MAX`.
const RX_WRITE_MAX: usize = 486;
/// ATT write header.
const ATT_HEADER: usize = 3;
/// Bounded inbound buffer (indications); overflow fails the transport.
const INBOUND_CHUNKS: usize = 64;
const OUTBOUND_FRAMES: usize = 4;

/// A CoreBluetooth peripheral identifier, persisted by the caller at enrolment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlePeripheralId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    pub id: BlePeripheralId,
    pub name: Option<String>,
}

/// Scans for `duration` and lists advertising Flippers.
pub async fn scan(duration: Duration) -> Result<Vec<Discovered>, TransportError> {
    let adapter = adapter().await?;
    adapter
        .start_scan(ScanFilter::default())
        .await
        .map_err(ble)?;
    tokio::time::sleep(duration).await;
    adapter.stop_scan().await.map_err(ble)?;
    let mut found = Vec::new();
    for peripheral in adapter.peripherals().await.map_err(ble)? {
        let Some(properties) = peripheral.properties().await.map_err(ble)? else {
            continue;
        };
        if is_flipper(properties.local_name.as_deref(), &properties.services) {
            found.push(Discovered {
                id: BlePeripheralId(peripheral.id().to_string()),
                name: properties.local_name,
            });
        }
    }
    Ok(found)
}

/// Connects to an enrolled Flipper and returns the serial byte stream.
pub async fn connect(
    id: &BlePeripheralId,
    scan_timeout: Duration,
) -> Result<impl RpcStream + use<>, TransportError> {
    let adapter = adapter().await?;
    let mut events = adapter.events().await.map_err(ble)?;
    adapter
        .start_scan(ScanFilter::default())
        .await
        .map_err(ble)?;
    let found = timeout(scan_timeout, async {
        loop {
            for peripheral in adapter.peripherals().await.map_err(ble)? {
                if peripheral.id().to_string().eq_ignore_ascii_case(&id.0) {
                    return Ok(peripheral);
                }
            }
            if events.next().await.is_none() {
                return Err(TransportError::Ble("adapter event stream ended".into()));
            }
        }
    })
    .await;
    adapter.stop_scan().await.map_err(ble)?;
    let peripheral = found.map_err(|_| TransportError::NotFound)??;

    peripheral.connect().await.map_err(ble)?;
    peripheral.discover_services().await.map_err(ble)?;
    let tx = characteristic(&peripheral, SERIAL_TX)?;
    let rx = characteristic(&peripheral, SERIAL_RX)?;
    let flow = characteristic(&peripheral, SERIAL_FLOW_CONTROL)?;
    let notifications = peripheral.notifications().await.map_err(ble)?;
    peripheral.subscribe(&tx).await.map_err(ble)?;
    peripheral.subscribe(&flow).await.map_err(ble)?;
    let initial = parse_credit(&peripheral.read(&flow).await.map_err(ble)?)
        .ok_or_else(|| TransportError::Ble("bad flow control value".into()))?;
    let max_chunk = chunk_limit(peripheral.mtu());
    tracing::info!(peripheral = %id.0, initial, max_chunk, "BLE serial connected");

    // Either stream ending is a failure: the patched btleplug ends them on
    // lag (third_party/btleplug/PATCHES.md).
    let own_id = peripheral.id();
    let data = notifications
        .filter_map(|n| std::future::ready(classify(n.uuid, n.value)))
        .chain(stream::once(lost("notification stream ended")));
    let disconnects = events
        .filter_map(move |event| {
            std::future::ready(match event {
                CentralEvent::DeviceDisconnected(gone) if gone == own_id => {
                    Some(Incoming::Lost("peripheral disconnected"))
                }
                _ => None,
            })
        })
        .chain(stream::once(lost("adapter event stream ended")));
    let incoming = stream::select(data, disconnects);

    let (inbound_tx, inbound_rx) = mpsc::channel(INBOUND_CHUNKS);
    let (outbound_tx, outbound_rx) = mpsc::channel(OUTBOUND_FRAMES);
    let (credit_tx, credit_rx) = watch::channel(initial);
    let reader_side = peripheral.clone();
    tokio::spawn(async move {
        pump(incoming, inbound_tx, credit_tx).await;
        let _ = reader_side.disconnect().await;
    });
    tokio::spawn(async move {
        let write = |chunk: Vec<u8>| {
            let peripheral = peripheral.clone();
            let rx = rx.clone();
            async move {
                peripheral
                    .write(&rx, &chunk, WriteType::WithResponse)
                    .await
                    .map_err(|error| error.to_string())
            }
        };
        if let Err(error) = write_out(outbound_rx, credit_rx, max_chunk, write).await {
            tracing::warn!(%error, "BLE write failed");
        }
    });
    Ok(bridge(inbound_rx, outbound_tx))
}

async fn adapter() -> Result<Adapter, TransportError> {
    let manager = Manager::new().await.map_err(ble)?;
    manager
        .adapters()
        .await
        .map_err(ble)?
        .into_iter()
        .next()
        .ok_or(TransportError::NotFound)
}

fn characteristic(peripheral: &Peripheral, uuid: Uuid) -> Result<Characteristic, TransportError> {
    peripheral
        .characteristics()
        .into_iter()
        .find(|c| c.uuid == uuid && c.service_uuid == SERIAL_SERVICE)
        .ok_or_else(|| TransportError::Ble(format!("missing characteristic {uuid}")))
}

fn ble(error: btleplug::Error) -> TransportError {
    TransportError::Ble(error.to_string())
}

/// Flippers advertise `0x3080 | hw_color` (16-bit) and a "Flipper" name.
fn is_flipper(name: Option<&str>, services: &[Uuid]) -> bool {
    let advertised = services.iter().any(|uuid| {
        let (short, rest) = (uuid.as_u128() >> 96, uuid.as_u128() & ((1 << 96) - 1));
        rest == 0x0000_1000_8000_0080_5f9b_34fb && (0x3080..=0x308F).contains(&short)
    });
    advertised || name.is_some_and(|name| name.starts_with("Flipper "))
}

#[derive(Debug, PartialEq, Eq)]
enum Incoming {
    Data(Bytes),
    Credit(u32),
    Lost(&'static str),
}

fn lost(reason: &'static str) -> std::future::Ready<Incoming> {
    std::future::ready(Incoming::Lost(reason))
}

fn classify(uuid: Uuid, value: Vec<u8>) -> Option<Incoming> {
    if uuid == SERIAL_TX {
        Some(Incoming::Data(Bytes::from(value)))
    } else if uuid == SERIAL_FLOW_CONTROL {
        Some(
            parse_credit(&value).map_or(Incoming::Lost("bad flow control value"), Incoming::Credit),
        )
    } else {
        None
    }
}

fn parse_credit(value: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(value.try_into().ok()?))
}

fn chunk_limit(mtu: u16) -> usize {
    usize::from(mtu)
        .saturating_sub(ATT_HEADER)
        .clamp(1, RX_WRITE_MAX)
}

/// Forwards indications into the bounded inbound channel. Overflow, loss
/// and disconnect end it with an error the session reader observes.
async fn pump(
    incoming: impl Stream<Item = Incoming>,
    data: mpsc::Sender<io::Result<Bytes>>,
    credit: watch::Sender<u32>,
) {
    let mut incoming = std::pin::pin!(incoming);
    let reason = loop {
        let item = tokio::select! {
            item = incoming.next() => item,
            () = data.closed() => return,
        };
        match item {
            Some(Incoming::Data(bytes)) => match data.try_send(Ok(bytes)) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Closed(_)) => return,
                Err(mpsc::error::TrySendError::Full(_)) => break "inbound buffer overflow",
            },
            Some(Incoming::Credit(value)) => {
                credit.send_replace(value);
            }
            Some(Incoming::Lost(reason)) => break reason,
            None => break "incoming stream ended",
        }
    };
    tracing::warn!(reason, "BLE transport failed");
    let _ = data.send(Err(io::Error::other(reason))).await;
}

/// Writes outbound frames in chunks of at most `max_chunk` bytes, never
/// exceeding the device's announced credit.
async fn write_out<F, Fut>(
    mut outbound: mpsc::Receiver<Bytes>,
    mut credit: watch::Receiver<u32>,
    max_chunk: usize,
    mut write: F,
) -> Result<(), String>
where
    F: FnMut(Vec<u8>) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let mut available = usize::try_from(*credit.borrow_and_update()).unwrap_or(usize::MAX);
    while let Some(mut data) = outbound.recv().await {
        while !data.is_empty() {
            while available == 0 {
                credit
                    .changed()
                    .await
                    .map_err(|_| "flow control ended".to_owned())?;
                available = usize::try_from(*credit.borrow_and_update()).unwrap_or(usize::MAX);
            }
            let length = data.len().min(available).min(max_chunk);
            write(data.split_to(length).to_vec()).await?;
            available -= length;
        }
    }
    Ok(())
}

fn bridge(
    mut inbound: mpsc::Receiver<io::Result<Bytes>>,
    outbound: mpsc::Sender<Bytes>,
) -> impl RpcStream + use<> {
    let reader = StreamReader::new(stream::poll_fn(move |cx| inbound.poll_recv(cx)));
    let sink =
        PollSender::new(outbound).sink_map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe));
    tokio::io::join(reader, SinkWriter::new(CopyToBytes::new(sink)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// UUID byte arrays exactly as in `serial_service_uuid.inc` (little-endian).
    #[test]
    fn uuids_match_firmware_byte_arrays() {
        let le = |bytes: [u8; 16]| {
            let mut be = bytes;
            be.reverse();
            Uuid::from_bytes(be)
        };
        let service = [
            0x00, 0x00, 0xfe, 0x60, 0xcc, 0x7a, 0x48, 0x2a, 0x98, 0x4a, 0x7f, 0x2e, 0xd5, 0xb3,
            0xe5, 0x8f,
        ];
        assert_eq!(le(service), SERIAL_SERVICE);
        for (last, uuid) in [
            (0x61, SERIAL_TX),
            (0x62, SERIAL_RX),
            (0x63, SERIAL_FLOW_CONTROL),
            (0x64, SERIAL_RPC_STATUS),
        ] {
            let bytes = [
                0x00, 0x00, 0xfe, last, 0x8e, 0x22, 0x45, 0x41, 0x9d, 0x4c, 0x21, 0xed, 0xae, 0x82,
                0xed, 0x19,
            ];
            assert_eq!(le(bytes), uuid);
        }
    }

    #[test]
    fn recognises_flipper_advertisements() {
        let short = |n: u128| Uuid::from_u128((n << 96) | 0x0000_1000_8000_0080_5f9b_34fb);
        assert!(is_flipper(None, &[short(0x3082)]));
        assert!(is_flipper(Some("Flipper Ozyn"), &[]));
        assert!(!is_flipper(Some("Keyboard"), &[short(0x180F)]));
    }

    #[test]
    fn flow_control_is_big_endian_u32() {
        assert_eq!(parse_credit(&[0, 0, 0x04, 0x00]), Some(1024));
        assert_eq!(parse_credit(&[1, 2, 3]), None);
        assert_eq!(
            classify(SERIAL_FLOW_CONTROL, vec![9]),
            Some(Incoming::Lost("bad flow control value"))
        );
        assert_eq!(classify(SERIAL_RPC_STATUS, vec![1, 0, 0, 0]), None);
    }

    #[test]
    fn chunks_fit_mtu_and_firmware_limit() {
        assert_eq!(chunk_limit(23), 20);
        assert_eq!(chunk_limit(185), 182);
        assert_eq!(chunk_limit(527), RX_WRITE_MAX);
        assert_eq!(chunk_limit(0), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn writes_respect_credit_and_chunk_size() {
        let (outbound_tx, outbound_rx) = mpsc::channel(4);
        let (credit_tx, credit_rx) = watch::channel(10);
        let (sent_tx, mut sent) = mpsc::unbounded_channel();
        let writer = tokio::spawn(write_out(outbound_rx, credit_rx, 4, move |chunk| {
            let _ = sent_tx.send(chunk.len());
            std::future::ready(Ok(()))
        }));
        outbound_tx.send(Bytes::from(vec![0; 13])).await.unwrap();
        for expected in [4, 4, 2] {
            assert_eq!(sent.recv().await, Some(expected));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(sent.try_recv().is_err(), "wrote without credit");
        credit_tx.send_replace(10);
        assert_eq!(sent.recv().await, Some(3));
        drop(outbound_tx);
        assert_eq!(writer.await.unwrap(), Ok(()));
    }

    #[tokio::test(start_paused = true)]
    async fn overflow_and_loss_surface_as_read_errors() {
        let (inbound_tx, inbound_rx) = mpsc::channel(2);
        let (outbound_tx, _outbound_rx) = mpsc::channel(1);
        let (credit_tx, _credit_rx) = watch::channel(0);
        let mut stream = bridge(inbound_rx, outbound_tx);
        let chunks = (0..3).map(|i| Incoming::Data(Bytes::from(vec![i; 2])));
        tokio::spawn(pump(stream::iter(chunks), inbound_tx, credit_tx));

        let mut got = Vec::new();
        let error = stream.read_to_end(&mut got).await.unwrap_err();
        assert_eq!(got, [0, 0, 1, 1]);
        assert_eq!(error.to_string(), "inbound buffer overflow");
    }

    #[tokio::test(start_paused = true)]
    async fn loss_ends_the_stream_with_an_error_and_credit_updates() {
        let (inbound_tx, inbound_rx) = mpsc::channel(8);
        let (outbound_tx, mut outbound_rx) = mpsc::channel(1);
        let (credit_tx, credit_rx) = watch::channel(0);
        let mut stream = bridge(inbound_rx, outbound_tx);
        let items = [
            Incoming::Credit(486),
            Incoming::Data(Bytes::from_static(b"hi")),
            Incoming::Lost("peripheral disconnected"),
        ];
        tokio::spawn(pump(stream::iter(items), inbound_tx, credit_tx));
        let mut got = Vec::new();
        let error = stream.read_to_end(&mut got).await.unwrap_err();
        assert_eq!(got, b"hi");
        assert_eq!(error.to_string(), "peripheral disconnected");
        assert_eq!(*credit_rx.borrow(), 486);

        stream.write_all(b"frame").await.unwrap();
        stream.flush().await.unwrap();
        assert_eq!(
            outbound_rx.recv().await.unwrap(),
            Bytes::from_static(b"frame")
        );
    }
}

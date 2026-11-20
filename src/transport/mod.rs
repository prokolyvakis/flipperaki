//! Byte transports for the RPC session. Opening a transport yields a plain
//! async byte stream; a transport change is a new session (new epoch).

pub mod ble;
pub mod usb;

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

/// Any duplex byte stream the RPC session can run on.
pub trait RpcStream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> RpcStream for T {}

pub type BoxedStream = Box<dyn RpcStream>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    Ble(ble::BlePeripheralId),
    Usb,
}

/// How long `open` scans for an enrolled BLE peripheral.
pub const BLE_SCAN_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("no Flipper found")]
    NotFound,
    #[error("transport i/o error: {0}")]
    Io(String),
    #[error("bluetooth error: {0}")]
    Ble(String),
    #[error("RPC handshake failed: {0}")]
    Handshake(&'static str),
}

impl From<std::io::Error> for TransportError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

/// Opens `transport` and returns a stream already in RPC mode.
pub async fn open(transport: &Transport) -> Result<BoxedStream, TransportError> {
    match transport {
        Transport::Ble(id) => Ok(Box::new(ble::connect(id, BLE_SCAN_TIMEOUT).await?)),
        Transport::Usb => Ok(Box::new(usb::open().await?)),
    }
}

//! Flipper USB CDC serial: discovery and the CLI-to-RPC handshake.
//!
//! Follows `flipper_companion_protocol::serial::start_session`: toggle DTR,
//! wait (advisory) for the `>: ` prompt, send `start_rpc_session\r`, wait for
//! its echo. Unlike upstream we read byte by byte and stop right after the
//! echo's `\n`, so no RPC byte that follows is swallowed.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::{sleep, timeout};
use tokio_serial::{SerialPort, SerialPortInfo, SerialPortType, SerialStream};

use crate::transport::TransportError;

/// STMicroelectronics VID / Flipper CDC (VCP) PID.
pub const FLIPPER_USB_VID: u16 = 0x0483;
pub const FLIPPER_USB_PID: u16 = 0x5740;

const BAUD_RATE: u32 = 115_200;
const DTR_SETTLE: Duration = Duration::from_millis(50);
const PROMPT_TIMEOUT: Duration = Duration::from_secs(2);
const ECHO_TIMEOUT: Duration = Duration::from_secs(2);
/// shortcut: local bound on CLI bytes (MOTD + echo) read during the handshake;
/// move to `crate::limits` if it ever needs tuning.
const HANDSHAKE_MAX_BYTES: usize = 16 * 1024;
const PROMPT: &[u8] = b">: ";
const START_RPC: &[u8] = b"start_rpc_session\r";

/// Picks the Flipper port, preferring macOS `/dev/cu.*` call-out devices.
#[must_use]
pub fn select_port(ports: &[SerialPortInfo]) -> Option<String> {
    let mut flippers: Vec<&str> = ports
        .iter()
        .filter(|port| {
            matches!(&port.port_type, SerialPortType::UsbPort(usb)
                if usb.vid == FLIPPER_USB_VID && usb.pid == FLIPPER_USB_PID)
        })
        .map(|port| port.port_name.as_str())
        .collect();
    flippers.sort_by_key(|name| !name.starts_with("/dev/cu."));
    flippers.first().map(|name| (*name).to_owned())
}

/// Finds, opens and switches the first Flipper serial port to RPC mode.
pub async fn open() -> Result<SerialStream, TransportError> {
    let ports =
        tokio_serial::available_ports().map_err(|error| TransportError::Io(error.to_string()))?;
    let path = select_port(&ports).ok_or(TransportError::NotFound)?;
    tracing::info!(%path, "opening Flipper USB serial port");
    let mut stream = tokio_serial::SerialStream::open(&tokio_serial::new(&path, BAUD_RATE))
        .map_err(|error| TransportError::Io(error.to_string()))?;
    // A DTR toggle resets the CLI to a known prompt state.
    if let Err(error) = stream.write_data_terminal_ready(false) {
        tracing::warn!(%error, "DTR toggle failed");
    }
    sleep(DTR_SETTLE).await;
    if let Err(error) = stream.write_data_terminal_ready(true) {
        tracing::warn!(%error, "DTR toggle failed");
    }
    enter_rpc(&mut stream).await?;
    Ok(stream)
}

/// Runs the CLI handshake on an already-open stream.
pub async fn enter_rpc<S>(stream: &mut S) -> Result<(), TransportError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut seen = Vec::new();
    // The MOTD and prompt are advisory: some configurations print neither.
    let _ = timeout(PROMPT_TIMEOUT, read_until(stream, &mut seen, PROMPT)).await;
    stream.write_all(START_RPC).await?;
    stream.flush().await?;
    let mut echo = Vec::new();
    let expected = [START_RPC, b"\n"].concat();
    timeout(ECHO_TIMEOUT, read_until(stream, &mut echo, &expected))
        .await
        .map_err(|_| TransportError::Handshake("no start_rpc_session echo"))?
}

async fn read_until<S>(stream: &mut S, seen: &mut Vec<u8>, end: &[u8]) -> Result<(), TransportError>
where
    S: AsyncRead + Unpin,
{
    while !seen.ends_with(end) {
        if seen.len() >= HANDSHAKE_MAX_BYTES {
            return Err(TransportError::Handshake("CLI output exceeds bound"));
        }
        seen.push(stream.read_u8().await?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_serial::UsbPortInfo;

    fn port(name: &str, vid: u16, pid: u16) -> SerialPortInfo {
        SerialPortInfo {
            port_name: name.to_owned(),
            port_type: SerialPortType::UsbPort(UsbPortInfo {
                vid,
                pid,
                serial_number: None,
                manufacturer: None,
                product: None,
            }),
        }
    }

    #[test]
    fn selects_flipper_call_out_port() {
        let ports = [
            port("/dev/tty.usbmodemflip_X1", FLIPPER_USB_VID, FLIPPER_USB_PID),
            port("/dev/cu.other", 0x1234, FLIPPER_USB_PID),
            port("/dev/cu.usbmodemflip_X1", FLIPPER_USB_VID, FLIPPER_USB_PID),
        ];
        assert_eq!(
            select_port(&ports).as_deref(),
            Some("/dev/cu.usbmodemflip_X1")
        );
        assert_eq!(select_port(&ports[1..2]), None);
    }

    #[tokio::test(start_paused = true)]
    async fn handshake_stops_right_after_the_echo() {
        let (mut host, mut device) = tokio::io::duplex(4096);
        let fake = tokio::spawn(async move {
            device.write_all(b"Welcome\r\n>: ").await.unwrap();
            let mut command = [0; START_RPC.len()];
            device.read_exact(&mut command).await.unwrap();
            assert_eq!(&command, START_RPC);
            // Echo plus the first RPC byte, which must not be consumed.
            device
                .write_all(b"start_rpc_session\r\n\x07")
                .await
                .unwrap();
            device
        });
        enter_rpc(&mut host).await.unwrap();
        assert_eq!(host.read_u8().await.unwrap(), 0x07);
        drop(fake.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn handshake_times_out_without_echo() {
        let (mut host, _device) = tokio::io::duplex(4096);
        assert!(matches!(
            enter_rpc(&mut host).await,
            Err(TransportError::Handshake(_))
        ));
    }
}

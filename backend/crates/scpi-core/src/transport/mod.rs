//! Transport abstraction and built-in transports.
//!
//! A transport only moves bytes; SCPI framing, typed parsing, and command
//! validation live above it. Instances are single-connection and mutable.

use std::time::Duration;

use crate::error::{Error, Result};

pub mod mock;
pub mod tcp;
pub mod usb;
pub mod usbtmc;

pub trait Transport: Send {
    /// Human-readable resource description for logs and `info`.
    fn description(&self) -> String;

    /// Write one already-framed SCPI message.
    fn write_all(&mut self, bytes: &[u8]) -> Result<()>;

    /// Read up to `buf.len()` bytes. Returning `Ok(0)` means end of stream.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize>;

    /// Read whatever is available without waiting for new data.
    /// `Ok(0)` means "nothing available right now".
    fn read_available(&mut self, _buf: &mut [u8]) -> Result<usize> {
        Ok(0)
    }

    /// Discard pending input, where the transport supports it.
    fn clear(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Open a transport from a spec:
///
/// * `mock` (silent) or `mock:<responses.json>` (scripted)
/// * `tcp://host:port` (raw SCPI socket)
/// * `usbtmc:/dev/usbtmc0` or `/dev/usbtmc0`
pub fn open(spec: &str, timeout: Duration) -> Result<Box<dyn Transport>> {
    let spec = spec.trim();
    match spec {
        "mock" | "mock:empty" => Ok(Box::new(mock::MockTransport::empty())),
        _ => {
            if let Some(path) = spec.strip_prefix("mock:") {
                return Ok(Box::new(mock::MockTransport::from_file(
                    std::path::Path::new(path),
                )?));
            }
            if let Some(addr) = spec
                .strip_prefix("tcp://")
                .or_else(|| spec.strip_prefix("tcp:"))
            {
                return Ok(Box::new(tcp::TcpTransport::connect(addr, timeout)?));
            }
            if let Some(rest) = spec.strip_prefix("usb:") {
                return usb::open(rest, timeout);
            }
            if let Some(path) = spec.strip_prefix("usbtmc:") {
                return usbtmc::open(path, timeout);
            }
            if spec.starts_with("/dev/usbtmc") {
                return usbtmc::open(spec, timeout);
            }
            Err(Error::TransportSpec(spec.to_string()))
        }
    }
}

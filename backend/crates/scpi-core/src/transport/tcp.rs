//! Raw SCPI over TCP (many instruments expose a socket on port 5025 or 5555).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::error::{Error, Result};

use super::Transport;

pub struct TcpTransport {
    stream: TcpStream,
    timeout: Duration,
    description: String,
}

impl TcpTransport {
    pub fn connect(addr: &str, timeout: Duration) -> Result<Self> {
        // A hostname can resolve to several addresses (IPv4 + IPv6, multiple
        // A records). Try each in turn instead of only the first one.
        let targets: Vec<SocketAddr> = addr
            .to_socket_addrs()
            .map_err(|e| Error::Transport(format!("cannot resolve '{addr}': {e}")))?
            .collect();
        if targets.is_empty() {
            return Err(Error::Transport(format!("no address for '{addr}'")));
        }
        let mut stream = None;
        let mut last_error = None;
        for target in &targets {
            match TcpStream::connect_timeout(target, timeout) {
                Ok(connected) => {
                    stream = Some((connected, *target));
                    break;
                }
                Err(error) => last_error = Some((*target, error)),
            }
        }
        let Some((stream, target)) = stream else {
            let (target, error) = last_error.expect("targets is not empty");
            return Err(Error::Transport(format!(
                "cannot connect to {target}: {error}"
            )));
        };
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        stream.set_nodelay(true)?;
        Ok(Self {
            stream,
            timeout,
            description: format!("tcp://{target}"),
        })
    }

    fn map_error(&self, error: std::io::Error) -> Error {
        match error.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                Error::Timeout(self.timeout)
            }
            _ => Error::Io(error),
        }
    }
}

impl Transport for TcpTransport {
    fn description(&self) -> String {
        self.description.clone()
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.stream.write_all(bytes).map_err(|e| self.map_error(e))
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.stream.read(buf).map_err(|e| self.map_error(e))
    }

    fn read_available(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.stream.set_nonblocking(true)?;
        let result = self.stream.read(buf);
        let restored = self.stream.set_nonblocking(false);
        match result {
            Ok(read) => {
                restored?;
                Ok(read)
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                restored?;
                Ok(0)
            }
            Err(error) => {
                let _ = restored;
                Err(self.map_error(error))
            }
        }
    }

    fn clear(&mut self) -> Result<()> {
        self.stream.set_nonblocking(true)?;
        let mut scratch = [0u8; 1024];
        loop {
            match self.stream.read(&mut scratch) {
                Ok(0) => break,
                Ok(_) => continue,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    let _ = self.stream.set_nonblocking(false);
                    return Err(e.into());
                }
            }
        }
        self.stream.set_nonblocking(false)?;
        Ok(())
    }
}

//! Linux USBTMC transport using the kernel's `/dev/usbtmc*` character device.
//!
//! This is intentionally small: the kernel driver handles USB framing, and we
//! only add non-blocking I/O plus a poll-based timeout. It does not implement
//! USBTMC `CLEAR` (that needs a control transfer) or SRQ handling.

use std::time::Duration;

use crate::error::Result;

use super::Transport;

#[cfg(target_os = "linux")]
pub fn open(path: &str, timeout: Duration) -> Result<Box<dyn Transport>> {
    imp::open(path, timeout)
}

#[cfg(not(target_os = "linux"))]
pub fn open(_path: &str, _timeout: Duration) -> Result<Box<dyn Transport>> {
    Err(crate::error::Error::FeatureDisabled(
        "USBTMC is only implemented for Linux (/dev/usbtmc*)",
    ))
}

#[cfg(target_os = "linux")]
mod imp {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::fd::{AsRawFd, RawFd};
    use std::time::Duration;

    use crate::error::{Error, Result};

    use super::super::Transport;

    pub struct UsbtmcTransport {
        file: File,
        timeout: Duration,
        description: String,
    }

    pub fn open(path: &str, timeout: Duration) -> Result<Box<dyn Transport>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| Error::Transport(format!("cannot open {path}: {e}")))?;
        set_nonblocking(file.as_raw_fd(), true)?;
        Ok(Box::new(UsbtmcTransport {
            file,
            timeout,
            description: path.to_string(),
        }))
    }

    impl Transport for UsbtmcTransport {
        fn description(&self) -> String {
            self.description.clone()
        }

        fn write_all(&mut self, mut bytes: &[u8]) -> Result<()> {
            let fd = self.file.as_raw_fd();
            while !bytes.is_empty() {
                let written =
                    unsafe { libc::write(fd, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
                if written > 0 {
                    bytes = &bytes[written as usize..];
                    continue;
                }
                if written == 0 {
                    return Err(Error::Transport("USBTMC write returned 0".into()));
                }
                let error = io::Error::last_os_error();
                match error.kind() {
                    io::ErrorKind::Interrupted => continue,
                    io::ErrorKind::WouldBlock => {
                        if !poll(fd, libc::POLLOUT, self.timeout)? {
                            return Err(Error::Timeout(self.timeout));
                        }
                    }
                    _ => return Err(Error::Io(error)),
                }
            }
            Ok(())
        }

        fn read_available(&mut self, buf: &mut [u8]) -> Result<usize> {
            let fd = self.file.as_raw_fd();
            if !poll(fd, libc::POLLIN, Duration::ZERO)? {
                return Ok(0);
            }
            let read = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if read >= 0 {
                return Ok(read as usize);
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                Ok(0)
            } else {
                Err(Error::Io(error))
            }
        }

        fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
            let fd = self.file.as_raw_fd();
            loop {
                let read =
                    unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
                if read >= 0 {
                    return Ok(read as usize);
                }
                let error = io::Error::last_os_error();
                match error.kind() {
                    io::ErrorKind::Interrupted => continue,
                    io::ErrorKind::WouldBlock => {
                        if !poll(fd, libc::POLLIN, self.timeout)? {
                            return Err(Error::Timeout(self.timeout));
                        }
                    }
                    _ => return Err(Error::Io(error)),
                }
            }
        }
    }

    fn set_nonblocking(fd: RawFd, enabled: bool) -> Result<()> {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let flags = if enabled {
            flags | libc::O_NONBLOCK
        } else {
            flags & !libc::O_NONBLOCK
        };
        if unsafe { libc::fcntl(fd, libc::F_SETFL, flags) } < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }

    fn poll(fd: RawFd, events: libc::c_short, timeout: Duration) -> Result<bool> {
        let mut descriptor = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
        let ready = unsafe { libc::poll(&mut descriptor, 1, millis) };
        if ready < 0 {
            return Err(io::Error::last_os_error().into());
        }
        if ready == 0 {
            return Ok(false);
        }
        if descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(Error::Transport("USBTMC device reported an error".into()));
        }
        Ok(true)
    }
}

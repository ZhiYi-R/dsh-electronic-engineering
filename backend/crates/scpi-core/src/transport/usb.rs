//! Cross-platform USBTMC over raw USB using `nusb`.
//!
//! This does not depend on `/dev/usbtmc*` or any OS-specific device path.
//! A resource is addressed as `usb:<vid>:<pid>[:<serial>]`, with hex VID/PID.

use std::collections::VecDeque;
use std::time::Duration;

use nusb::descriptors::TransferType;
use nusb::transfer::{Buffer, Bulk, Direction, In, Out};
use nusb::{Device, DeviceInfo, Interface, MaybeFuture};
use serde::Serialize;

use super::Transport;
use crate::error::{Error, Result};

const USBTMC_CLASS: u8 = 0xFE;
const USBTMC_SUBCLASS: u8 = 0x03;
const USBTMC_PROTOCOL: u8 = 0x01;
const MSG_DEV_DEP_MSG_OUT: u8 = 1;
const MSG_DEV_DEP_MSG_IN: u8 = 2;
const EOM: u8 = 0x01;
const REQUEST_SIZE: usize = 4096;
const MAX_MESSAGE: usize = 16 * 1024 * 1024;

/// One USBTMC-capable USB device found by descriptor enumeration.
#[derive(Debug, Serialize)]
pub struct UsbTmcDevice {
    pub resource: String,
    pub vendor_id: u16,
    pub product_id: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
}

/// Enumerate USBTMC devices on Windows, macOS, Linux, and other nusb targets.
///
/// This reads USB descriptors only. It does not open the device, bind a driver,
/// or query `*IDN?`, so it is safe to call while another program owns the
/// instrument.
pub fn discover() -> Result<Vec<UsbTmcDevice>> {
    let devices = nusb::list_devices()
        .wait()
        .map_err(|error| Error::Transport(error.to_string()))?;
    let mut found = Vec::new();
    for info in devices {
        if !is_usbtmc(&info) {
            continue;
        }
        found.push(UsbTmcDevice {
            resource: resource(&info),
            vendor_id: info.vendor_id(),
            product_id: info.product_id(),
            serial_number: info.serial_number().map(str::to_string),
            manufacturer: info.manufacturer_string().map(str::to_string),
            product: info.product_string().map(str::to_string),
        });
    }
    found.sort_by(|a, b| a.resource.cmp(&b.resource));
    Ok(found)
}

fn is_usbtmc(info: &DeviceInfo) -> bool {
    (info.class() == USBTMC_CLASS
        && info.subclass() == USBTMC_SUBCLASS
        && info.protocol() == USBTMC_PROTOCOL)
        || info.interfaces().any(|interface| {
            interface.class() == USBTMC_CLASS
                && interface.subclass() == USBTMC_SUBCLASS
                && interface.protocol() == USBTMC_PROTOCOL
        })
}

fn resource(info: &DeviceInfo) -> String {
    let base = format!("usb:{:04x}:{:04x}", info.vendor_id(), info.product_id());
    match info.serial_number().filter(|serial| !serial.is_empty()) {
        Some(serial) => format!("{base}:{serial}"),
        None => base,
    }
}

/// Open a USBTMC device by `usb:<vid>:<pid>[:<serial>]`.
pub fn open(spec: &str, timeout: Duration) -> Result<Box<dyn Transport>> {
    let spec = spec.strip_prefix("usb:").unwrap_or(spec);
    let mut parts = spec.splitn(3, ':');
    let vendor_id = parse_hex(parts.next(), "vendor id")?;
    let product_id = parse_hex(parts.next(), "product id")?;
    let serial = parts.next().filter(|value| !value.is_empty());

    let devices = nusb::list_devices()
        .wait()
        .map_err(|error| Error::Transport(error.to_string()))?;
    let info = devices
        .into_iter()
        .find(|info| {
            info.vendor_id() == vendor_id
                && info.product_id() == product_id
                && serial.is_none_or(|serial| info.serial_number() == Some(serial))
        })
        .ok_or_else(|| Error::Transport(format!("USBTMC device not found: usb:{spec}")))?;

    if !is_usbtmc(&info) {
        return Err(Error::Transport(format!(
            "usb:{spec} does not expose a USBTMC interface"
        )));
    }

    let device = info
        .open()
        .wait()
        .map_err(|error| Error::Transport(error.to_string()))?;
    let configuration = device
        .active_configuration()
        .map_err(|error| Error::Transport(error.to_string()))?;

    let mut endpoint_pair = None;
    for interface in configuration.interfaces() {
        let descriptor = interface.first_alt_setting();
        if descriptor.class() != USBTMC_CLASS
            || descriptor.subclass() != USBTMC_SUBCLASS
            || descriptor.protocol() != USBTMC_PROTOCOL
        {
            continue;
        }
        let mut out = None;
        let mut input = None;
        for endpoint in descriptor.endpoints() {
            if endpoint.transfer_type() != TransferType::Bulk {
                continue;
            }
            match endpoint.direction() {
                Direction::Out => out = Some(endpoint.address()),
                Direction::In => input = Some(endpoint.address()),
            }
        }
        if let (Some(out), Some(input)) = (out, input) {
            endpoint_pair = Some((interface.interface_number(), out, input));
            break;
        }
    }
    let (interface_number, out_address, in_address) = endpoint_pair
        .ok_or_else(|| Error::Transport(format!("usb:{spec} has no bulk USBTMC endpoints")))?;

    let interface = device
        .detach_and_claim_interface(interface_number)
        .wait()
        .map_err(|error| Error::Transport(error.to_string()))?;
    let endpoint_out = interface
        .endpoint::<Bulk, Out>(out_address)
        .map_err(|error| Error::Transport(error.to_string()))?;
    let endpoint_in = interface
        .endpoint::<Bulk, In>(in_address)
        .map_err(|error| Error::Transport(error.to_string()))?;

    let mut transport = UsbTmcTransport {
        _device: device,
        _interface: interface,
        endpoint_out,
        endpoint_in,
        timeout,
        tag: 0,
        read_buffer: VecDeque::new(),
        description: format!("usb:{spec}"),
    };
    transport.drain_input();
    Ok(Box::new(transport))
}

fn parse_hex(value: Option<&str>, field: &str) -> Result<u16> {
    let value = value.unwrap_or_default();
    u16::from_str_radix(value, 16)
        .map_err(|_| Error::Transport(format!("invalid {field} in usb resource: {value:?}")))
}

struct UsbTmcTransport {
    _device: Device,
    _interface: Interface,
    endpoint_out: nusb::Endpoint<Bulk, Out>,
    endpoint_in: nusb::Endpoint<Bulk, In>,
    timeout: Duration,
    tag: u8,
    read_buffer: VecDeque<u8>,
    description: String,
}

impl UsbTmcTransport {
    fn next_tag(&mut self) -> u8 {
        self.tag = self.tag.wrapping_add(1);
        if self.tag == 0 {
            self.tag = 1;
        }
        self.tag
    }

    fn request_in(&mut self, tag: u8, size: usize) -> Result<()> {
        let mut message = Vec::with_capacity(12);
        message.push(MSG_DEV_DEP_MSG_IN);
        message.push(tag);
        message.push(!tag);
        message.push(0);
        message.extend_from_slice(&(size as u32).to_le_bytes());
        message.push(0);
        message.push(0);
        message.extend_from_slice(&[0, 0]);
        self.endpoint_out.submit(message.into());
        let completion = self
            .endpoint_out
            .wait_next_complete(self.timeout)
            .ok_or(Error::Timeout(self.timeout))?;
        completion.status.map_err(transfer_error)?;
        Ok(())
    }

    fn cancel_input(&mut self) {
        self.endpoint_in.cancel_all();
        while self.endpoint_in.pending() > 0 {
            let _ = self
                .endpoint_in
                .wait_next_complete(Duration::from_millis(10));
        }
    }

    fn drain_input(&mut self) {
        let packet = self.endpoint_in.max_packet_size().max(1);
        for _ in 0..4 {
            self.endpoint_in.submit(Buffer::new(packet));
            match self
                .endpoint_in
                .wait_next_complete(Duration::from_millis(20))
            {
                Some(completion) if completion.status.is_ok() && completion.actual_len > 0 => {}
                _ => break,
            }
        }
        self.cancel_input();
    }

    fn fill(&mut self, timeout: Duration) -> Result<()> {
        if !self.read_buffer.is_empty() {
            return Ok(());
        }

        let packet = self.endpoint_in.max_packet_size().max(1);
        let initial_len = (REQUEST_SIZE + 12).div_ceil(packet) * packet + packet;
        let tag = self.next_tag();
        self.request_in(tag, REQUEST_SIZE)?;
        self.endpoint_in.submit(Buffer::new(initial_len));
        let completion = self
            .endpoint_in
            .wait_next_complete(timeout)
            .ok_or(Error::Timeout(timeout))?;
        completion.status.map_err(transfer_error)?;
        let actual = completion.actual_len;
        let mut data = completion.buffer.into_vec();
        data.truncate(actual);
        if data.len() < 12 {
            return Err(Error::Transport(format!(
                "short USBTMC bulk-in header: {} bytes",
                data.len()
            )));
        }
        if data[0] != MSG_DEV_DEP_MSG_IN {
            return Err(Error::Transport(format!(
                "unexpected USBTMC message id {}",
                data[0]
            )));
        }
        let transfer_size = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
        if transfer_size > MAX_MESSAGE {
            return Err(Error::Transport(format!(
                "USBTMC response too large: {transfer_size} bytes"
            )));
        }
        let attributes = data[8];
        let mut payload = data.split_off(12);
        if payload.len() > transfer_size {
            payload.truncate(transfer_size);
        }

        while payload.len() < transfer_size {
            let remaining = transfer_size - payload.len();
            let chunk = remaining.min(1024 * 1024);
            let request_len = chunk.div_ceil(packet) * packet + packet;
            self.endpoint_in.submit(Buffer::new(request_len));
            let completion = self
                .endpoint_in
                .wait_next_complete(timeout)
                .ok_or(Error::Timeout(timeout))?;
            completion.status.map_err(transfer_error)?;
            let actual = completion.actual_len;
            let mut chunk_data = completion.buffer.into_vec();
            chunk_data.truncate(actual);
            if chunk_data.is_empty() {
                break;
            }
            let take = remaining.min(chunk_data.len());
            payload.extend_from_slice(&chunk_data[..take]);
        }

        if attributes & EOM != 0 && payload.last() != Some(&b'\n') {
            payload.push(b'\n');
        }
        self.read_buffer.extend(payload);
        Ok(())
    }
}

impl Transport for UsbTmcTransport {
    fn description(&self) -> String {
        self.description.clone()
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        let tag = self.next_tag();
        let mut message = Vec::with_capacity(12 + bytes.len() + 3);
        message.push(MSG_DEV_DEP_MSG_OUT);
        message.push(tag);
        message.push(!tag);
        message.push(0);
        message.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        message.push(EOM);
        message.extend_from_slice(&[0, 0, 0]);
        message.extend_from_slice(bytes);
        while message.len() % 4 != 0 {
            message.push(0);
        }
        self.endpoint_out.submit(message.into());
        let completion = self
            .endpoint_out
            .wait_next_complete(self.timeout)
            .ok_or(Error::Timeout(self.timeout))?;
        completion.status.map_err(transfer_error)?;
        Ok(())
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        if self.read_buffer.is_empty() {
            self.fill(self.timeout)?;
        }
        let count = buffer.len().min(self.read_buffer.len());
        for slot in buffer.iter_mut().take(count) {
            *slot = self.read_buffer.pop_front().unwrap_or(0);
        }
        Ok(count)
    }

    fn read_available(&mut self, buffer: &mut [u8]) -> Result<usize> {
        if self.read_buffer.is_empty() {
            match self.fill(Duration::from_millis(1)) {
                Ok(()) => {}
                Err(Error::Timeout(_)) => {
                    self.cancel_input();
                    return Ok(0);
                }
                Err(error) => return Err(error),
            }
        }
        let count = buffer.len().min(self.read_buffer.len());
        for slot in buffer.iter_mut().take(count) {
            *slot = self.read_buffer.pop_front().unwrap_or(0);
        }
        Ok(count)
    }

    fn clear(&mut self) -> Result<()> {
        self.cancel_input();
        self.endpoint_out.cancel_all();
        while self.endpoint_out.pending() > 0 {
            let _ = self
                .endpoint_out
                .wait_next_complete(Duration::from_millis(10));
        }
        self.read_buffer.clear();
        Ok(())
    }
}

fn transfer_error(error: nusb::transfer::TransferError) -> Error {
    Error::Transport(error.to_string())
}

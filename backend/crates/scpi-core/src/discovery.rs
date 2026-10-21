// SPDX-License-Identifier: AGPL-3.0-or-later

//! Cross-transport device discovery.
//!
//! Never fails because one transport is unavailable; problems are reported
//! through [`Discovery::warnings`].

use std::time::Duration;

use serde::Serialize;

use crate::error::Result;
use crate::transport::lan::{self, LanSource, Subnet};
use crate::transport::usb;

#[derive(Debug, Clone)]
pub struct DiscoverOptions {
    pub usb: bool,
    pub lan: bool,
    pub mdns: bool,
    pub sweep: bool,
    /// Empty means the local interfaces are used instead.
    pub subnets: Vec<String>,
    pub hosts: Vec<String>,
    /// Empty means [`lan::DEFAULT_PORTS`].
    pub ports: Vec<u16>,
    pub probe_idn: bool,
    pub connect_timeout: Duration,
    pub mdns_timeout: Duration,
    pub idn_timeout: Duration,
}

impl Default for DiscoverOptions {
    fn default() -> Self {
        Self {
            usb: true,
            lan: false,
            mdns: true,
            sweep: true,
            subnets: Vec::new(),
            hosts: Vec::new(),
            ports: Vec::new(),
            probe_idn: true,
            connect_timeout: Duration::from_millis(250),
            mdns_timeout: Duration::from_millis(1200),
            idn_timeout: Duration::from_millis(700),
        }
    }
}

/// USB devices carry `vendor_id`/`product_id`, LAN devices carry
/// `host`/`port`; both may carry `*IDN?` fields when probing was enabled.
#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub resource: String,
    pub transport: &'static str,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_id: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firmware: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Discovery {
    pub devices: Vec<Device>,
    /// Non-fatal: devices are still returned.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

pub fn discover(options: &DiscoverOptions) -> Result<Discovery> {
    let mut result = Discovery::default();
    if options.usb {
        match usb::discover() {
            Ok(devices) => result.devices.extend(devices.into_iter().map(from_usb)),
            Err(error) => result
                .warnings
                .push(format!("USB enumeration failed: {error}")),
        }
    }
    if options.lan {
        discover_lan(options, &mut result);
    }
    result.devices.sort_by(|left, right| {
        (left.transport, &left.resource).cmp(&(right.transport, &right.resource))
    });
    Ok(result)
}

fn discover_lan(options: &DiscoverOptions, result: &mut Discovery) {
    let ports = if options.ports.is_empty() {
        lan::DEFAULT_PORTS.to_vec()
    } else {
        options.ports.clone()
    };

    let mut candidates = lan::parse_hosts(&options.hosts, ports[0]);

    if options.mdns {
        candidates.extend(lan::mdns_browse(options.mdns_timeout));
    }
    if options.sweep {
        let mut subnets = Vec::new();
        for text in &options.subnets {
            match Subnet::parse(text) {
                Ok(subnet) => subnets.push(subnet.clamped(subnet.network)),
                Err(error) => result.warnings.push(error),
            }
        }
        if !subnets.is_empty() {
            candidates.extend(lan::sweep(&subnets, &ports, options.connect_timeout));
        }
        if options.subnets.is_empty() {
            let subnets = lan::default_subnets();
            if subnets.is_empty() {
                result
                    .warnings
                    .push("no non-loopback IPv4 interface found; LAN sweep skipped".to_string());
            } else {
                candidates.extend(lan::sweep(&subnets, &ports, options.connect_timeout));
            }
        }
    }

    for candidate in lan::dedupe(candidates) {
        let manual = candidate.source == LanSource::Manual;
        if options.probe_idn {
            // A reachable socket that does not answer *IDN? is not something
            // this plugin can drive, so it is not reported.
            match lan::probe(candidate.address, options.idn_timeout) {
                lan::Probe::Identity(idn) => result.devices.push(from_lan(candidate, Some(idn))),
                lan::Probe::Silent if manual => result.warnings.push(format!(
                    "{} accepted a connection but did not answer *IDN?",
                    candidate.address
                )),
                lan::Probe::Unreachable if manual => result
                    .warnings
                    .push(format!("{} did not accept a connection", candidate.address)),
                _ => {}
            }
            continue;
        }
        // Without IDN probing the socket still has to prove it exists.
        if lan::is_reachable(candidate.address, options.connect_timeout) {
            result.devices.push(from_lan(candidate, None));
        } else if manual {
            result
                .warnings
                .push(format!("{} did not accept a connection", candidate.address));
        }
    }
}

fn from_usb(device: usb::UsbTmcDevice) -> Device {
    Device {
        resource: device.resource,
        transport: "usb",
        source: "usb".to_string(),
        vendor_id: Some(device.vendor_id),
        product_id: Some(device.product_id),
        host: None,
        port: None,
        serial_number: device.serial_number,
        manufacturer: device.manufacturer,
        product: device.product.clone(),
        model: device.product,
        firmware: None,
        idn: None,
        service: None,
        instance: None,
    }
}

fn from_lan(candidate: lan::LanCandidate, idn: Option<lan::Idn>) -> Device {
    let host = match candidate.address.ip() {
        std::net::IpAddr::V4(ip) => ip.to_string(),
        std::net::IpAddr::V6(ip) => ip.to_string(),
    };
    Device {
        resource: resource_for(candidate.address),
        transport: "lan",
        source: match candidate.source {
            LanSource::Mdns => "mdns",
            LanSource::Sweep => "sweep",
            LanSource::Manual => "manual",
        }
        .to_string(),
        vendor_id: None,
        product_id: None,
        host: Some(host),
        port: Some(candidate.address.port()),
        serial_number: idn.as_ref().and_then(|idn| idn.serial_number.clone()),
        manufacturer: idn.as_ref().and_then(|idn| idn.manufacturer.clone()),
        product: idn.as_ref().and_then(|idn| idn.model.clone()),
        model: idn.as_ref().and_then(|idn| idn.model.clone()),
        firmware: idn.as_ref().and_then(|idn| idn.firmware.clone()),
        idn: idn.map(|idn| idn.raw),
        service: candidate.service,
        instance: candidate.instance,
    }
}

/// Build the transport spec for a socket address.
///
/// `SocketAddr`'s `Display` already brackets IPv6 literals, so the same
/// format works for both families.
pub fn resource_for(address: std::net::SocketAddr) -> String {
    format!("tcp://{address}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_specs_bracket_ipv6() {
        let v4: std::net::SocketAddr = "192.168.1.5:5025".parse().unwrap();
        assert_eq!(resource_for(v4), "tcp://192.168.1.5:5025");
        let v6: std::net::SocketAddr = "[fd00::5]:5025".parse().unwrap();
        assert_eq!(resource_for(v6), "tcp://[fd00::5]:5025");
    }

    #[test]
    fn usb_devices_map_to_the_unified_shape() {
        let device = from_usb(usb::UsbTmcDevice {
            resource: "usb:1ab1:09c4:SN1".to_string(),
            vendor_id: 0x1ab1,
            product_id: 0x09c4,
            serial_number: Some("SN1".to_string()),
            manufacturer: Some("Rigol".to_string()),
            product: Some("DM3058".to_string()),
        });
        assert_eq!(device.transport, "usb");
        assert_eq!(device.resource, "usb:1ab1:09c4:SN1");
        assert_eq!(device.vendor_id, Some(0x1ab1));
        assert_eq!(device.model.as_deref(), Some("DM3058"));
        assert!(device.host.is_none());
    }

    #[test]
    fn lan_devices_use_idn_fields() {
        let candidate = lan::LanCandidate {
            address: "192.168.1.40:5025".parse().unwrap(),
            source: LanSource::Mdns,
            service: Some("_scpi-raw._tcp.local".to_string()),
            instance: Some("dm3058".to_string()),
        };
        let idn = lan::parse_idn("Rigol Technologies,DM3058,SN9,1.2\n");
        let device = from_lan(candidate, Some(idn));
        assert_eq!(device.resource, "tcp://192.168.1.40:5025");
        assert_eq!(device.source, "mdns");
        assert_eq!(device.model.as_deref(), Some("DM3058"));
        assert_eq!(device.serial_number.as_deref(), Some("SN9"));
        assert_eq!(
            device.idn.as_deref(),
            Some("Rigol Technologies,DM3058,SN9,1.2")
        );
    }
}

//! LAN discovery for SCPI instruments.
//!
//! Best-effort by design: an unreachable subnet or a silent network yields
//! fewer candidates rather than an error.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use simple_dns::rdata::RData;
use simple_dns::{Name, Packet, Question, CLASS, TYPE};

/// 5025 is LXI/SCPI-RAW; 5555 is what legacy Rigol and Siglent models use.
pub const DEFAULT_PORTS: [u16; 2] = [5025, 5555];

const SERVICE_TYPES: [&str; 4] = [
    "_scpi-raw._tcp.local",
    "_scpi._tcp.local",
    "_vxi-11._tcp.local",
    "_lxi._tcp.local",
];

const MDNS_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const MDNS_PORT: u16 = 5353;
const MDNS_TTL: u32 = 255;
const MAX_SWEEP_WORKERS: usize = 64;
const MAX_SWEEP_HOSTS: u64 = 1024;
const IDN_QUERY: &[u8] = b"*IDN?\n";
const IDN_LIMIT: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LanSource {
    Mdns,
    Sweep,
    Manual,
}

#[derive(Debug, Clone)]
pub struct LanCandidate {
    pub address: SocketAddr,
    pub source: LanSource,
    pub service: Option<String>,
    pub instance: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Idn {
    pub raw: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firmware: Option<String>,
}

/// Instruments disagree about quoting and omit trailing fields, so a short
/// or quoted response is normal rather than an error.
pub fn parse_idn(raw: &str) -> Idn {
    let cleaned = raw
        .trim_matches(|c: char| c == '\u{0}' || c.is_control())
        .trim()
        .trim_matches('"')
        .trim();
    let parts: Vec<&str> = cleaned
        .split(',')
        .map(|part| part.trim().trim_matches('"').trim())
        .collect();
    let field = |index: usize| {
        parts
            .get(index)
            .filter(|value| !value.is_empty())
            .map(|value| (*value).to_string())
    };
    Idn {
        raw: cleaned.to_string(),
        manufacturer: field(0),
        model: field(1),
        serial_number: field(2),
        firmware: field(3),
    }
}

#[derive(Debug, Clone)]
pub enum Probe {
    Identity(Idn),
    /// Listening, but it did not return a usable identity.
    Silent,
    Unreachable,
}

/// The connection is closed as soon as the reply is read, so probing leaves
/// no session behind on the instrument.
pub fn probe(address: SocketAddr, timeout: Duration) -> Probe {
    let Ok(mut stream) = TcpStream::connect_timeout(&address, timeout) else {
        return Probe::Unreachable;
    };
    if stream.set_read_timeout(Some(timeout)).is_err()
        || stream.set_write_timeout(Some(timeout)).is_err()
        || stream.set_nodelay(true).is_err()
        || stream.write_all(IDN_QUERY).is_err()
    {
        return Probe::Silent;
    }

    let deadline = Instant::now() + timeout;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 512];
    while buffer.len() < IDN_LIMIT && Instant::now() < deadline {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => {
                buffer.extend_from_slice(&chunk[..count]);
                if buffer.contains(&b'\n') {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&buffer);
    match text.lines().next().map(str::trim) {
        Some(line) if !line.is_empty() => Probe::Identity(parse_idn(line)),
        _ => Probe::Silent,
    }
}

pub fn probe_idn(address: SocketAddr, timeout: Duration) -> Option<Idn> {
    match probe(address, timeout) {
        Probe::Identity(idn) => Some(idn),
        _ => None,
    }
}

pub fn is_reachable(address: SocketAddr, timeout: Duration) -> bool {
    TcpStream::connect_timeout(&address, timeout).is_ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subnet {
    pub network: Ipv4Addr,
    pub prefix: u8,
}

impl Subnet {
    /// Build a subnet, masking off host bits. Returns `None` for an invalid
    /// prefix.
    pub fn new(address: Ipv4Addr, prefix: u8) -> Option<Self> {
        if prefix > 32 {
            return None;
        }
        let network = Ipv4Addr::from(u32::from(address) & mask(prefix));
        Some(Self { network, prefix })
    }

    /// Parse `a.b.c.d/nn` or a bare `a.b.c.d` (treated as `/24`).
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("empty subnet".to_string());
        }
        let (address, prefix) = match text.split_once('/') {
            Some((address, prefix)) => (
                address,
                prefix
                    .trim()
                    .parse::<u8>()
                    .map_err(|_| format!("invalid prefix in subnet '{text}'"))?,
            ),
            None => (text, 24),
        };
        let address = address
            .trim()
            .parse::<Ipv4Addr>()
            .map_err(|_| format!("invalid IPv4 address in subnet '{text}'"))?;
        Self::new(address, prefix).ok_or_else(|| format!("invalid subnet prefix in '{text}'"))
    }

    pub fn len(&self) -> u64 {
        1u64 << (32 - u32::from(self.prefix))
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn contains(&self, address: Ipv4Addr) -> bool {
        u32::from(address) & mask(self.prefix) == u32::from(self.network)
    }

    /// The network and broadcast addresses are excluded. Nothing is returned
    /// for subnets wider than [`MAX_SWEEP_HOSTS`], so callers must
    /// [`Subnet::clamped`] first; that keeps a stray `/8` from allocating
    /// gigabytes.
    pub fn hosts(&self) -> Vec<Ipv4Addr> {
        if self.len() > MAX_SWEEP_HOSTS {
            return Vec::new();
        }
        let base = u32::from(self.network);
        let len = self.len();
        let (start, end) = if self.prefix >= 31 {
            (base, base + len as u32)
        } else {
            (base + 1, base + len as u32 - 1)
        };
        (start..end).map(Ipv4Addr::from).collect()
    }

    /// Anything wider than [`MAX_SWEEP_HOSTS`] becomes the `/24` around
    /// `address`.
    pub fn clamped(&self, address: Ipv4Addr) -> Self {
        if self.len() <= MAX_SWEEP_HOSTS {
            return *self;
        }
        Self::new(address, 24).unwrap_or(*self)
    }
}

fn mask(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(prefix))
    }
}

/// Non-loopback IPv4 interfaces as `(name, address, prefix)`.
pub fn local_interfaces() -> Vec<(String, Ipv4Addr, u8)> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut found: Vec<(String, Ipv4Addr, u8)> = interfaces
        .into_iter()
        .filter(|interface| interface.oper_status != if_addrs::IfOperStatus::Down)
        .filter_map(|interface| match interface.addr {
            if_addrs::IfAddr::V4(v4) => Some((interface.name, v4.ip, v4.prefixlen)),
            if_addrs::IfAddr::V6(_) => None,
        })
        .filter(|(_, ip, _)| !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified())
        .collect();
    found.sort_by(|left, right| left.0.cmp(&right.0));
    found
}

pub fn default_subnets() -> Vec<Subnet> {
    let mut subnets: Vec<Subnet> = local_interfaces()
        .into_iter()
        .filter_map(|(_, ip, prefix)| Subnet::new(ip, prefix).map(|subnet| subnet.clamped(ip)))
        .collect();
    subnets.sort_by_key(|subnet| u32::from(subnet.network));
    subnets.dedup();
    subnets
}

/// A successful handshake only means something is listening; whether it
/// speaks SCPI is decided later by [`probe`].
pub fn sweep(subnets: &[Subnet], ports: &[u16], connect_timeout: Duration) -> Vec<LanCandidate> {
    let mut targets: Vec<SocketAddr> = Vec::new();
    for subnet in subnets {
        for host in subnet.hosts() {
            for port in ports {
                targets.push(SocketAddr::new(IpAddr::V4(host), *port));
            }
        }
    }
    if targets.is_empty() {
        return Vec::new();
    }

    let next = AtomicUsize::new(0);
    let found = Mutex::new(Vec::new());
    let workers = MAX_SWEEP_WORKERS.min(targets.len());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(address) = targets.get(index) else {
                    break;
                };
                if TcpStream::connect_timeout(address, connect_timeout).is_ok() {
                    let mut seen = found.lock().unwrap_or_else(|error| error.into_inner());
                    seen.push(LanCandidate {
                        address: *address,
                        source: LanSource::Sweep,
                        service: None,
                        instance: None,
                    });
                }
            });
        }
    });

    let mut candidates = found
        .into_inner()
        .unwrap_or_else(|error| error.into_inner());
    candidates.sort_by_key(|candidate| candidate.address);
    candidates
}

pub fn parse_hosts(hosts: &[String], default_port: u16) -> Vec<LanCandidate> {
    let mut candidates = Vec::new();
    for host in hosts {
        let host = host.trim();
        if host.is_empty() {
            continue;
        }
        if let Ok(address) = host.parse::<SocketAddr>() {
            candidates.push(manual(address));
            continue;
        }
        if let Ok(ip) = host.parse::<IpAddr>() {
            candidates.push(manual(SocketAddr::new(ip, default_port)));
            continue;
        }
        if let Ok(addresses) = std::net::ToSocketAddrs::to_socket_addrs(&(host, default_port)) {
            for address in addresses.take(1) {
                candidates.push(manual(address));
            }
        }
    }
    candidates
}

fn manual(address: SocketAddr) -> LanCandidate {
    LanCandidate {
        address,
        source: LanSource::Manual,
        service: None,
        instance: None,
    }
}

/// Queries carry the unicast-response bit, so replies arrive on the querying
/// socket even when another mDNS responder owns port 5353.
pub fn mdns_browse(timeout: Duration) -> Vec<LanCandidate> {
    let interfaces = local_interfaces();
    let binds: Vec<Ipv4Addr> = if interfaces.is_empty() {
        vec![Ipv4Addr::UNSPECIFIED]
    } else {
        interfaces.into_iter().map(|(_, ip, _)| ip).collect()
    };

    let mut sockets = Vec::new();
    let query = match build_query() {
        Ok(bytes) => bytes,
        Err(_) => return Vec::new(),
    };
    for bind in &binds {
        let Ok(socket) = UdpSocket::bind(SocketAddr::new(IpAddr::V4(*bind), 0)) else {
            continue;
        };
        let _ = socket.set_multicast_ttl_v4(MDNS_TTL);
        let _ = socket.set_read_timeout(Some(Duration::from_millis(150)));
        // A socket that cannot multicast on this interface is still useful
        // for unicast replies from manual candidates.
        let _ = socket.send_to(&query, SocketAddr::new(IpAddr::V4(MDNS_GROUP), MDNS_PORT));
        sockets.push(socket);
    }
    if sockets.is_empty() {
        return Vec::new();
    }

    let deadline = Instant::now() + timeout;
    let mut records = RecordSet::default();
    receive(&sockets, &mut records, deadline);
    resolve_missing(&sockets, &mut records, Duration::from_millis(400));
    records.candidates()
}

fn build_query() -> Result<Vec<u8>, simple_dns::SimpleDnsError> {
    let mut packet = Packet::new_query(0);
    for service in SERVICE_TYPES {
        let name = Name::new(service)?;
        packet.questions.push(Question::new(
            name,
            TYPE::PTR.into(),
            CLASS::IN.into(),
            true,
        ));
    }
    packet.build_bytes_vec_compressed()
}

fn receive(sockets: &[UdpSocket], records: &mut RecordSet, deadline: Instant) {
    let mut buffer = [0u8; 9000];
    while Instant::now() < deadline {
        let mut received = false;
        for socket in sockets {
            while let Ok((count, _)) = socket.recv_from(&mut buffer) {
                received = true;
                if let Ok(packet) = Packet::parse(&buffer[..count]) {
                    records.absorb(&packet);
                }
            }
        }
        if !received {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn resolve_missing(sockets: &[UdpSocket], records: &mut RecordSet, budget: Duration) {
    let mut wanted: Vec<(String, TYPE)> = Vec::new();
    for instance in records.instances() {
        match records.srv.get(&instance) {
            Some((_, target)) if records.has_address(target) => {}
            Some((_, target)) => wanted.push((target.clone(), TYPE::A)),
            None => wanted.push((instance, TYPE::SRV)),
        }
    }
    if wanted.is_empty() {
        return;
    }

    let mut packet = Packet::new_query(0);
    for (name, query_type) in wanted {
        let name = match Name::new(&name) {
            Ok(name) => name.into_owned(),
            Err(_) => continue,
        };
        packet.questions.push(Question::new(
            name,
            query_type.into(),
            CLASS::IN.into(),
            true,
        ));
    }
    let Ok(bytes) = packet.build_bytes_vec_compressed() else {
        return;
    };
    for socket in sockets {
        let _ = socket.send_to(&bytes, SocketAddr::new(IpAddr::V4(MDNS_GROUP), MDNS_PORT));
    }
    receive(sockets, records, Instant::now() + budget);
}

#[derive(Debug, Default)]
struct RecordSet {
    /// Service type -> instance name.
    ptr: Vec<(String, String)>,
    /// Instance name -> (port, target host).
    srv: HashMap<String, (u16, String)>,
    /// Host name -> addresses.
    addresses: HashMap<String, Vec<IpAddr>>,
}

impl RecordSet {
    fn absorb(&mut self, packet: &Packet) {
        for record in packet
            .answers
            .iter()
            .chain(packet.additional_records.iter())
        {
            match &record.rdata {
                RData::PTR(name) => {
                    let service = normalize(&record.name);
                    let instance = normalize(&name.0);
                    if !self.ptr.contains(&(service.clone(), instance.clone())) {
                        self.ptr.push((service, instance));
                    }
                }
                RData::SRV(srv) => {
                    let instance = normalize(&record.name);
                    self.srv
                        .insert(instance, (srv.port, normalize(&srv.target)));
                }
                RData::A(a) => {
                    let host = normalize(&record.name);
                    let address = IpAddr::V4(Ipv4Addr::from(a.address));
                    self.push_address(host, address);
                }
                RData::AAAA(aaaa) => {
                    let host = normalize(&record.name);
                    let address = IpAddr::V6(Ipv6Addr::from(aaaa.address));
                    if !is_unusable_v6(&address) {
                        self.push_address(host, address);
                    }
                }
                _ => {}
            }
        }
    }

    fn push_address(&mut self, host: String, address: IpAddr) {
        let entry = self.addresses.entry(host).or_default();
        if !entry.contains(&address) {
            entry.push(address);
        }
    }

    fn instances(&self) -> Vec<String> {
        let mut instances: Vec<String> = self
            .ptr
            .iter()
            .map(|(_, instance)| instance.clone())
            .collect();
        instances.sort();
        instances.dedup();
        instances
    }

    fn has_address(&self, host: &str) -> bool {
        self.addresses
            .get(host)
            .is_some_and(|addresses| !addresses.is_empty())
    }

    fn candidates(&self) -> Vec<LanCandidate> {
        let mut found = Vec::new();
        for (service, instance) in &self.ptr {
            let Some((port, target)) = self.srv.get(instance) else {
                continue;
            };
            let Some(addresses) = self.addresses.get(target) else {
                continue;
            };
            for address in addresses {
                found.push(LanCandidate {
                    address: SocketAddr::new(*address, *port),
                    source: LanSource::Mdns,
                    service: Some(service.clone()),
                    instance: Some(instance.clone()),
                });
            }
        }
        found.sort_by_key(|candidate| (candidate.address, candidate.instance.clone()));
        found.dedup_by_key(|candidate| (candidate.address, candidate.instance.clone()));
        found
    }
}

fn normalize(name: &Name) -> String {
    name.to_string().trim_end_matches('.').to_ascii_lowercase()
}

fn is_unusable_v6(address: &IpAddr) -> bool {
    match address {
        IpAddr::V6(v6) => v6.is_unicast_link_local() || v6.is_loopback() || v6.is_unspecified(),
        IpAddr::V4(_) => false,
    }
}

/// Keeps one candidate per address; a manual `--host` beats an mDNS
/// advertisement, which beats a bare handshake.
pub fn dedupe(candidates: Vec<LanCandidate>) -> Vec<LanCandidate> {
    let mut best: HashMap<SocketAddr, LanCandidate> = HashMap::new();
    let rank = |source: LanSource| match source {
        LanSource::Manual => 0,
        LanSource::Mdns => 1,
        LanSource::Sweep => 2,
    };
    for candidate in candidates {
        match best.get(&candidate.address) {
            Some(existing) if rank(existing.source) <= rank(candidate.source) => {}
            _ => {
                best.insert(candidate.address, candidate);
            }
        }
    }
    let mut merged: Vec<LanCandidate> = best.into_values().collect();
    merged.sort_by_key(|candidate| candidate.address);
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idn_parsing_handles_all_fields() {
        let idn = parse_idn("RIGOL TECHNOLOGIES,DM3058E,DM3R251000551,00.01.00\n");
        assert_eq!(idn.manufacturer.as_deref(), Some("RIGOL TECHNOLOGIES"));
        assert_eq!(idn.model.as_deref(), Some("DM3058E"));
        assert_eq!(idn.serial_number.as_deref(), Some("DM3R251000551"));
        assert_eq!(idn.firmware.as_deref(), Some("00.01.00"));
    }

    #[test]
    fn idn_parsing_tolerates_short_and_quoted_responses() {
        let short = parse_idn("Keysight,34461A\r\n");
        assert_eq!(short.model.as_deref(), Some("34461A"));
        assert_eq!(short.serial_number, None);

        let quoted = parse_idn("\"Rigol\",\"DG1022Z\",\"\",\"2.0\"\n");
        assert_eq!(quoted.manufacturer.as_deref(), Some("Rigol"));
        assert_eq!(quoted.serial_number, None);
        assert_eq!(quoted.firmware.as_deref(), Some("2.0"));
    }

    #[test]
    fn subnet_parse_and_hosts() {
        let subnet = Subnet::parse("192.168.1.77/24").unwrap();
        assert_eq!(subnet.network, Ipv4Addr::new(192, 168, 1, 0));
        assert_eq!(subnet.hosts().len(), 254);
        assert!(subnet.contains(Ipv4Addr::new(192, 168, 1, 42)));
        assert!(!subnet.contains(Ipv4Addr::new(192, 168, 2, 42)));

        let bare = Subnet::parse("10.0.0.5").unwrap();
        assert_eq!(bare.prefix, 24);

        let single = Subnet::parse("127.0.0.1/32").unwrap();
        assert_eq!(single.hosts(), vec![Ipv4Addr::new(127, 0, 0, 1)]);

        assert!(Subnet::parse("not-an-ip").is_err());
        assert!(Subnet::parse("10.0.0.1/33").is_err());
    }

    #[test]
    fn wide_subnets_are_clamped() {
        let wide = Subnet::parse("10.0.0.0/8").unwrap();
        let clamped = wide.clamped(Ipv4Addr::new(10, 4, 7, 9));
        assert_eq!(clamped.prefix, 24);
        assert_eq!(clamped.network, Ipv4Addr::new(10, 4, 7, 0));
        assert!(clamped.hosts().len() <= MAX_SWEEP_HOSTS as usize);
        // A subnet this wide is never enumerated directly.
        assert!(wide.hosts().is_empty());
    }

    #[test]
    fn probe_against_local_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let mut writer = stream;
            let _ = writer.write_all(b"Rigol Technologies,DG1022Z,DG1ZA000000000,2.03\n");
        });
        let idn = probe_idn(address, Duration::from_secs(2)).expect("idn response");
        assert_eq!(idn.model.as_deref(), Some("DG1022Z"));
        server.join().unwrap();
    }

    #[test]
    fn probe_reports_unreachable_addresses() {
        // Port 1 on loopback is never listening in a test environment.
        let address: SocketAddr = "127.0.0.1:1".parse().unwrap();
        assert!(matches!(
            probe(address, Duration::from_millis(200)),
            Probe::Unreachable
        ));
    }

    #[test]
    fn sweep_finds_a_local_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            // The sweep opens exactly one connection for this single-host subnet.
            let _ = listener.accept();
        });
        let subnet = Subnet::parse("127.0.0.1/32").unwrap();
        let found = sweep(&[subnet], &[port], Duration::from_millis(300));
        assert!(found
            .iter()
            .any(|candidate| candidate.address.port() == port));
        server.join().unwrap();
    }

    #[test]
    fn hosts_parse_accepts_ports_and_bare_addresses() {
        let candidates = parse_hosts(
            &["192.168.1.10:5025".to_string(), "127.0.0.1".to_string()],
            5555,
        );
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].address.port(), 5025);
        assert_eq!(candidates[1].address.port(), 5555);
        assert!(candidates.iter().all(|c| c.source == LanSource::Manual));
    }

    #[test]
    fn dedupe_prefers_more_specific_sources() {
        let address: SocketAddr = "10.0.0.5:5025".parse().unwrap();
        let merged = dedupe(vec![
            LanCandidate {
                address,
                source: LanSource::Sweep,
                service: None,
                instance: None,
            },
            LanCandidate {
                address,
                source: LanSource::Mdns,
                service: Some("_scpi-raw._tcp.local".into()),
                instance: Some("inst".into()),
            },
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].source, LanSource::Mdns);
    }

    #[test]
    fn record_set_builds_candidates() {
        let mut records = RecordSet::default();
        records.ptr.push((
            "_scpi-raw._tcp.local".into(),
            "dm3058._scpi-raw._tcp.local".into(),
        ));
        records.srv.insert(
            "dm3058._scpi-raw._tcp.local".into(),
            (5025, "dm3058.local".into()),
        );
        records.push_address("dm3058.local".into(), "192.168.1.40".parse().unwrap());
        let candidates = records.candidates();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].address, "192.168.1.40:5025".parse().unwrap());
        assert_eq!(candidates[0].source, LanSource::Mdns);
    }
}

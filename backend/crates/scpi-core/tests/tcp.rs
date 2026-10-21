//! TCP transport round-trip against a loopback SCPI server.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use scpi_core::profile::Profile;
use scpi_core::{Instrument, Value};

const PROFILE: &str = r#"
{
  "schema_version": 1,
  "id": "test.tcp",
  "name": "TCP test",
  "idn_match": ["DM3058"],
  "commands": [
    { "id": "meas.vdc", "header": ":MEASure:VOLTage:DC", "readable": true, "result": "float" }
  ]
}
"#;

#[test]
fn tcp_transport_roundtrip() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).unwrap() == 0 {
                break;
            }
            let response = match line.trim() {
                "*IDN?" => "RIGOL TECHNOLOGIES,DM3058E,SN1,1.0\n",
                ":MEASure:VOLTage:DC?" => "+3.3E-01\n",
                _ => "0\n",
            };
            writer.write_all(response.as_bytes()).unwrap();
            writer.flush().unwrap();
        }
    });

    let profile = Arc::new(Profile::from_json_str(PROFILE).unwrap());
    let transport =
        scpi_core::transport::open(&format!("tcp://{address}"), Duration::from_secs(2)).unwrap();
    let mut instrument = Instrument::open(profile, transport).unwrap();

    instrument.verify_identity().unwrap();
    assert_eq!(instrument.get("meas.vdc").unwrap(), Value::Float(0.33));

    drop(instrument);
    server.join().unwrap();
}

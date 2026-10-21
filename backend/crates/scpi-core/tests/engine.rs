//! End-to-end tests for the profile-driven engine, using the mock transport.

use std::sync::Arc;

use scpi_core::profile::Profile;
use scpi_core::transport::mock::MockTransport;
use scpi_core::{Error, Instrument, Value};

const PROFILE: &str = r#"
{
  "schema_version": 1,
  "id": "test.dmm",
  "name": "Test DMM",
  "idn_match": ["DM3058"],
  "error_query": "sys.error",
  "commands": [
    { "id": "ieee.idn", "header": "*IDN", "readable": true, "result": "string" },
    { "id": "meas.vdc", "header": ":MEASure:VOLTage:DC", "readable": true, "result": { "type": "float", "unit": "V" } },
    {
      "id": "meas.vdc.range",
      "header": ":MEASure:VOLTage:DC",
      "query_header": ":MEASure:VOLTage:DC:RANGe",
      "writable": true,
      "readable": true,
      "args": [{ "name": "range", "type": "int", "min": 0, "max": 4, "special": ["MIN", "MAX", "DEF"] }],
      "result": "int"
    },
    { "id": "func.dc_voltage", "header": ":FUNCtion:VOLTage:DC", "writable": true },
    { "id": "func.current", "header": ":FUNCtion", "readable": true, "result": "string" },
    {
      "id": "rate.dc_voltage",
      "header": ":RATE:VOLTage:DC",
      "writable": true,
      "readable": true,
      "args": [{ "name": "rate", "type": "enum", "values": ["F", "M", "S"] }],
      "result": "enum"
    },
    {
      "id": "meas.vdc.filter",
      "header": ":MEASure:VOLTage:DC:FILTer:STATe",
      "writable": true,
      "readable": true,
      "args": [{ "name": "state", "type": "bool" }],
      "result": "bool"
    },
    {
      "id": "util.lan.ip",
      "header": ":UTILity:INTErface:LAN:IP",
      "writable": true,
      "args": [{ "name": "address", "type": "string", "quoted": true }]
    },
    { "id": "sys.error", "header": ":SYSTem:ERRor", "readable": true, "result": "string" }
  ]
}
"#;

fn profile() -> Arc<Profile> {
    Arc::new(Profile::from_json_str(PROFILE).expect("test profile parses"))
}

#[test]
fn typed_get_set_and_canonical_query() {
    let mock = MockTransport::empty()
        .with_response(":MEASure:VOLTage:DC?", "+1.5E-01")
        .with_response(":MEASure:VOLTage:DC:RANGe?", "2");
    let writes = mock.writes();
    let mut instrument = Instrument::open(profile(), Box::new(mock)).unwrap();

    assert_eq!(instrument.get("meas.vdc").unwrap(), Value::Float(0.15));
    instrument.set("meas.vdc.range", &[Value::Int(2)]).unwrap();

    let (value, raw) = instrument.query_typed(":meas:volt:dc?").unwrap();
    assert_eq!(value, Value::Float(0.15));
    assert_eq!(raw, "+1.5E-01");

    let writes = writes.lock().unwrap();
    assert!(writes.contains(&":MEASure:VOLTage:DC 2".to_string()));
    // Short forms are rewritten to the documented long form on the wire.
    assert!(writes.contains(&":MEASure:VOLTage:DC?".to_string()));
}

#[test]
fn range_limits_and_special_tokens() {
    let mock = MockTransport::empty();
    let writes = mock.writes();
    let mut instrument = Instrument::open(profile(), Box::new(mock)).unwrap();

    let error = instrument
        .set("meas.vdc.range", &[Value::Int(5)])
        .unwrap_err();
    assert!(matches!(error, Error::InvalidArg { .. }));

    instrument
        .set("meas.vdc.range", &[Value::Raw("MIN".into())])
        .unwrap();
    assert!(writes
        .lock()
        .unwrap()
        .contains(&":MEASure:VOLTage:DC MIN".to_string()));
}

#[test]
fn arguments_render_as_scpi() {
    let mock = MockTransport::empty();
    let writes = mock.writes();
    let mut instrument = Instrument::open(profile(), Box::new(mock)).unwrap();

    instrument
        .set("meas.vdc.filter", &[Value::Bool(true)])
        .unwrap();
    instrument
        .set("rate.dc_voltage", &[Value::String("f".into())])
        .unwrap();
    instrument
        .set("util.lan.ip", &[Value::String("192.168.1.42".into())])
        .unwrap();

    let writes = writes.lock().unwrap();
    assert!(writes.contains(&":MEASure:VOLTage:DC:FILTer:STATe ON".to_string()));
    assert!(writes.contains(&":RATE:VOLTage:DC F".to_string()));
    assert!(writes.contains(&":UTILity:INTErface:LAN:IP \"192.168.1.42\"".to_string()));
}

#[test]
fn access_flags_are_enforced() {
    let mut instrument = Instrument::open(profile(), Box::new(MockTransport::empty())).unwrap();

    assert!(matches!(
        instrument.get("func.dc_voltage"),
        Err(Error::NotReadable { .. })
    ));
    assert!(matches!(
        instrument.set("meas.vdc", &[]),
        Err(Error::NotWritable { .. })
    ));
    assert!(matches!(
        instrument.get("does.not.exist"),
        Err(Error::UnknownCommand(_))
    ));
}

#[test]
fn identity_and_error_queue() {
    let mock = MockTransport::empty()
        .with_response("*IDN?", "RIGOL TECHNOLOGIES,DM3058E,SN123,1.0.0")
        .with_response(":SYSTem:ERRor?", "-113,\"Undefined header\"")
        .with_response(":SYSTem:ERRor?", "+0,\"No error\"");
    let mut instrument = Instrument::open(profile(), Box::new(mock)).unwrap();

    instrument.verify_identity().unwrap();
    assert_eq!(
        instrument.drain_errors().unwrap(),
        vec!["-113,\"Undefined header\"".to_string()]
    );
}

#[test]
fn wrong_instrument_is_rejected() {
    let mock =
        MockTransport::empty().with_response("*IDN?", "KEYSIGHT TECHNOLOGIES,34461A,SN1,1.0");
    let mut instrument = Instrument::open(profile(), Box::new(mock)).unwrap();
    assert!(matches!(
        instrument.verify_identity(),
        Err(Error::UnexpectedInstrument { .. })
    ));
}

#[test]
fn profile_validation_catches_mistakes() {
    let duplicate = PROFILE.replace(
        r#"{ "id": "ieee.idn", "header": "*IDN", "readable": true, "result": "string" },"#,
        r#"{ "id": "ieee.idn", "header": "*IDN", "readable": true, "result": "string" },
           { "id": "ieee.idn", "header": "*ESR", "readable": true, "result": "int" },"#,
    );
    assert!(Profile::from_json_str(&duplicate).is_err());

    let bad_header = PROFILE.replace(":MEASure:VOLTage:DC", ":MEASure:VOLTage:DC 2");
    assert!(Profile::from_json_str(&bad_header).is_err());

    let no_access = PROFILE.replace(
        r#"{ "id": "func.dc_voltage", "header": ":FUNCtion:VOLTage:DC", "writable": true },"#,
        r#"{ "id": "func.dc_voltage", "header": ":FUNCtion:VOLTage:DC" },"#,
    );
    assert!(Profile::from_json_str(&no_access).is_err());
}

#[test]
fn cli_style_coercion() {
    let profile = profile();
    let range = profile.command("meas.vdc.range").unwrap();
    assert_eq!(
        range.coerce_args(&["MIN".to_string()]).unwrap()[0],
        Value::Raw("MIN".into())
    );
    assert_eq!(
        range.coerce_args(&["2".to_string()]).unwrap()[0],
        Value::Int(2)
    );
    // Range limits are enforced when the wire message is rendered.
    assert!(range.format_set(&[Value::Int(9)]).is_err());
}

#[cfg(feature = "lua")]
#[test]
fn lua_macros_query_and_parse() {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    let dir = std::env::temp_dir().join(format!(
        "ee-scpi-lua-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("macro.lua"),
        r#"
        local functions = { DCV = ":MEASure:VOLTage:DC?" }
        function active()
          return (string.gsub(scpi.query(":FUNCtion?"), "%s+$", ""))
        end
        function measure()
          local name = string.upper(active())
          return tonumber(scpi.query(functions[name]))
        end
        "#,
    )
    .unwrap();
    let json = PROFILE.replace(
        r#""error_query": "sys.error","#,
        r#""error_query": "sys.error", "lua": "macro.lua","#,
    );
    fs::write(dir.join("profile.json"), json).unwrap();

    let profile = Arc::new(Profile::load(&dir.join("profile.json")).unwrap());
    let mock = MockTransport::empty()
        .with_response(":FUNCtion?", "DCV")
        .with_response(":MEASure:VOLTage:DC?", "+2.5E-01");
    let mut instrument = Instrument::open(profile, Box::new(mock)).unwrap();

    assert_eq!(
        instrument.call("active", &[]).unwrap(),
        Value::String("DCV".into())
    );
    assert_eq!(instrument.call("measure", &[]).unwrap(), Value::Float(0.25));

    fs::remove_dir_all(&dir).ok();
}

const STRICT_PROFILE: &str = r#"
{
  "schema_version": 1,
  "id": "test.strict",
  "name": "Strict query-only instrument",
  "io": {
    "read_mode": "query_only",
    "allow_pending_responses": false,
    "clear_input": "never"
  },
  "commands": [
    { "id": "meas.vdc", "header": ":MEASure:VOLTage:DC", "readable": true, "result": "float" }
  ]
}
"#;

const NONBLOCKING_PROFILE: &str = r#"
{
  "schema_version": 1,
  "id": "test.nonblocking",
  "name": "Non-blocking instrument",
  "io": {
    "read_mode": "nonblocking",
    "allow_pending_responses": true,
    "clear_input": "manual"
  },
  "commands": [
    { "id": "meas.vdc", "header": ":MEASure:VOLTage:DC", "readable": true, "result": "float" }
  ]
}
"#;

#[test]
fn raw_query_leaves_a_readable_pending_response() {
    let mock = MockTransport::empty()
        .with_response(":RAW?", "hello")
        .with_response("*IDN?", "RIGOL,DM3058,SN,1.0");
    let mut instrument = Instrument::open(profile(), Box::new(mock)).unwrap();

    instrument.write_raw(":RAW?").unwrap();
    assert!(matches!(
        instrument.query_raw("*IDN?"),
        Err(Error::PendingResponse)
    ));
    assert_eq!(instrument.read_raw().unwrap(), Some("hello".into()));

    // Clearing a pending response is allowed by the default policy.
    instrument.write_raw(":RAW?").unwrap();
    instrument.clear_input().unwrap();
    assert_eq!(
        instrument.query_raw("*IDN?").unwrap(),
        "RIGOL,DM3058,SN,1.0"
    );
}

#[test]
fn query_only_forbids_raw_reads_and_raw_queries() {
    let profile = Arc::new(Profile::from_json_str(STRICT_PROFILE).unwrap());
    let mock = MockTransport::empty().with_response(":MEASure:VOLTage:DC?", "+1.0E+00");
    let mut instrument = Instrument::open(profile, Box::new(mock)).unwrap();

    assert!(matches!(
        instrument.read_raw(),
        Err(Error::ReadForbidden { .. })
    ));
    assert!(matches!(
        instrument.write_raw(":MEAS? "),
        Err(Error::RawQueryForbidden { .. })
    ));
    assert!(matches!(
        instrument.clear_input(),
        Err(Error::ClearForbidden { .. })
    ));

    // Paired queries still work, and ordinary writes are unaffected.
    assert_eq!(instrument.get("meas.vdc").unwrap(), Value::Float(1.0));
    instrument.write_raw(":FUNCtion:VOLTage:DC").unwrap();
}

#[test]
fn nonblocking_read_returns_none_when_empty() {
    let profile = Arc::new(Profile::from_json_str(NONBLOCKING_PROFILE).unwrap());
    let mock = MockTransport::empty().with_response(":RAW?", "later");
    let mut instrument = Instrument::open(profile, Box::new(mock)).unwrap();

    instrument.write_raw(":RAW?").unwrap();
    assert_eq!(instrument.read_raw().unwrap(), Some("later".into()));
    assert_eq!(instrument.read_raw().unwrap(), None);
}

#[test]
fn strict_policy_requires_a_pending_response_for_reads() {
    let json = STRICT_PROFILE.replace(r#""read_mode": "query_only""#, r#""read_mode": "blocking""#);
    let profile = Arc::new(Profile::from_json_str(&json).unwrap());
    let mut instrument = Instrument::open(profile, Box::new(MockTransport::empty())).unwrap();
    assert!(matches!(
        instrument.read_raw(),
        Err(Error::NoPendingResponse)
    ));
}

#[test]
fn io_policy_is_validated() {
    let contradiction = STRICT_PROFILE.replace(
        r#""allow_pending_responses": false"#,
        r#""allow_pending_responses": true"#,
    );
    assert!(Profile::from_json_str(&contradiction).is_err());

    let bad_mode = STRICT_PROFILE.replace(r#""read_mode": "query_only""#, r#""read_mode": "fast""#);
    assert!(Profile::from_json_str(&bad_mode).is_err());

    let on_open =
        STRICT_PROFILE.replace(r#""clear_input": "never""#, r#""clear_input": "on_open""#);
    assert!(Profile::from_json_str(&on_open).is_err());
}

#[test]
fn safety_output_wraps_writes_and_uses_any_args() {
    let profile = Profile::from_json_str(
        r#"{
          "schema_version": 1,
          "id": "test.safety",
          "name": "Safety test",
          "terminator": "\n",
          "commands": [
            {"id":"ch1.output","header":":OUTPut1","writable":true,"readable":true,
             "args":[{"name":"on","type":"bool"}],"result":"bool"},
            {"id":"ch1.freq","header":":SOURce1:FREQ","writable":true,"safety_output":"ch1.output",
             "args":[{"name":"hz","type":"any"}]}
          ]
        }"#,
    )
    .unwrap();
    let transport = MockTransport::empty();
    let writes = transport.writes();
    let mut instrument = Instrument::open(Arc::new(profile), Box::new(transport)).unwrap();
    instrument
        .set("ch1.freq", &[Value::String("1.5e3".into())])
        .unwrap();
    assert_eq!(
        *writes.lock().unwrap(),
        vec![":OUTPut1 OFF", ":SOURce1:FREQ 1.5e3", ":OUTPut1 ON"]
    );

    let profile = Profile::from_json_str(
        r#"{
          "schema_version": 1,
          "id": "test.safety2",
          "name": "Safety test 2",
          "terminator": "\n",
          "commands": [
            {"id":"ch1.output","header":":OUTPut1","writable":true,
             "args":[{"name":"on","type":"bool"}]},
            {"id":"ch1.freq","header":":SOURce1:FREQ","writable":true,"safety_output":"ch1.output",
             "args":[{"name":"hz","type":"float"}]}
          ]
        }"#,
    )
    .unwrap();
    let transport = MockTransport::empty();
    let writes = transport.writes();
    let mut instrument = Instrument::open(Arc::new(profile), Box::new(transport)).unwrap();
    assert!(instrument.set("ch1.freq", &[]).is_err());
    assert_eq!(*writes.lock().unwrap(), vec![":OUTPut1 OFF"]);
}

#[test]
fn query_arguments_are_rendered_after_the_question_mark() {
    let profile = Profile::from_json_str(
        r#"{
          "schema_version": 1,
          "id": "test.query_args",
          "name": "Query args",
          "terminator": "\n",
          "commands": [
            {"id":"meas.measure","header":":MEASure:ITEM","readable":true,
             "args":[{"name":"item","type":"any"},{"name":"source","type":"any"}],
             "result":"float"}
          ]
        }"#,
    )
    .unwrap();
    let transport =
        MockTransport::empty().with_response(":MEASure:ITEM? VPP,CHANnel1", "+1.500000E+00");
    let writes = transport.writes();
    let mut instrument = Instrument::open(Arc::new(profile), Box::new(transport)).unwrap();
    let reading = instrument
        .get_with_args(
            "meas.measure",
            &[
                Value::String("VPP".into()),
                Value::String("CHANnel1".into()),
            ],
        )
        .unwrap();
    assert_eq!(reading.value, Value::Float(1.5));
    assert_eq!(*writes.lock().unwrap(), vec![":MEASure:ITEM? VPP,CHANnel1"]);
}

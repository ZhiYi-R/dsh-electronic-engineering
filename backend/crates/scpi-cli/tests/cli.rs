//! Smoke tests for the `scpi` binary against a fixture profile and scripted mock.

use std::process::Command;

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_scpi"))
}

fn profile_path() -> String {
    format!(
        "{}/tests/fixtures/test-profile.json",
        env!("CARGO_MANIFEST_DIR")
    )
}

#[test]
fn get_returns_typed_json() {
    let output = binary()
        .args(["--profile", &profile_path(), "get", "meas.vdc"])
        .output()
        .expect("scpi runs");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["value"], 0.12345);
    assert_eq!(value["unit"], "V");
    assert_eq!(value["raw"], "+1.234500E-01");
}

#[test]
fn set_reports_the_wire_command() {
    let output = binary()
        .args(["--profile", &profile_path(), "set", "meas.vdc.range", "MIN"])
        .output()
        .expect("scpi runs");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["scpi"], ":MEASure:VOLTage:DC MIN");
}

#[test]
fn invalid_argument_exits_nonzero() {
    let output = binary()
        .args(["--profile", &profile_path(), "set", "meas.vdc.range", "9"])
        .output()
        .expect("scpi runs");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("above maximum"));
}

#[test]
fn info_exposes_the_io_policy() {
    let output = binary()
        .args(["--profile", &profile_path(), "info"])
        .output()
        .expect("scpi runs");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["io"]["read_mode"], "nonblocking");
    assert_eq!(value["io"]["allow_pending_responses"], true);
    assert_eq!(value["io"]["clear_input"], "manual");
}

#[test]
fn read_without_pending_data_is_null() {
    let output = binary()
        .args(["--profile", &profile_path(), "read"])
        .output()
        .expect("scpi runs");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["read"].is_null());
}

#[test]
fn clear_input_is_explicit() {
    let output = binary()
        .args(["--profile", &profile_path(), "clear-input"])
        .output()
        .expect("scpi runs");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["cleared"], true);
}

#[test]
fn write_refuses_a_query_command() {
    let output = binary()
        .args(["--profile", &profile_path(), "write", ":MEASure?"])
        .output()
        .expect("scpi runs");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("use 'query'"));
}

#[test]
fn profile_is_required() {
    let output = binary()
        .args(["get", "meas.vdc"])
        .output()
        .expect("scpi runs");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--profile"));
}

#[test]
fn discover_returns_a_json_array() {
    let output = binary().args(["discover"]).output().expect("scpi runs");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value.is_array());
}

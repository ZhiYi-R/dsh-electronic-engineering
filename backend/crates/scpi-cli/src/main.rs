// SPDX-License-Identifier: AGPL-3.0-or-later

//! `scpi`: CLI tool and JSON-lines stdio daemon (`scpi serve`).

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};

use scpi_core::profile::{Command, Profile};
use scpi_core::value::Value;
use scpi_core::Instrument;

#[derive(Parser, Debug)]
#[command(
    name = "scpi",
    version,
    about = "Data-driven SCPI instrument control (JSON profiles + Lua macros)"
)]
struct Cli {
    /// Instrument profile JSON. Required by every command that talks to an instrument.
    #[arg(long, global = true)]
    profile: Option<PathBuf>,

    /// Transport: mock, mock:<responses.json>, tcp://host:port,
    /// usb:<vid>:<pid>[:<serial>], or /dev/usbtmc0.
    #[arg(long, global = true, default_value = "mock")]
    transport: String,

    /// Skip the *IDN? profile check.
    #[arg(long, global = true)]
    no_idn_check: bool,

    #[command(subcommand)]
    command: Sub,
}

#[derive(Subcommand, Debug)]
enum Sub {
    /// Show profile, transport, and instrument identification.
    Info,
    /// List commands declared by the profile.
    List {
        /// Case-insensitive filter on id, header, or summary.
        filter: Option<String>,
    },
    /// Show one command definition as JSON.
    Describe { id: String },
    /// Query a command id and print value, unit, and raw response.
    Get { id: String },
    /// Drain the instrument error queue.
    Errors,
    /// Read one response without writing (profile read policy applies).
    Read,
    /// Discard buffered input (profile clear policy applies).
    ClearInput,
    /// Set a command id; arguments are coerced using the profile.
    Set { id: String, args: Vec<String> },
    /// Send a raw SCPI query and parse it when the profile knows the header.
    Query { scpi: String },
    /// Send a raw SCPI command without querying.
    Write { scpi: String },
    /// Call a Lua macro defined by the profile.
    Call { name: String, args: Vec<String> },
    /// Discover instruments on USB and, with --lan, on the network.
    Discover(DiscoverArgs),
    /// Run the newline-delimited JSON backend on stdin/stdout.
    Serve,
    /// Interactive prompt on top of the same engine.
    Repl,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Sub::Info => cmd_info(&cli),
        Sub::List { filter } => cmd_list(&cli, filter.as_deref()),
        Sub::Describe { id } => cmd_describe(&cli, id),
        Sub::Get { id } => cmd_get(&cli, id),
        Sub::Errors => cmd_errors(&cli),
        Sub::Read => cmd_read(&cli),
        Sub::ClearInput => cmd_clear_input(&cli),
        Sub::Set { id, args } => cmd_set(&cli, id, args),
        Sub::Query { scpi } => cmd_query(&cli, scpi),
        Sub::Write { scpi } => cmd_write(&cli, scpi),
        Sub::Call { name, args } => cmd_call(&cli, name, args),
        Sub::Discover(args) => cmd_discover(args),
        Sub::Serve => cmd_serve(&cli),
        Sub::Repl => cmd_repl(&cli),
    }
}

fn profile_path(cli: &Cli) -> Result<&Path> {
    cli.profile.as_deref().ok_or_else(|| {
        anyhow!("--profile <path> is required (the Rust executor has no built-in instrument)")
    })
}

fn load_profile(path: &Path) -> Result<Arc<Profile>> {
    let profile =
        Profile::load(path).with_context(|| format!("loading profile {}", path.display()))?;
    Ok(Arc::new(profile))
}

fn load_profile_arg(cli: &Cli) -> Result<Arc<Profile>> {
    load_profile(profile_path(cli)?)
}

/// `mock` uses `<profile-stem>.mock.json` next to the profile when present.
fn mock_companion(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_string_lossy();
    let candidate = path.with_file_name(format!("{stem}.mock.json"));
    candidate.is_file().then_some(candidate)
}

fn open(cli: &Cli) -> Result<(Instrument, Arc<Profile>)> {
    let path = profile_path(cli)?;
    let profile = load_profile(path)?;
    let transport_spec = if cli.transport == "mock" {
        mock_companion(path)
            .map(|p| format!("mock:{}", p.display()))
            .unwrap_or_else(|| "mock".to_string())
    } else {
        cli.transport.clone()
    };
    let transport =
        scpi_core::transport::open(&transport_spec, Duration::from_millis(profile.timeout_ms()))?;
    let mut instrument = Instrument::open(Arc::clone(&profile), transport)?;
    if !cli.no_idn_check {
        instrument.verify_identity()?;
    }
    Ok((instrument, profile))
}

fn cmd_info(cli: &Cli) -> Result<()> {
    let (mut instrument, profile) = open(cli)?;
    let idn = instrument.identify()?;
    let mut meta = profile.meta_json();
    if let Some(object) = meta.as_object_mut() {
        object.insert(
            "transport".into(),
            json!(instrument.transport_description()),
        );
        object.insert("idn".into(), json!(idn));
    }
    println!("{}", serde_json::to_string_pretty(&meta)?);
    Ok(())
}

fn cmd_list(cli: &Cli, filter: Option<&str>) -> Result<()> {
    let profile = load_profile_arg(cli)?;
    let filter = filter.map(|value| value.to_ascii_lowercase());
    println!("{:<32} {:<6} {:<46} SUMMARY", "ID", "ACCESS", "SCPI HEADER");
    for command in profile.commands() {
        if let Some(needle) = &filter {
            let haystack = format!(
                "{} {} {}",
                command.id,
                command.header,
                command.summary.as_deref().unwrap_or("")
            )
            .to_ascii_lowercase();
            if !haystack.contains(needle) {
                continue;
            }
        }
        let access = match (command.readable, command.writable) {
            (true, true) => "R/W",
            (true, false) => "R",
            (false, true) => "W",
            (false, false) => "-",
        };
        println!(
            "{:<32} {:<6} {:<46} {}",
            command.id,
            access,
            command.header,
            command.summary.as_deref().unwrap_or("")
        );
    }
    Ok(())
}

fn cmd_describe(cli: &Cli, id: &str) -> Result<()> {
    let profile = load_profile_arg(cli)?;
    let command = profile
        .command(id)
        .ok_or_else(|| anyhow!("unknown command id '{id}'"))?;
    println!("{}", serde_json::to_string_pretty(&command.to_json())?);
    Ok(())
}

fn cmd_get(cli: &Cli, id: &str) -> Result<()> {
    let (mut instrument, _profile) = open(cli)?;
    let reading = instrument.get_reading(id)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "command": id,
            "value": reading.value.to_plain_json(),
            "unit": reading.unit,
            "raw": reading.raw,
        }))?
    );
    Ok(())
}

fn cmd_errors(cli: &Cli) -> Result<()> {
    let (mut instrument, _profile) = open(cli)?;
    let errors = instrument.drain_errors()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({ "errors": errors }))?
    );
    Ok(())
}

fn cmd_read(cli: &Cli) -> Result<()> {
    let (mut instrument, _profile) = open(cli)?;
    match instrument.read_raw()? {
        Some(raw) => println!("{}", json!({ "read": raw })),
        None => println!("{}", json!({ "read": null })),
    }
    Ok(())
}

fn cmd_clear_input(cli: &Cli) -> Result<()> {
    let (mut instrument, _profile) = open(cli)?;
    instrument.clear_input()?;
    println!("{}", json!({ "cleared": true }));
    Ok(())
}

fn cmd_set(cli: &Cli, id: &str, args: &[String]) -> Result<()> {
    let (mut instrument, profile) = open(cli)?;
    let command = profile
        .command(id)
        .ok_or_else(|| anyhow!("unknown command id '{id}'"))?;
    let values = command.coerce_args(args)?;
    let wire = instrument.set_command(id, &values)?;
    println!("{}", json!({ "ok": true, "command": id, "scpi": wire }));
    Ok(())
}

fn cmd_query(cli: &Cli, scpi: &str) -> Result<()> {
    let (mut instrument, _profile) = open(cli)?;
    let (value, raw) = instrument.query_typed(scpi)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "scpi": scpi,
            "value": value.to_plain_json(),
            "raw": raw,
        }))?
    );
    Ok(())
}

fn cmd_write(cli: &Cli, scpi: &str) -> Result<()> {
    if looks_like_query(scpi) {
        bail!("'write' cannot send a query; use 'query' so the response is consumed");
    }
    let (mut instrument, _profile) = open(cli)?;
    instrument.write_raw(scpi)?;
    println!("{}", json!({ "ok": true, "scpi": scpi }));
    Ok(())
}

fn cmd_call(cli: &Cli, name: &str, args: &[String]) -> Result<()> {
    let (mut instrument, _profile) = open(cli)?;
    let values = args
        .iter()
        .map(|text| literal_value(text))
        .collect::<Vec<_>>();
    let result = instrument.call(name, &values)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "call": name,
            "value": result.to_plain_json(),
        }))?
    );
    Ok(())
}

fn looks_like_query(command: &str) -> bool {
    command
        .split_whitespace()
        .next()
        .is_some_and(|token| token.ends_with('?'))
}

fn literal_value(text: &str) -> Value {
    match text {
        "true" | "TRUE" | "on" | "ON" => return Value::Bool(true),
        "false" | "FALSE" | "off" | "OFF" => return Value::Bool(false),
        _ => {}
    }
    if let Ok(value) = text.parse::<i64>() {
        return Value::Int(value);
    }
    if let Ok(value) = text.parse::<f64>() {
        return Value::Float(value);
    }
    Value::String(text.to_string())
}

#[derive(Debug, Deserialize)]
struct Request {
    #[serde(default)]
    id: Option<JsonValue>,
    op: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    args: Vec<JsonValue>,
    #[serde(default)]
    scpi: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

fn cmd_discover(args: &DiscoverArgs) -> Result<()> {
    let implied_lan = !args.subnets.is_empty() || !args.hosts.is_empty() || !args.ports.is_empty();
    let lan = args.lan || implied_lan;
    let options = scpi_core::discovery::DiscoverOptions {
        usb: !args.no_usb,
        lan,
        mdns: lan && !args.no_mdns,
        sweep: lan && !args.no_sweep,
        subnets: args.subnets.clone(),
        hosts: args.hosts.clone(),
        ports: args.ports.clone(),
        probe_idn: !args.no_idn,
        connect_timeout: Duration::from_millis(args.sweep_timeout_ms),
        mdns_timeout: Duration::from_millis(args.mdns_timeout_ms),
        idn_timeout: Duration::from_millis(args.idn_timeout_ms),
    };
    let discovery = scpi_core::discovery::discover(&options)?;
    for warning in &discovery.warnings {
        eprintln!("warning: {warning}");
    }
    println!("{}", serde_json::to_string_pretty(&discovery.devices)?);
    Ok(())
}

/// Anything implying network work (`--lan`, `--subnet`, `--host`, `--port`)
/// also turns on LAN discovery.
#[derive(Args, Debug)]
struct DiscoverArgs {
    /// Also look for instruments on the network.
    #[arg(long)]
    lan: bool,

    /// Skip USBTMC enumeration.
    #[arg(long)]
    no_usb: bool,

    /// Skip the mDNS / DNS-SD browse (LAN only).
    #[arg(long)]
    no_mdns: bool,

    /// Skip the TCP subnet sweep (LAN only).
    #[arg(long)]
    no_sweep: bool,

    /// Subnet to sweep, e.g. 192.168.1.0/24. Repeatable; defaults to the
    /// local interfaces, clamped to /24.
    #[arg(long = "subnet", value_name = "CIDR")]
    subnets: Vec<String>,

    /// Address to probe, e.g. 192.168.1.40:5025. Repeatable.
    #[arg(long = "host", value_name = "HOST[:PORT]")]
    hosts: Vec<String>,

    /// Port to sweep. Repeatable; defaults to 5025 and 5555.
    #[arg(long = "port", value_name = "PORT")]
    ports: Vec<u16>,

    /// Do not query *IDN?; report anything that answers the socket.
    #[arg(long)]
    no_idn: bool,

    /// Per-address TCP connect timeout for the sweep, in milliseconds.
    #[arg(long, default_value_t = 250)]
    sweep_timeout_ms: u64,

    /// Total mDNS browse window, in milliseconds.
    #[arg(long, default_value_t = 1200)]
    mdns_timeout_ms: u64,

    /// *IDN? response timeout, in milliseconds.
    #[arg(long, default_value_t = 700)]
    idn_timeout_ms: u64,
}

fn cmd_serve(cli: &Cli) -> Result<()> {
    let (mut instrument, profile) = open(cli)?;
    eprintln!(
        "scpi backend ready: profile={} transport={}",
        profile.id,
        instrument.transport_description()
    );
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Request = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => {
                let response = json!({
                    "id": null,
                    "ok": false,
                    "error": { "kind": "bad_request", "message": error.to_string() },
                });
                writeln!(stdout, "{response}")?;
                stdout.flush()?;
                continue;
            }
        };
        let id = request.id.clone();
        let shutdown = request.op == "shutdown";
        let response = dispatch(&mut instrument, &profile, request);
        let response = match response {
            Ok(mut value) => {
                if let Some(object) = value.as_object_mut() {
                    object.insert("id".into(), id.clone().unwrap_or(JsonValue::Null));
                    object.insert("ok".into(), json!(true));
                }
                value
            }
            Err(error) => json!({
                "id": id,
                "ok": false,
                "error": { "kind": error.kind(), "message": error.to_string() },
            }),
        };
        writeln!(stdout, "{response}")?;
        stdout.flush()?;
        if shutdown {
            break;
        }
    }
    Ok(())
}

fn dispatch(
    instrument: &mut Instrument,
    profile: &Arc<Profile>,
    request: Request,
) -> scpi_core::Result<JsonValue> {
    match request.op.as_str() {
        "ping" => Ok(json!({ "pong": true })),
        "info" => {
            let idn = instrument.identify()?;
            let mut meta = profile.meta_json();
            if let Some(object) = meta.as_object_mut() {
                object.insert(
                    "transport".into(),
                    json!(instrument.transport_description()),
                );
                object.insert("idn".into(), json!(idn));
            }
            Ok(json!({ "profile": meta }))
        }
        "commands" => Ok(json!({
            "commands": profile.commands().iter().map(Command::to_json).collect::<Vec<_>>()
        })),
        "describe" => {
            let path = request
                .path
                .ok_or_else(|| scpi_core::Error::Message("'describe' needs a path".into()))?;
            let command = profile
                .command(&path)
                .ok_or_else(|| scpi_core::Error::UnknownCommand(path.clone()))?;
            Ok(json!({ "command": command.to_json() }))
        }
        "errors" => {
            let errors = instrument.drain_errors()?;
            Ok(json!({ "errors": errors }))
        }
        "read" => {
            let value = instrument.read_raw()?;
            Ok(json!({ "read": value }))
        }
        "clear" => {
            instrument.clear_input()?;
            Ok(json!({ "cleared": true }))
        }
        "get" => {
            let path = request
                .path
                .ok_or_else(|| scpi_core::Error::Message("'get' needs a path".into()))?;
            let command = profile
                .command(&path)
                .ok_or_else(|| scpi_core::Error::UnknownCommand(path.clone()))?;
            let values = if request.args.is_empty() {
                Vec::new()
            } else {
                json_args(command, &request.args)?
            };
            let reading = instrument.get_with_args(&path, &values)?;
            Ok(json!({
                "path": path,
                "value": reading.value.to_plain_json(),
                "unit": reading.unit,
                "raw": reading.raw,
            }))
        }
        "set" => {
            let path = request
                .path
                .ok_or_else(|| scpi_core::Error::Message("'set' needs a path".into()))?;
            let command = profile
                .command(&path)
                .ok_or_else(|| scpi_core::Error::UnknownCommand(path.clone()))?;
            let values = json_args(command, &request.args)?;
            let wire = instrument.set_command(&path, &values)?;
            Ok(json!({ "path": path, "scpi": wire }))
        }
        "query" => {
            let scpi = request
                .scpi
                .ok_or_else(|| scpi_core::Error::Message("'query' needs scpi".into()))?;
            let (value, raw) = instrument.query_typed(&scpi)?;
            Ok(json!({ "scpi": scpi, "value": value.to_plain_json(), "raw": raw }))
        }
        "write" => {
            let scpi = request
                .scpi
                .ok_or_else(|| scpi_core::Error::Message("'write' needs scpi".into()))?;
            instrument.write_raw(&scpi)?;
            Ok(json!({ "scpi": scpi }))
        }
        "call" => {
            let name = request
                .name
                .ok_or_else(|| scpi_core::Error::Message("'call' needs a name".into()))?;
            let values = request
                .args
                .iter()
                .map(json_scalar)
                .collect::<scpi_core::Result<Vec<_>>>()?;
            let result = instrument.call(&name, &values)?;
            Ok(json!({ "call": name, "value": result.to_plain_json() }))
        }
        "shutdown" => Ok(json!({ "bye": true })),
        other => Err(scpi_core::Error::Message(format!("unknown op '{other}'"))),
    }
}

fn json_args(command: &Command, args: &[JsonValue]) -> scpi_core::Result<Vec<Value>> {
    if args.len() != command.args.len() {
        return Err(scpi_core::Error::ArgCount {
            id: command.id.clone(),
            expected: command.args.len(),
            got: args.len(),
        });
    }
    command
        .args
        .iter()
        .zip(args.iter())
        .map(|(spec, value)| match value {
            JsonValue::Bool(flag) if spec.kind == scpi_core::ValueType::Bool => {
                Ok(Value::Bool(*flag))
            }
            JsonValue::Number(number) if spec.kind == scpi_core::ValueType::Int => number
                .as_i64()
                .map(Value::Int)
                .ok_or_else(|| scpi_core::Error::InvalidArg {
                    id: command.id.clone(),
                    name: spec.name.clone(),
                    reason: format!("{number} is not an integer"),
                }),
            JsonValue::Number(number) if spec.kind == scpi_core::ValueType::Float => number
                .as_f64()
                .map(Value::Float)
                .ok_or_else(|| scpi_core::Error::InvalidArg {
                    id: command.id.clone(),
                    name: spec.name.clone(),
                    reason: format!("{number} is not a number"),
                }),
            JsonValue::String(text) => spec.coerce(&command.id, text),
            JsonValue::Number(number) => spec.coerce(&command.id, &number.to_string()),
            JsonValue::Bool(flag) => spec.coerce(&command.id, &flag.to_string()),
            _ => Err(scpi_core::Error::InvalidArg {
                id: command.id.clone(),
                name: spec.name.clone(),
                reason: "expected a scalar JSON value".into(),
            }),
        })
        .collect()
}

fn json_scalar(value: &JsonValue) -> scpi_core::Result<Value> {
    match value {
        JsonValue::Bool(flag) => Ok(Value::Bool(*flag)),
        JsonValue::Number(number) => {
            if let Some(integer) = number.as_i64() {
                Ok(Value::Int(integer))
            } else {
                number.as_f64().map(Value::Float).ok_or_else(|| {
                    scpi_core::Error::Message(format!("unsupported number {number}"))
                })
            }
        }
        JsonValue::String(text) => Ok(Value::String(text.clone())),
        JsonValue::Null => Ok(Value::Raw(String::new())),
        _ => Err(scpi_core::Error::Message(
            "Lua arguments must be scalars".into(),
        )),
    }
}

fn cmd_repl(cli: &Cli) -> Result<()> {
    let (mut instrument, profile) = open(cli)?;
    println!(
        "profile={} transport={}  (try: help, get meas.vdc, set rate.dc_voltage F, call measure)",
        profile.id,
        instrument.transport_description()
    );
    let stdin = io::stdin();
    loop {
        print!("scpi> ");
        io::stdout().flush()?;
        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (verb, rest) = match line.split_once(char::is_whitespace) {
            Some((verb, rest)) => (verb, rest.trim()),
            None => (line, ""),
        };
        let result: Result<()> = match verb {
            "help" | "?" => {
                println!(
                    "commands: get <id> | set <id> <arg>... | query <SCPI> | write <SCPI> | call <macro> [arg]... | info | quit"
                );
                Ok(())
            }
            "quit" | "exit" => break,
            "info" => {
                let idn = instrument.identify()?;
                println!("{idn}");
                Ok(())
            }
            "get" => {
                let reading = instrument.get_reading(rest)?;
                println!(
                    "{} {}",
                    reading.value.display(),
                    reading.unit.as_deref().unwrap_or("")
                );
                Ok(())
            }
            "set" => {
                let mut parts = rest.split_whitespace();
                let id = parts
                    .next()
                    .ok_or_else(|| anyhow!("usage: set <id> <arg>..."))?;
                let args = parts.map(str::to_string).collect::<Vec<_>>();
                let command = profile
                    .command(id)
                    .ok_or_else(|| anyhow!("unknown command id '{id}'"))?;
                let values = command.coerce_args(&args)?;
                let wire = instrument.set_command(id, &values)?;
                println!("{wire}");
                Ok(())
            }
            "query" => {
                let (value, raw) = instrument.query_typed(rest)?;
                println!("{}  (raw: {raw})", value.display());
                Ok(())
            }
            "write" => {
                instrument.write_raw(rest)?;
                println!("ok");
                Ok(())
            }
            "call" => {
                let mut parts = rest.split_whitespace();
                let name = parts
                    .next()
                    .ok_or_else(|| anyhow!("usage: call <macro> [arg]..."))?;
                let values = parts.map(literal_value).collect::<Vec<_>>();
                let result = instrument.call(name, &values)?;
                println!("{}", result.display());
                Ok(())
            }
            other => {
                eprintln!("unknown command '{other}' (try: help)");
                Ok(())
            }
        };
        if let Err(error) = result {
            eprintln!("error: {error}");
        }
    }
    Ok(())
}

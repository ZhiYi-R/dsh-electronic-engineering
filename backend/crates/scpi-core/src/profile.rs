//! Instrument profiles: a JSON file declares every SCPI command an instrument
//! understands, plus argument and response types. The Rust side stays generic.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;
use serde_json::{json, Value as JsonValue};

use crate::error::{Error, Result};
use crate::scpi::HeaderPattern;
use crate::value::{format_float, parse_value, Value, ValueType};

/// Companion Lua macro script loaded alongside a profile.
#[derive(Debug, Clone)]
pub struct LuaScript {
    pub name: String,
    pub source: String,
}

/// How standalone `read()` calls (not paired with a query write) behave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadMode {
    /// Only paired queries are allowed; standalone `read()` is rejected.
    /// Needed for instruments like the Keysight 33600A that hang until timeout
    /// if read without a pending query.
    QueryOnly,
    /// Block until a newline-terminated response arrives or timeout elapses.
    Blocking,
    /// Return a complete buffered line if available, or `None` immediately.
    NonBlocking,
}

impl ReadMode {
    fn parse(text: &str) -> Result<Self> {
        match text {
            "query_only" => Ok(Self::QueryOnly),
            "blocking" => Ok(Self::Blocking),
            "nonblocking" | "non_blocking" => Ok(Self::NonBlocking),
            other => Err(Error::Profile(format!(
                "io.read_mode '{other}' is invalid (expected query_only, blocking, nonblocking)"
            ))),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::QueryOnly => "query_only",
            Self::Blocking => "blocking",
            Self::NonBlocking => "nonblocking",
        }
    }
}

/// Policy for clearing the instrument input buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearPolicy {
    /// Reject `clear_input()` calls.
    Never,
    /// Allow explicit `clear_input()` calls, but do not clear on open.
    Manual,
    /// Clear once when opening the session, and allow explicit clears later.
    OnOpen,
}

impl ClearPolicy {
    fn parse(text: &str) -> Result<Self> {
        match text {
            "never" => Ok(Self::Never),
            "manual" => Ok(Self::Manual),
            "on_open" => Ok(Self::OnOpen),
            other => Err(Error::Profile(format!(
                "io.clear_input '{other}' is invalid (expected never, manual, on_open)"
            ))),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Manual => "manual",
            Self::OnOpen => "on_open",
        }
    }
}

/// I/O and queue policy for an instrument profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoPolicy {
    pub read_mode: ReadMode,
    /// When `false`, queries must be consumed immediately and raw query writes
    /// via `write_raw` are rejected.
    pub allow_pending_responses: bool,
    pub clear_input: ClearPolicy,
}

impl Default for IoPolicy {
    fn default() -> Self {
        Self {
            read_mode: ReadMode::Blocking,
            allow_pending_responses: true,
            clear_input: ClearPolicy::Manual,
        }
    }
}

impl IoPolicy {
    pub fn to_json(&self) -> JsonValue {
        json!({
            "read_mode": self.read_mode.name(),
            "allow_pending_responses": self.allow_pending_responses,
            "clear_input": self.clear_input.name(),
        })
    }
}

/// One SCPI command definition compiled from a profile.
#[derive(Debug, Clone)]
pub struct Command {
    pub id: String,
    /// Canonical write header; queries append `?` unless `read_header` is set.
    pub header: String,
    /// Separate query header when read and write paths differ (e.g. `:MEAS:VOLT:DC:RANGe`).
    pub read_header: Option<String>,
    pub summary: Option<String>,
    pub writable: bool,
    pub readable: bool,
    /// Boolean output command to turn off before writing and back on after.
    pub safety_output: Option<String>,
    /// Hardware options required for this command.
    pub requires: Vec<String>,
    pub args: Vec<Arg>,
    pub result: Option<ResultSpec>,
    pattern: HeaderPattern,
    read_pattern: Option<HeaderPattern>,
}

/// One positional argument of a command.
#[derive(Debug, Clone)]
pub struct Arg {
    pub name: String,
    pub kind: ValueType,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// Accepted enum spellings (stored upper-case).
    pub values: Vec<String>,
    /// Permitted verbatim tokens such as `MIN`, `MAX`, `DEF`, `CURR`.
    pub special: Vec<String>,
    pub quoted: bool,
    pub unit: Option<String>,
}

/// Declared response type of a query.
#[derive(Debug, Clone)]
pub struct ResultSpec {
    pub kind: ValueType,
    pub unit: Option<String>,
}

/// A fully compiled instrument profile.
#[derive(Debug)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub manufacturer: Option<String>,
    pub description: Option<String>,
    pub idn_match: Vec<String>,
    pub terminator: String,
    pub timeout_ms: u64,
    pub error_query: Option<String>,
    pub io: IoPolicy,
    commands: Vec<Command>,
    index: HashMap<String, usize>,
    lua: Option<LuaScript>,
}

#[derive(Debug, Deserialize)]
struct RawProfile {
    schema_version: u32,
    id: String,
    name: String,
    #[serde(default)]
    manufacturer: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    idn_match: Vec<String>,
    #[serde(default = "default_terminator")]
    terminator: String,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default)]
    lua: Option<String>,
    #[serde(default)]
    error_query: Option<String>,
    #[serde(default)]
    io: RawIo,
    commands: Vec<RawCommand>,
}

#[derive(Debug, Default, Deserialize)]
struct RawIo {
    #[serde(default)]
    read_mode: Option<String>,
    #[serde(default)]
    allow_pending_responses: Option<bool>,
    #[serde(default)]
    clear_input: Option<String>,
}

impl RawIo {
    fn compile(self) -> Result<IoPolicy> {
        let mut policy = IoPolicy::default();
        if let Some(mode) = self.read_mode {
            policy.read_mode = ReadMode::parse(&mode)?;
        }
        if let Some(allow) = self.allow_pending_responses {
            policy.allow_pending_responses = allow;
        }
        if let Some(clear) = self.clear_input {
            policy.clear_input = ClearPolicy::parse(&clear)?;
        }
        if policy.read_mode == ReadMode::QueryOnly && policy.allow_pending_responses {
            return Err(Error::Profile(
                "io: read_mode 'query_only' requires allow_pending_responses=false".into(),
            ));
        }
        if policy.read_mode == ReadMode::QueryOnly && policy.clear_input == ClearPolicy::OnOpen {
            return Err(Error::Profile(
                "io: read_mode 'query_only' cannot use clear_input 'on_open'".into(),
            ));
        }
        Ok(policy)
    }
}

#[derive(Debug, Deserialize)]
struct RawCommand {
    id: String,
    header: String,
    #[serde(default)]
    query_header: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    writable: bool,
    #[serde(default)]
    readable: bool,
    #[serde(default)]
    safety_output: Option<String>,
    #[serde(default)]
    requires: Vec<String>,
    #[serde(default)]
    args: Vec<RawArg>,
    #[serde(default)]
    result: Option<RawResult>,
}

#[derive(Debug, Deserialize)]
struct RawArg {
    name: String,
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    min: Option<f64>,
    #[serde(default)]
    max: Option<f64>,
    #[serde(default)]
    values: Vec<String>,
    #[serde(default)]
    special: Vec<String>,
    #[serde(default)]
    quoted: bool,
    #[serde(default)]
    unit: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawResult {
    Type(String),
    Detailed {
        #[serde(rename = "type")]
        ty: String,
        #[serde(default)]
        unit: Option<String>,
    },
}

fn default_terminator() -> String {
    "\n".into()
}

fn default_timeout_ms() -> u64 {
    10_000
}

fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

impl Profile {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .map_err(|e| Error::Profile(format!("cannot read {}: {e}", path.display())))?;
        let base = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf);
        Self::from_json(&text, base.as_deref())
    }

    /// Parse a profile from a string. Lua files cannot be resolved without a
    /// base directory, so profiles with a `lua` field need [`Profile::load`]
    /// or [`Profile::from_json_with_base`].
    pub fn from_json_str(text: &str) -> Result<Self> {
        Self::from_json(text, None)
    }

    pub fn from_json_with_base(text: &str, base: &Path) -> Result<Self> {
        Self::from_json(text, Some(base))
    }

    fn from_json(text: &str, base: Option<&Path>) -> Result<Self> {
        let raw: RawProfile =
            serde_json::from_str(text).map_err(|e| Error::Profile(format!("invalid JSON: {e}")))?;
        if raw.schema_version != 1 {
            return Err(Error::Profile(format!(
                "unsupported schema_version {} (expected 1)",
                raw.schema_version
            )));
        }
        if raw.id.trim().is_empty() || raw.name.trim().is_empty() {
            return Err(Error::Profile(
                "profile 'id' and 'name' must not be empty".into(),
            ));
        }
        if raw.terminator.is_empty() {
            return Err(Error::Profile("terminator must not be empty".into()));
        }
        if raw.timeout_ms == 0 {
            return Err(Error::Profile(
                "timeout_ms must be greater than zero".into(),
            ));
        }
        if raw.commands.is_empty() {
            return Err(Error::Profile("profile declares no commands".into()));
        }

        let lua = match &raw.lua {
            Some(name) => {
                let base = base.ok_or_else(|| {
                    Error::Profile(format!(
                        "profile declares lua file '{name}' but no base directory is available"
                    ))
                })?;
                let path = base.join(name);
                let source = fs::read_to_string(&path)
                    .map_err(|e| Error::Profile(format!("cannot read {}: {e}", path.display())))?;
                Some(LuaScript {
                    name: name.clone(),
                    source,
                })
            }
            None => None,
        };

        let mut commands = Vec::with_capacity(raw.commands.len());
        let mut index = HashMap::with_capacity(raw.commands.len());
        for raw_cmd in raw.commands {
            if !valid_id(&raw_cmd.id) {
                return Err(Error::Profile(format!(
                    "invalid command id '{}'",
                    raw_cmd.id
                )));
            }
            if index.contains_key(&raw_cmd.id) {
                return Err(Error::Profile(format!(
                    "duplicate command id '{}'",
                    raw_cmd.id
                )));
            }
            if !raw_cmd.writable && !raw_cmd.readable {
                return Err(Error::Profile(format!(
                    "command '{}' is neither readable nor writable",
                    raw_cmd.id
                )));
            }
            let pattern = HeaderPattern::parse(&raw_cmd.header)
                .map_err(|e| Error::Profile(format!("command '{}': {e}", raw_cmd.id)))?;
            if pattern.query {
                return Err(Error::Profile(format!(
                    "command '{}': header must not contain '?', queries are derived",
                    raw_cmd.id
                )));
            }
            let read_pattern = match &raw_cmd.query_header {
                Some(header) => {
                    let parsed = HeaderPattern::parse(header).map_err(|e| {
                        Error::Profile(format!("command '{}' query_header: {e}", raw_cmd.id))
                    })?;
                    if parsed.query {
                        return Err(Error::Profile(format!(
                            "command '{}': query_header must not contain '?'",
                            raw_cmd.id
                        )));
                    }
                    if !raw_cmd.readable {
                        return Err(Error::Profile(format!(
                            "command '{}': query_header requires readable=true",
                            raw_cmd.id
                        )));
                    }
                    Some(parsed)
                }
                None => None,
            };
            let args = raw_cmd
                .args
                .into_iter()
                .map(|arg| Arg::compile(&raw_cmd.id, arg))
                .collect::<Result<Vec<_>>>()?;
            let mut result = match raw_cmd.result {
                Some(RawResult::Type(ty)) => Some(ResultSpec {
                    kind: ValueType::parse(&ty)?,
                    unit: None,
                }),
                Some(RawResult::Detailed { ty, unit }) => Some(ResultSpec {
                    kind: ValueType::parse(&ty)?,
                    unit,
                }),
                None => None,
            };
            if raw_cmd.readable && result.is_none() {
                result = Some(ResultSpec {
                    kind: ValueType::Raw,
                    unit: None,
                });
            }
            let cmd = Command {
                id: raw_cmd.id.clone(),
                header: raw_cmd.header,
                read_header: raw_cmd.query_header,
                summary: raw_cmd.summary,
                writable: raw_cmd.writable,
                readable: raw_cmd.readable,
                safety_output: raw_cmd.safety_output,
                requires: raw_cmd.requires,
                args,
                result,
                pattern,
                read_pattern,
            };
            index.insert(cmd.id.clone(), commands.len());
            commands.push(cmd);
        }

        for cmd in &commands {
            if let Some(output_id) = &cmd.safety_output {
                let output = index
                    .get(output_id)
                    .map(|position| &commands[*position])
                    .ok_or_else(|| {
                        Error::Profile(format!(
                            "command '{}' safety_output references unknown command '{}'",
                            cmd.id, output_id
                        ))
                    })?;
                if !output.writable
                    || output.args.len() != 1
                    || output.args[0].kind != ValueType::Bool
                {
                    return Err(Error::Profile(format!(
                        "command '{}' safety_output '{}' must be a writable bool command with one argument",
                        cmd.id, output_id
                    )));
                }
                if output.safety_output.is_some() {
                    return Err(Error::Profile(format!(
                        "command '{}' safety_output '{}' must not itself use safety_output",
                        cmd.id, output_id
                    )));
                }
            }
        }

        if let Some(error_id) = &raw.error_query {
            match index.get(error_id) {
                Some(pos) if commands[*pos].readable => {}
                Some(_) => {
                    return Err(Error::Profile(format!(
                        "error_query '{error_id}' is not readable"
                    )))
                }
                None => {
                    return Err(Error::Profile(format!(
                        "error_query '{error_id}' is not declared"
                    )))
                }
            }
        }

        Ok(Self {
            id: raw.id,
            name: raw.name,
            manufacturer: raw.manufacturer,
            description: raw.description,
            idn_match: raw.idn_match,
            terminator: raw.terminator,
            timeout_ms: raw.timeout_ms,
            error_query: raw.error_query,
            io: raw.io.compile()?,
            commands,
            index,
            lua,
        })
    }

    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    pub fn command(&self, id: &str) -> Option<&Command> {
        self.index.get(id).map(|pos| &self.commands[*pos])
    }

    pub fn command_count(&self) -> usize {
        self.commands.len()
    }

    pub fn lua_script(&self) -> Option<&LuaScript> {
        self.lua.as_ref()
    }

    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }

    /// Find the readable command whose declared header matches a raw query.
    pub fn readable_command(&self, header: &HeaderPattern) -> Option<&Command> {
        self.commands.iter().find(|cmd| {
            cmd.readable
                && (cmd.pattern.matches_path(header)
                    || cmd
                        .read_pattern
                        .as_ref()
                        .is_some_and(|pattern| pattern.matches_path(header)))
        })
    }

    pub fn meta_json(&self) -> JsonValue {
        json!({
            "id": self.id,
            "name": self.name,
            "manufacturer": self.manufacturer,
            "description": self.description,
            "idn_match": self.idn_match,
            "terminator": self.terminator,
            "timeout_ms": self.timeout_ms,
            "error_query": self.error_query,
            "io": self.io.to_json(),
            "commands": self.commands.len(),
        })
    }
}

impl Command {
    pub fn query_header(&self) -> String {
        match &self.read_header {
            Some(header) => format!("{header}?"),
            None => format!("{}?", self.header),
        }
    }

    pub fn is_readable(&self) -> bool {
        self.readable
    }

    pub fn is_writable(&self) -> bool {
        self.writable
    }

    pub fn parse_result(&self, raw: &str) -> Result<Value> {
        let spec = self.result.clone().unwrap_or(ResultSpec {
            kind: ValueType::Raw,
            unit: None,
        });
        parse_value(raw, spec.kind).map_err(|reason| Error::Parse {
            id: self.id.clone(),
            kind: spec.kind.name(),
            reason,
            raw: raw.to_string(),
        })
    }

    /// Render `HEADER arg1,arg2` after validating every argument.
    pub fn format_set(&self, args: &[Value]) -> Result<String> {
        if !self.writable {
            return Err(Error::NotWritable {
                id: self.id.clone(),
            });
        }
        if args.len() != self.args.len() {
            return Err(Error::ArgCount {
                id: self.id.clone(),
                expected: self.args.len(),
                got: args.len(),
            });
        }
        if args.is_empty() {
            return Ok(self.header.clone());
        }
        let mut rendered = Vec::with_capacity(args.len());
        for (spec, value) in self.args.iter().zip(args.iter()) {
            rendered.push(spec.format(&self.id, value)?);
        }
        Ok(format!("{} {}", self.header, rendered.join(",")))
    }

    /// Render `HEADER?` optionally followed by the declared query arguments.
    pub fn format_query(&self, args: &[Value]) -> Result<String> {
        if !self.readable {
            return Err(Error::NotReadable {
                id: self.id.clone(),
            });
        }
        let header = self.query_header();
        if args.is_empty() {
            return Ok(header);
        }
        if args.len() != self.args.len() {
            return Err(Error::ArgCount {
                id: self.id.clone(),
                expected: self.args.len(),
                got: args.len(),
            });
        }
        let mut rendered = Vec::with_capacity(args.len());
        for (spec, value) in self.args.iter().zip(args.iter()) {
            rendered.push(spec.format(&self.id, value)?);
        }
        Ok(format!("{header} {}", rendered.join(",")))
    }

    /// Parse a literal supplied on the command line according to argument specs.
    pub fn coerce_args(&self, texts: &[String]) -> Result<Vec<Value>> {
        if texts.len() != self.args.len() {
            return Err(Error::ArgCount {
                id: self.id.clone(),
                expected: self.args.len(),
                got: texts.len(),
            });
        }
        self.args
            .iter()
            .zip(texts.iter())
            .map(|(spec, text)| spec.coerce(&self.id, text))
            .collect()
    }

    pub fn to_json(&self) -> JsonValue {
        let mut out = json!({
            "id": self.id,
            "header": self.header,
            "summary": self.summary,
            "writable": self.writable,
            "readable": self.readable,
            "args": self.args.iter().map(Arg::to_json).collect::<Vec<_>>(),
            "result": self.result.as_ref().map(|r| json!({
                "type": r.kind.name(),
                "unit": r.unit,
            })),
        });
        if let Some(header) = &self.read_header {
            if let Some(object) = out.as_object_mut() {
                object.insert("query_header".into(), json!(header));
            }
        }
        if let Some(output) = &self.safety_output {
            if let Some(object) = out.as_object_mut() {
                object.insert("safety_output".into(), json!(output));
            }
        }
        if !self.requires.is_empty() {
            if let Some(object) = out.as_object_mut() {
                object.insert("requires".into(), json!(self.requires));
            }
        }
        out
    }
}

impl Arg {
    fn compile(command_id: &str, raw: RawArg) -> Result<Self> {
        let kind = ValueType::parse(&raw.ty).map_err(|e| {
            Error::Profile(format!(
                "command '{command_id}' argument '{}': {e}",
                raw.name
            ))
        })?;
        if kind == ValueType::Raw {
            return Err(Error::Profile(format!(
                "command '{command_id}' argument '{}': 'raw' is not a valid argument type",
                raw.name
            )));
        }
        if let (Some(min), Some(max)) = (raw.min, raw.max) {
            if min > max {
                return Err(Error::Profile(format!(
                    "command '{command_id}' argument '{}': min > max",
                    raw.name
                )));
            }
        }
        let values = raw
            .values
            .into_iter()
            .map(|v| v.to_ascii_uppercase())
            .collect::<Vec<_>>();
        if kind == ValueType::Enum && values.is_empty() {
            return Err(Error::Profile(format!(
                "command '{command_id}' argument '{}': enum needs 'values'",
                raw.name
            )));
        }
        Ok(Self {
            name: raw.name,
            kind,
            min: raw.min,
            max: raw.max,
            values,
            special: raw
                .special
                .into_iter()
                .map(|s| s.to_ascii_uppercase())
                .collect(),
            quoted: raw.quoted,
            unit: raw.unit,
        })
    }

    pub fn coerce(&self, command_id: &str, text: &str) -> Result<Value> {
        let text = text.trim();
        if self.special.iter().any(|s| s.eq_ignore_ascii_case(text)) {
            return Ok(Value::Raw(text.to_ascii_uppercase()));
        }
        let invalid = |reason: String| Error::InvalidArg {
            id: command_id.to_string(),
            name: self.name.clone(),
            reason,
        };
        match self.kind {
            ValueType::Bool => match text.to_ascii_uppercase().as_str() {
                "1" | "ON" | "TRUE" | "YES" => Ok(Value::Bool(true)),
                "0" | "OFF" | "FALSE" | "NO" => Ok(Value::Bool(false)),
                _ => Err(invalid(format!("'{text}' is not a boolean"))),
            },
            ValueType::Int => text
                .parse::<i64>()
                .map(Value::Int)
                .map_err(|_| invalid(format!("'{text}' is not an integer"))),
            ValueType::Float => text
                .parse::<f64>()
                .map(Value::Float)
                .map_err(|_| invalid(format!("'{text}' is not a number"))),
            ValueType::String => Ok(Value::String(text.to_string())),
            ValueType::Enum => self
                .canonical_enum(text)
                .map(Value::String)
                .map_err(invalid),
            ValueType::Raw => Ok(Value::Raw(text.to_string())),
            ValueType::Any => Ok(Value::String(text.to_string())),
        }
    }

    fn canonical_enum(&self, text: &str) -> std::result::Result<String, String> {
        self.values
            .iter()
            .find(|v| v.eq_ignore_ascii_case(text))
            .cloned()
            .ok_or_else(|| format!("expected one of [{}], got '{text}'", self.values.join(", ")))
    }

    fn format(&self, command_id: &str, value: &Value) -> Result<String> {
        let invalid = |reason: String| Error::InvalidArg {
            id: command_id.to_string(),
            name: self.name.clone(),
            reason,
        };

        if let Value::Raw(text) = value {
            if self.special.iter().any(|s| s.eq_ignore_ascii_case(text)) {
                return Ok(text.to_ascii_uppercase());
            }
        }
        if let Value::String(text) = value {
            if self.special.iter().any(|s| s.eq_ignore_ascii_case(text)) {
                return Ok(text.to_ascii_uppercase());
            }
        }

        match (self.kind, value) {
            (ValueType::Bool, Value::Bool(v)) => Ok(if *v { "ON" } else { "OFF" }.into()),
            (ValueType::Int, Value::Int(v)) => {
                self.check_range(command_id, *v as f64)?;
                Ok(v.to_string())
            }
            (ValueType::Int, Value::Float(v)) if v.fract() == 0.0 => {
                self.check_range(command_id, *v)?;
                Ok((*v as i64).to_string())
            }
            (ValueType::Int, Value::String(v)) => {
                let parsed = v
                    .parse::<i64>()
                    .map_err(|_| invalid(format!("'{v}' is not an integer")))?;
                self.check_range(command_id, parsed as f64)?;
                Ok(parsed.to_string())
            }
            (ValueType::Float, Value::Float(v)) => {
                self.check_range(command_id, *v)?;
                Ok(format_float(*v))
            }
            (ValueType::Float, Value::Int(v)) => {
                self.check_range(command_id, *v as f64)?;
                Ok(v.to_string())
            }
            (ValueType::Float, Value::String(v)) => {
                let parsed = v
                    .parse::<f64>()
                    .map_err(|_| invalid(format!("'{v}' is not a number")))?;
                self.check_range(command_id, parsed)?;
                Ok(format_float(parsed))
            }
            (ValueType::String, Value::String(v)) => {
                Ok(if self.quoted { quote(v) } else { v.clone() })
            }
            (ValueType::Enum, Value::String(v)) => self.canonical_enum(v).map_err(invalid),
            (ValueType::Enum, Value::Raw(v)) => self.canonical_enum(v).map_err(invalid),
            (ValueType::Any, Value::Bool(v)) => Ok(if *v { "ON" } else { "OFF" }.into()),
            (ValueType::Any, Value::Int(v)) => Ok(v.to_string()),
            (ValueType::Any, Value::Float(v)) => Ok(format_float(*v)),
            (ValueType::Any, Value::String(v)) | (ValueType::Any, Value::Raw(v)) => {
                Ok(if self.quoted { quote(v) } else { v.clone() })
            }
            _ => Err(invalid(format!(
                "expected {}, got {}",
                self.kind.name(),
                value_type_name(value)
            ))),
        }
    }

    fn check_range(&self, command_id: &str, value: f64) -> Result<()> {
        if let Some(min) = self.min {
            if value < min {
                return Err(Error::InvalidArg {
                    id: command_id.to_string(),
                    name: self.name.clone(),
                    reason: format!("{value} is below minimum {min}"),
                });
            }
        }
        if let Some(max) = self.max {
            if value > max {
                return Err(Error::InvalidArg {
                    id: command_id.to_string(),
                    name: self.name.clone(),
                    reason: format!("{value} is above maximum {max}"),
                });
            }
        }
        Ok(())
    }

    pub fn to_json(&self) -> JsonValue {
        let mut out = json!({
            "name": self.name,
            "type": self.kind.name(),
        });
        if let Some(obj) = out.as_object_mut() {
            if let Some(min) = self.min {
                obj.insert("min".into(), json!(min));
            }
            if let Some(max) = self.max {
                obj.insert("max".into(), json!(max));
            }
            if !self.values.is_empty() {
                obj.insert("values".into(), json!(self.values));
            }
            if !self.special.is_empty() {
                obj.insert("special".into(), json!(self.special));
            }
            if self.quoted {
                obj.insert("quoted".into(), json!(true));
            }
            if let Some(unit) = &self.unit {
                obj.insert("unit".into(), json!(unit));
            }
        }
        out
    }
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::String(_) => "string",
        Value::Raw(_) => "raw",
    }
}

fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}

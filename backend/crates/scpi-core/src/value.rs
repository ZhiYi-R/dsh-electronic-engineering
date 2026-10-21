//! Typed values exchanged with instruments.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The type a command argument or response is declared as in the JSON profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValueType {
    Float,
    Int,
    Bool,
    String,
    Enum,
    Raw,
    /// Accept any scalar and render it verbatim (untyped generated profiles).
    Any,
}

impl ValueType {
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "float" => Ok(Self::Float),
            "int" => Ok(Self::Int),
            "bool" => Ok(Self::Bool),
            "string" => Ok(Self::String),
            "enum" => Ok(Self::Enum),
            "raw" => Ok(Self::Raw),
            "any" => Ok(Self::Any),
            other => Err(Error::Profile(format!(
                "unknown value type '{other}' (expected float, int, bool, string, enum, raw, any)"
            ))),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Float => "float",
            Self::Int => "int",
            Self::Bool => "bool",
            Self::String => "string",
            Self::Enum => "enum",
            Self::Raw => "raw",
            Self::Any => "any",
        }
    }
}

/// A scalar value sent to or received from an instrument.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "lowercase")]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    /// A verbatim token (for example `MIN`, `MAX`, `DEF`) or an unparsed response.
    Raw(String),
}

impl Value {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Float(v) => Some(*v),
            Value::Int(v) => Some(*v as f64),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v),
            Value::Float(v) if v.fract() == 0.0 => Some(*v as i64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(v) | Value::Raw(v) => Some(v),
            _ => None,
        }
    }

    /// JSON without the type tag, suitable for human-facing CLI output.
    pub fn to_plain_json(&self) -> serde_json::Value {
        match self {
            Value::Bool(v) => serde_json::Value::Bool(*v),
            Value::Int(v) => serde_json::Value::from(*v),
            Value::Float(v) if v.is_finite() => serde_json::Value::from(*v),
            Value::Float(v) => serde_json::Value::String(v.to_string()),
            Value::String(v) | Value::Raw(v) => serde_json::Value::String(v.clone()),
        }
    }

    /// Human-readable form (used for error-queue entries).
    pub fn display(&self) -> String {
        match self {
            Value::Bool(v) => {
                if *v {
                    "ON".into()
                } else {
                    "OFF".into()
                }
            }
            Value::Int(v) => v.to_string(),
            Value::Float(v) => format_float(*v),
            Value::String(v) | Value::Raw(v) => v.clone(),
        }
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}
impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Int(v)
    }
}
impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Float(v)
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::String(v)
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::String(v.to_string())
    }
}

/// Format a float for SCPI without an exponent and without a trailing `.0`.
pub fn format_float(value: f64) -> String {
    if value.is_nan() {
        return "NAN".into();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-INF".into()
        } else {
            "INF".into()
        };
    }
    let text = format!("{value}");
    if text == "-0" {
        "0".into()
    } else {
        text
    }
}

/// Parse a raw SCPI response according to its declared type.
pub fn parse_value(raw: &str, kind: ValueType) -> std::result::Result<Value, String> {
    let text = raw.trim();
    match kind {
        ValueType::Float => parse_f64(text).map(Value::Float),
        ValueType::Int => parse_i64(text).map(Value::Int),
        ValueType::Bool => parse_bool(text).map(Value::Bool),
        ValueType::String => Ok(Value::String(unquote(text))),
        ValueType::Enum => Ok(Value::String(unquote(text).to_ascii_uppercase())),
        ValueType::Raw => Ok(Value::Raw(raw.trim_end_matches(['\r', '\n']).to_string())),
        ValueType::Any => Ok(Value::Raw(raw.trim_end_matches(['\r', '\n']).to_string())),
    }
}

fn parse_f64(text: &str) -> std::result::Result<f64, String> {
    let upper = text.to_ascii_uppercase();
    match upper.as_str() {
        "INF" | "+INF" | "INFINITY" | "+INFINITY" => return Ok(f64::INFINITY),
        "-INF" | "-INFINITY" => return Ok(f64::NEG_INFINITY),
        "NAN" => return Ok(f64::NAN),
        _ => {}
    }
    text.parse::<f64>()
        .map_err(|_| format!("'{text}' is not a number"))
}

fn parse_i64(text: &str) -> std::result::Result<i64, String> {
    if let Ok(v) = text.parse::<i64>() {
        return Ok(v);
    }
    // Instruments often return integers as `5.0E+00`.
    let f = parse_f64(text)?;
    if !f.is_finite() {
        return Err(format!("'{text}' is not a finite integer"));
    }
    let truncated = f.trunc();
    if truncated < i64::MIN as f64 || truncated > i64::MAX as f64 {
        return Err(format!("'{text}' is outside the i64 range"));
    }
    Ok(truncated as i64)
}

fn parse_bool(text: &str) -> std::result::Result<bool, String> {
    match text.to_ascii_uppercase().as_str() {
        "1" | "ON" | "TRUE" | "YES" => Ok(true),
        "0" | "OFF" | "FALSE" | "NO" => Ok(false),
        _ => Err(format!("'{text}' is not a boolean")),
    }
}

/// Strip one layer of SCPI double quotes and unescape doubled quotes.
fn unquote(text: &str) -> String {
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        text[1..text.len() - 1].replace("\"\"", "\"")
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_instrument_number_formats() {
        assert_eq!(
            parse_value("+1.234500E-01", ValueType::Float),
            Ok(Value::Float(0.12345))
        );
        assert_eq!(parse_value("5.0E+00", ValueType::Int), Ok(Value::Int(5)));
        assert_eq!(
            parse_value("INFinity", ValueType::Float).unwrap().as_f64(),
            Some(f64::INFINITY)
        );
        assert_eq!(
            parse_value("9.9E+37", ValueType::Float).unwrap().as_f64(),
            Some(9.9e37)
        );
    }

    #[test]
    fn parses_booleans_and_strings() {
        assert_eq!(parse_value("ON", ValueType::Bool), Ok(Value::Bool(true)));
        assert_eq!(parse_value("0", ValueType::Bool), Ok(Value::Bool(false)));
        assert_eq!(
            parse_value("\"INT:\\data\\a\"", ValueType::String),
            Ok(Value::String("INT:\\data\\a".into()))
        );
        assert_eq!(
            parse_value("\"a\"\"b\"", ValueType::String),
            Ok(Value::String("a\"b".into()))
        );
    }

    #[test]
    fn formats_clean_floats() {
        assert_eq!(format_float(1.0), "1");
        assert_eq!(format_float(0.001), "0.001");
        assert_eq!(format_float(-0.0), "0");
    }
}

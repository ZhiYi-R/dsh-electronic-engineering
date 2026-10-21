//! Error type shared by the SCPI engine.

use std::time::Duration;

/// Everything that can go wrong while loading a profile, talking to an
/// instrument, or evaluating a Lua macro.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("transport error: {0}")]
    Transport(String),

    #[error("timeout after {0:?} while waiting for a response")]
    Timeout(Duration),

    #[error("profile error: {0}")]
    Profile(String),

    #[error("unknown command id: {0}")]
    UnknownCommand(String),

    #[error("command '{id}' is not readable")]
    NotReadable { id: String },

    #[error("command '{id}' is not writable")]
    NotWritable { id: String },

    #[error("command '{id}' expects {expected} argument(s), got {got}")]
    ArgCount {
        id: String,
        expected: usize,
        got: usize,
    },

    #[error("invalid argument '{name}' for '{id}': {reason}")]
    InvalidArg {
        id: String,
        name: String,
        reason: String,
    },

    #[error("cannot parse response for '{id}' as {kind}: {reason} (raw: {raw:?})")]
    Parse {
        id: String,
        kind: &'static str,
        reason: String,
        raw: String,
    },

    #[error("unexpected instrument: expected one of {expected:?}, got {actual:?}")]
    UnexpectedInstrument {
        expected: Vec<String>,
        actual: String,
    },

    #[error("profile '{profile}' only allows paired queries; standalone reads are disabled")]
    ReadForbidden { profile: String },

    #[error("profile '{profile}' requires queries to be consumed immediately; raw query writes are disabled")]
    RawQueryForbidden { profile: String },

    #[error("profile '{profile}' forbids clearing the input buffer")]
    ClearForbidden { profile: String },

    #[error(
        "a query response is still pending; read it or clear input before issuing another query"
    )]
    PendingResponse,

    #[error("there is no pending response to read")]
    NoPendingResponse,

    #[error("SCPI error: {0}")]
    Scpi(String),

    #[error("Lua error: {0}")]
    Lua(String),

    #[error("feature '{0}' is not available in this build")]
    FeatureDisabled(&'static str),

    #[error("unsupported transport spec: {0}")]
    TransportSpec(String),

    #[error("invalid SCPI header: {0}")]
    InvalidHeader(String),

    #[error("{0}")]
    Message(String),
}

impl Error {
    /// Stable machine-readable kind used by the JSON-lines backend.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::Io(_) => "io",
            Error::Transport(_) => "transport",
            Error::Timeout(_) => "timeout",
            Error::Profile(_) => "profile",
            Error::UnknownCommand(_) => "unknown_command",
            Error::NotReadable { .. } => "not_readable",
            Error::NotWritable { .. } => "not_writable",
            Error::ArgCount { .. } => "arg_count",
            Error::InvalidArg { .. } => "invalid_argument",
            Error::Parse { .. } => "parse",
            Error::UnexpectedInstrument { .. } => "unexpected_instrument",
            Error::ReadForbidden { .. } => "read_forbidden",
            Error::RawQueryForbidden { .. } => "raw_query_forbidden",
            Error::ClearForbidden { .. } => "clear_forbidden",
            Error::PendingResponse => "pending_response",
            Error::NoPendingResponse => "no_pending_response",
            Error::Scpi(_) => "scpi",
            Error::Lua(_) => "lua",
            Error::FeatureDisabled(_) => "feature_disabled",
            Error::TransportSpec(_) => "transport_spec",
            Error::InvalidHeader(_) => "invalid_header",
            Error::Message(_) => "error",
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

// SPDX-License-Identifier: AGPL-3.0-or-later

//! Generic data-driven SCPI engine.
//!
//! Handles framing, argument validation, response parsing, output interlocks,
//! error-queue polling, and USBTMC/TCP/mock transports. Concrete instrument
//! command sets live in external JSON profiles (and optional Lua scripts).

pub mod discovery;
pub mod error;
pub mod instrument;
pub mod profile;
pub mod scpi;
pub mod transport;
pub mod value;

#[cfg(feature = "lua")]
mod lua;

pub use error::{Error, Result};
pub use instrument::{Instrument, Reading};
pub use profile::{Arg, ClearPolicy, Command, IoPolicy, Profile, ReadMode, ResultSpec};
pub use scpi::HeaderPattern;
pub use value::{Value, ValueType};

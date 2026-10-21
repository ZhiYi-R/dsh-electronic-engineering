//! High-level instrument handle: typed commands, raw SCPI, and Lua macros.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::profile::{ClearPolicy, Command, Profile, ReadMode};
use crate::scpi::HeaderPattern;
use crate::transport::Transport;
use crate::value::Value;

/// One typed query result together with the raw wire response and unit.
#[derive(Debug, Clone)]
pub struct Reading {
    pub value: Value,
    pub unit: Option<String>,
    pub raw: String,
}

/// 绑定了指定 Profile 档案与底层传输通道的单台仪器活跃会话。
///
/// 【设计权衡】：本句柄特意保持单线程设计（内部使用 `Rc<RefCell<_>>`），因为硬件总线本质上是串行设备，
/// 同一瞬间只能有一条 SCPI 报文在线缆上流动；并且嵌入的 Lua 宏运行时并非线程安全（非 `Send`）。
/// 多仪器并发通过在上层为每台设备拉起独立的进程或会话来实现。
pub struct Instrument {
    core: Rc<RefCell<Core>>,
    #[cfg(feature = "lua")]
    lua: Option<mlua::Lua>,
}

const MAX_RESPONSE: usize = 4 * 1024 * 1024;

pub(crate) struct Core {
    profile: Arc<Profile>,
    transport: Box<dyn Transport>,
    read_buf: Vec<u8>,
    pending_responses: usize,
}

impl Instrument {
    pub fn open(profile: Arc<Profile>, transport: Box<dyn Transport>) -> Result<Self> {
        let clear_on_open = profile.io.clear_input == ClearPolicy::OnOpen;
        let mut core = Core {
            profile: Arc::clone(&profile),
            transport,
            read_buf: Vec::new(),
            pending_responses: 0,
        };
        if clear_on_open {
            core.transport.clear()?;
        }
        let core = Rc::new(RefCell::new(core));
        #[cfg(feature = "lua")]
        {
            let lua = match profile.lua_script() {
                Some(script) => Some(crate::lua::create(Rc::clone(&core), script)?),
                None => None,
            };
            Ok(Self { core, lua })
        }
        #[cfg(not(feature = "lua"))]
        {
            let _ = profile;
            Ok(Self { core })
        }
    }

    pub fn profile(&self) -> Arc<Profile> {
        Arc::clone(&self.core.borrow().profile)
    }

    pub fn transport_description(&self) -> String {
        self.core.borrow().transport.description()
    }

    /// Read `*IDN?` and return the trimmed identification string.
    pub fn identify(&mut self) -> Result<String> {
        self.query_raw("*IDN?").map(|s| s.trim().to_string())
    }

    /// Check `*IDN?` against the profile's `idn_match` substrings.
    pub fn verify_identity(&mut self) -> Result<()> {
        let expected = self.profile().idn_match.clone();
        if expected.is_empty() {
            return Ok(());
        }
        let actual = self.identify()?;
        let matches = expected.iter().any(|needle| {
            actual
                .to_ascii_uppercase()
                .contains(&needle.to_ascii_uppercase())
        });
        if matches {
            Ok(())
        } else {
            Err(Error::UnexpectedInstrument { expected, actual })
        }
    }

    pub fn get(&mut self, id: &str) -> Result<Value> {
        self.get_reading(id).map(|reading| reading.value)
    }

    pub fn get_reading(&mut self, id: &str) -> Result<Reading> {
        self.with_core(|core| core.get_reading(id))
    }

    /// Query a command that takes positional query arguments.
    pub fn get_with_args(&mut self, id: &str, args: &[Value]) -> Result<Reading> {
        self.with_core(|core| core.get_with_args(id, args))
    }

    pub fn set(&mut self, id: &str, args: &[Value]) -> Result<()> {
        self.set_command(id, args).map(|_| ())
    }

    /// Set a command and return the exact SCPI message that was sent.
    pub fn set_command(&mut self, id: &str, args: &[Value]) -> Result<String> {
        self.with_core(|core| core.set_command(id, args))
    }

    pub fn write_raw(&mut self, scpi: &str) -> Result<()> {
        self.with_core(|core| core.write_raw(scpi))
    }

    /// Read one response that was not produced by a paired query.
    /// Returns `Ok(None)` when `read_mode` is `nonblocking` and no complete
    /// line is available. Rejected for `query_only` profiles.
    pub fn read_raw(&mut self) -> Result<Option<String>> {
        self.with_core(Core::read_raw)
    }

    /// Discard buffered input according to the profile's `clear_input` policy.
    pub fn clear_input(&mut self) -> Result<()> {
        self.with_core(Core::clear_input)
    }

    pub fn query_raw(&mut self, scpi: &str) -> Result<String> {
        self.with_core(|core| core.query_line(scpi))
    }

    /// Query and parse the response using the profile's declared result type
    /// when the header is known; otherwise return [`Value::Raw`].
    pub fn query_typed(&mut self, scpi: &str) -> Result<(Value, String)> {
        self.with_core(|core| core.query_typed(scpi))
    }

    /// Drain `error_query` (usually `:SYSTem:ERRor?`) until "No error".
    pub fn drain_errors(&mut self) -> Result<Vec<String>> {
        self.with_core(Core::drain_errors)
    }

    #[cfg(feature = "lua")]
    pub fn call(&mut self, name: &str, args: &[Value]) -> Result<Value> {
        let lua = self.lua.as_ref().ok_or_else(|| {
            Error::Lua(format!(
                "profile '{}' has no Lua script; cannot call '{name}'",
                self.core.borrow().profile.id
            ))
        })?;
        let function: mlua::Function = lua
            .globals()
            .get(name)
            .map_err(|e| Error::Lua(format!("Lua macro '{name}' is not defined: {e}")))?;
        let mut lua_args = Vec::with_capacity(args.len());
        for arg in args {
            lua_args.push(crate::lua::value_to_lua(lua, arg).map_err(lua_error)?);
        }
        let result: mlua::Value = function.call(lua_args).map_err(lua_error)?;
        crate::lua::lua_to_value(result)
    }

    #[cfg(not(feature = "lua"))]
    pub fn call(&mut self, _name: &str, _args: &[Value]) -> Result<Value> {
        Err(Error::FeatureDisabled("lua"))
    }

    fn with_core<T>(&self, f: impl FnOnce(&mut Core) -> Result<T>) -> Result<T> {
        let mut core = self
            .core
            .try_borrow_mut()
            .map_err(|_| Error::Message("instrument is busy (re-entrant call)".into()))?;
        f(&mut core)
    }
}

#[cfg(feature = "lua")]
fn lua_error(error: mlua::Error) -> Error {
    Error::Lua(error.to_string())
}

impl Core {
    pub(crate) fn write_line(&mut self, command: &str) -> Result<()> {
        if !self.profile.io.allow_pending_responses && self.pending_responses > 0 {
            return Err(Error::PendingResponse);
        }
        let mut message = Vec::with_capacity(command.len() + 1);
        message.extend_from_slice(command.as_bytes());
        message.extend_from_slice(self.profile.terminator.as_bytes());
        self.transport.write_all(&message)
    }

    pub(crate) fn read_line(&mut self) -> Result<String> {
        loop {
            if let Some(line) = self.take_line() {
                return Ok(line);
            }
            if self.read_buf.len() > MAX_RESPONSE {
                return Err(Error::Transport(format!(
                    "response exceeds {MAX_RESPONSE} bytes without a terminator"
                )));
            }
            let mut chunk = [0u8; 1024];
            let read = self.transport.read(&mut chunk)?;
            if read == 0 {
                return Err(Error::Transport(
                    "instrument closed the connection before a full response".into(),
                ));
            }
            self.read_buf.extend_from_slice(&chunk[..read]);
        }
    }

    fn take_line(&mut self) -> Option<String> {
        let pos = self.read_buf.iter().position(|byte| *byte == b'\n')?;
        let mut line: Vec<u8> = self.read_buf.drain(..=pos).collect();
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        Some(String::from_utf8_lossy(&line).into_owned())
    }

    /// Non-blocking counterpart of `read_line`: consume a complete buffered
    /// line, or read whatever the transport has and return `None` if it does
    /// not yet contain a terminator.
    fn read_line_available(&mut self) -> Result<Option<String>> {
        loop {
            if let Some(line) = self.take_line() {
                return Ok(Some(line));
            }
            if self.read_buf.len() > MAX_RESPONSE {
                return Err(Error::Transport(format!(
                    "response exceeds {MAX_RESPONSE} bytes without a terminator"
                )));
            }
            let mut chunk = [0u8; 1024];
            let read = self.transport.read_available(&mut chunk)?;
            if read == 0 {
                return Ok(None);
            }
            self.read_buf.extend_from_slice(&chunk[..read]);
        }
    }

    pub(crate) fn write_raw(&mut self, command: &str) -> Result<()> {
        let query = is_query_command(command);
        if query
            && (self.profile.io.read_mode == ReadMode::QueryOnly
                || !self.profile.io.allow_pending_responses)
        {
            return Err(Error::RawQueryForbidden {
                profile: self.profile.id.clone(),
            });
        }
        self.write_line(command)?;
        if query {
            self.pending_responses += 1;
        }
        Ok(())
    }

    pub(crate) fn read_raw(&mut self) -> Result<Option<String>> {
        if self.profile.io.read_mode == ReadMode::QueryOnly {
            return Err(Error::ReadForbidden {
                profile: self.profile.id.clone(),
            });
        }
        if !self.profile.io.allow_pending_responses && self.pending_responses == 0 {
            return Err(Error::NoPendingResponse);
        }
        let response = match self.profile.io.read_mode {
            ReadMode::Blocking => Some(self.read_line()?),
            ReadMode::NonBlocking => self.read_line_available()?,
            ReadMode::QueryOnly => unreachable!(),
        };
        if response.is_some() {
            self.pending_responses = self.pending_responses.saturating_sub(1);
        }
        Ok(response)
    }

    pub(crate) fn clear_input(&mut self) -> Result<()> {
        if self.profile.io.clear_input == ClearPolicy::Never {
            return Err(Error::ClearForbidden {
                profile: self.profile.id.clone(),
            });
        }
        self.transport.clear()?;
        self.read_buf.clear();
        self.pending_responses = 0;
        Ok(())
    }

    pub(crate) fn query_line(&mut self, command: &str) -> Result<String> {
        if self.pending_responses > 0 {
            return Err(Error::PendingResponse);
        }
        self.write_line(command)?;
        self.read_line()
    }

    pub(crate) fn get_reading(&mut self, id: &str) -> Result<Reading> {
        self.get_with_args(id, &[])
    }

    pub(crate) fn get_with_args(&mut self, id: &str, args: &[Value]) -> Result<Reading> {
        let command = self.require_command(id)?.clone();
        if !command.readable {
            return Err(Error::NotReadable { id: id.to_string() });
        }
        let query = command.format_query(args)?;
        let raw = self.query_line(&query)?;
        let value = command.parse_result(&raw)?;
        Ok(Reading {
            value,
            unit: command.result.as_ref().and_then(|spec| spec.unit.clone()),
            raw,
        })
    }

    pub(crate) fn set_command(&mut self, id: &str, args: &[Value]) -> Result<String> {
        let command = self.require_command(id)?.clone();
        // 硬件输出安全联锁（Safety Output）：
        // 当修改幅度、频率或输出偏置等可能产生危险电压瞬变的高危参数时，
        // 自动先在总线上将输出通道关闭（输出 off）-> 下发新参数 -> 成功后再重新开启输出（输出 on）。
        // 若新参数下发失败或被拒绝，输出端口将保持关闭状态，彻底杜绝瞬间过压烧毁待测板卡的隐患。
        if let Some(output_id) = command.safety_output.clone() {
            self.set_command_direct(&output_id, &[Value::Bool(false)])?;
            let result = self.set_command_direct(id, args);
            if result.is_ok() {
                self.set_command_direct(&output_id, &[Value::Bool(true)])?;
            }
            return result;
        }
        self.set_command_direct(id, args)
    }

    fn set_command_direct(&mut self, id: &str, args: &[Value]) -> Result<String> {
        let command = self.require_command(id)?.clone();
        let wire = command.format_set(args)?;
        self.write_line(&wire)?;
        Ok(wire)
    }

    pub(crate) fn query_typed(&mut self, scpi: &str) -> Result<(Value, String)> {
        let trimmed = scpi.trim();
        if !trimmed.ends_with('?') {
            return Err(Error::Message(format!(
                "'{trimmed}' is not a query (missing '?')"
            )));
        }
        let header = trimmed
            .split_whitespace()
            .next()
            .ok_or_else(|| Error::Message("empty query".into()))?;
        let pattern = HeaderPattern::parse(header)?;
        let command = self.profile.readable_command(&pattern).cloned();
        let raw = match &command {
            // Send the documented long form so the wire traffic is stable even
            // when the caller typed a short form.
            Some(command) => {
                let suffix = trimmed[header.len()..].trim();
                let mut wire = command.query_header();
                if !suffix.is_empty() {
                    wire.push(' ');
                    wire.push_str(suffix);
                }
                self.query_line(&wire)?
            }
            None => self.query_line(trimmed)?,
        };
        let value = match command {
            Some(command) => command.parse_result(&raw)?,
            None => Value::Raw(raw.clone()),
        };
        Ok((value, raw))
    }

    pub(crate) fn drain_errors(&mut self) -> Result<Vec<String>> {
        let id = self.profile.error_query.clone().ok_or_else(|| {
            Error::Message("profile does not declare an 'error_query' command".into())
        })?;
        let mut errors = Vec::new();
        for _ in 0..32 {
            let reading = self.get_reading(&id)?;
            let text = reading.value.display();
            if text.trim_start_matches('+').starts_with('0')
                || text.to_ascii_lowercase().contains("no error")
            {
                break;
            }
            errors.push(text);
        }
        Ok(errors)
    }

    fn require_command(&self, id: &str) -> Result<&Command> {
        self.profile
            .command(id)
            .ok_or_else(|| Error::UnknownCommand(id.to_string()))
    }
}

fn is_query_command(command: &str) -> bool {
    command
        .split_whitespace()
        .next()
        .is_some_and(|token| token.ends_with('?'))
}

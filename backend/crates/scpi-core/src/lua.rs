//! Lua extension host.
//!
//! A profile may ship a Lua file that defines macros. The script sees a global
//! `scpi` table with `write`, `query`, `get`, `set`, `errors`, `sleep`, and
//! `idn`. This keeps device- or series-specific sequencing out of Rust while
//! still routing every byte through the same typed engine.

#![cfg(feature = "lua")]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use mlua::{Lua, Table, Value as LuaValue, Variadic};

use crate::error::{Error, Result};
use crate::instrument::Core;
use crate::profile::LuaScript;
use crate::value::Value;

pub(crate) fn create(core: Rc<RefCell<Core>>, script: &LuaScript) -> Result<Lua> {
    let lua = Lua::new();
    let scpi = lua.create_table().map_err(lua_error)?;
    register(&lua, &scpi, core).map_err(lua_error)?;
    lua.globals().set("scpi", scpi).map_err(lua_error)?;
    lua.load(&script.source)
        .set_name(&script.name)
        .exec()
        .map_err(lua_error)?;
    Ok(lua)
}

fn register(lua: &Lua, scpi: &Table, core: Rc<RefCell<Core>>) -> mlua::Result<()> {
    let handle = Rc::clone(&core);
    scpi.set(
        "write",
        lua.create_function(move |_, command: String| {
            with_core(&handle, |core| core.write_raw(&command))
        })?,
    )?;

    let handle = Rc::clone(&core);
    scpi.set(
        "query",
        lua.create_function(move |_, command: String| {
            with_core(&handle, |core| core.query_line(&command))
        })?,
    )?;

    let handle = Rc::clone(&core);
    scpi.set(
        "read",
        lua.create_function(move |lua, ()| match with_core(&handle, Core::read_raw)? {
            Some(text) => Ok(LuaValue::String(lua.create_string(&text)?)),
            None => Ok(LuaValue::Nil),
        })?,
    )?;

    let handle = Rc::clone(&core);
    scpi.set(
        "clear",
        lua.create_function(move |_, ()| with_core(&handle, Core::clear_input))?,
    )?;

    let handle = Rc::clone(&core);
    scpi.set(
        "get",
        lua.create_function(move |lua, id: String| {
            let reading = with_core(&handle, |core| core.get_reading(&id))?;
            value_to_lua(lua, &reading.value)
        })?,
    )?;

    let handle = Rc::clone(&core);
    scpi.set(
        "set",
        lua.create_function(move |_, (id, args): (String, Variadic<LuaValue>)| {
            let values = args
                .into_iter()
                .map(lua_to_value)
                .collect::<Result<Vec<_>>>()
                .map_err(|error| mlua::Error::RuntimeError(error.to_string()))?;
            with_core(&handle, |core| core.set_command(&id, &values).map(|_| ()))
        })?,
    )?;

    let handle = Rc::clone(&core);
    scpi.set(
        "errors",
        lua.create_function(move |lua, ()| {
            let errors = with_core(&handle, Core::drain_errors)?;
            let table = lua.create_table()?;
            for (index, error) in errors.iter().enumerate() {
                table.set(index + 1, error.clone())?;
            }
            Ok(table)
        })?,
    )?;

    scpi.set(
        "sleep",
        lua.create_function(|_, millis: u64| {
            std::thread::sleep(Duration::from_millis(millis));
            Ok(())
        })?,
    )?;

    let handle = Rc::clone(&core);
    scpi.set(
        "idn",
        lua.create_function(move |_, ()| {
            with_core(&handle, |core| {
                core.query_line("*IDN?")
                    .map(|value| value.trim().to_string())
            })
        })?,
    )?;

    Ok(())
}

fn with_core<T>(
    core: &Rc<RefCell<Core>>,
    f: impl FnOnce(&mut Core) -> Result<T>,
) -> mlua::Result<T> {
    let mut core = core
        .try_borrow_mut()
        .map_err(|_| mlua::Error::RuntimeError("instrument is busy (re-entrant call)".into()))?;
    f(&mut core).map_err(|error| mlua::Error::RuntimeError(error.to_string()))
}

fn lua_error(error: mlua::Error) -> Error {
    Error::Lua(error.to_string())
}

pub(crate) fn value_to_lua(lua: &Lua, value: &Value) -> mlua::Result<LuaValue> {
    Ok(match value {
        Value::Bool(v) => LuaValue::Boolean(*v),
        Value::Int(v) => LuaValue::Integer(*v),
        Value::Float(v) => LuaValue::Number(*v),
        Value::String(v) | Value::Raw(v) => LuaValue::String(lua.create_string(v)?),
    })
}

pub(crate) fn lua_to_value(value: LuaValue) -> Result<Value> {
    match value {
        LuaValue::Nil => Ok(Value::Raw(String::new())),
        LuaValue::Boolean(v) => Ok(Value::Bool(v)),
        LuaValue::Integer(v) => Ok(Value::Int(v)),
        LuaValue::Number(v) => {
            if v.fract() == 0.0 && v >= i64::MIN as f64 && v <= i64::MAX as f64 {
                Ok(Value::Int(v as i64))
            } else {
                Ok(Value::Float(v))
            }
        }
        LuaValue::String(v) => Ok(Value::String(v.to_string_lossy().to_string())),
        other => Err(Error::Lua(format!(
            "Lua macro returned an unsupported {} value",
            lua_type_name(&other)
        ))),
    }
}

fn lua_type_name(value: &LuaValue) -> &'static str {
    match value {
        LuaValue::Nil => "nil",
        LuaValue::Boolean(_) => "boolean",
        LuaValue::LightUserData(_) => "light userdata",
        LuaValue::Integer(_) => "integer",
        LuaValue::Number(_) => "number",
        LuaValue::String(_) => "string",
        LuaValue::Table(_) => "table",
        LuaValue::Function(_) => "function",
        LuaValue::Thread(_) => "thread",
        LuaValue::UserData(_) => "userdata",
        LuaValue::Error(_) => "error",
        _ => "value",
    }
}

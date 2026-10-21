//! # `scpi-core` — 通用数据驱动 SCPI 仪器协议引擎
//!
//! Rust 负责底层最坚固的物理通信与协议约束：
//! - 规整的消息帧拼接与 SCPI 换行终止符控制；
//! - 严格的参数强类型检查、数值范围判定与特殊 Token 放行；
//! - 仪器返回数据的自动强类型反序列化；
//! - 硬件级安全联锁（如调节参数前关闭输出端口）；
//! - 错误队列轮询与 Keysight 等严苛设备的队列防死锁策略；
//! - 跨平台的底层传输驱动（基于 nusb 的免驱动 USBTMC、TCP Socket、Linux 字符设备与离线 Mock）。
//!
//! 所有具体的仪器业务指令完全交由 JSON Profile（与可选 Lua 宏）定义，
//! 新增实验仪器无需重新编译本引擎。

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

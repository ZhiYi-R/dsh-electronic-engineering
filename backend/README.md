# ee-fat-whale SCPI 后端执行器

<p align="center">
  <b>简体中文</b> | <a href="README_EN.md">English</a>
</p>

`backend/` 是 `ee-fat-whale` 的 Rust 后端，负责按 Profile JSON（及可选 Lua 脚本）执行 SCPI 命令。

引擎本身不包含任何具体仪器的命令定义。仪器支持的指令、参数类型与范围、是否需要 `safety_output` 输出联锁、以及 I/O 队列策略，全部由启动时传入的 `--profile <path>` 声明。新增仪器只需编写 Profile，不需要修改或重编译 Rust 代码。

---

## 结构与职责

```text
DSH 插件宿主（Node.js）
        │
        ▼ 子进程：scpi serve --profile <path> --transport <spec>
Rust 执行器（scpi-core / scpi-cli）
   ├── profile.rs    加载 Profile JSON，编译参数类型与量程约束
   ├── scpi.rs       SCPI 命令头解析，长短助记符匹配与规范化输出
   ├── instrument.rs 会话管理、*IDN? 校验、safety_output 联锁、错误队列轮询
   ├── discovery.rs  USB 与 LAN（mDNS / 子网扫描 / *IDN?）设备发现
   ├── lua.rs        可选的 Lua 5.4 宏运行时（Cargo feature: lua，默认开启）
   └── transport/    Mock / TCP Socket / LAN 探测 / nusb USBTMC / Linux /dev/usbtmc*
```

主要特性：
1. **无内置仪器逻辑**：除 `discover` 外，所有命令均需指定 `--profile`。
2. **参数校验与结果解析**：写入前按声明的类型和 `min`/`max` 校验参数；查询响应按声明类型解析为浮点数、整数、布尔值、枚举或字符串。
3. **SCPI 长短格式匹配**：支持匹配简写（`:meas:volt:dc?`）或全称（`:MEASure:VOLTage:DC?`），发往总线时统一输出 Profile 中声明的标准长格式。
4. **Lua 可选编译**：默认启用 `lua` feature；可通过 `--no-default-features` 编译仅依赖 JSON 的二进制。

---

## 本地无硬件测试（Mock 模式）

可以直接使用仓库自带的 Profile 与 Mock 数据在本地验证：

```bash
cd backend
cargo build --release

# 解压内置的 DM3058 仪器包
TEMP_DIR=$(mktemp -d)
tar -xzf ../bundles/dm3058.tar.gz -C "$TEMP_DIR"
PROFILE="$TEMP_DIR/dm3058.json"

# 查看仪器信息与 *IDN?
./target/release/scpi \
  --profile "$PROFILE" \
  --transport mock:../test/fixtures/dm3058.mock.json \
  info

# 类型化查询（读取直流电压）
./target/release/scpi \
  --profile "$PROFILE" \
  --transport mock:../test/fixtures/dm3058.mock.json \
  get meas.vdc

# 类型化写入（设置直流电压量程挡位）
./target/release/scpi \
  --profile "$PROFILE" \
  --transport mock:../test/fixtures/dm3058.mock.json \
  set meas.vdc.range 2

# 调用 Lua 宏
./target/release/scpi \
  --profile "$PROFILE" \
  --transport mock:../test/fixtures/dm3058.mock.json \
  call measure

# 发送原始 SCPI 查询并按 Profile 解析结果
./target/release/scpi \
  --profile "$PROFILE" \
  --transport mock:../test/fixtures/dm3058.mock.json \
  query ':meas:volt:dc?'
```

连接真实 USB 仪器（如 Rigol DM3058，VID:PID `1ab1:09c4`）：

```bash
./target/release/scpi --profile "$PROFILE" --transport usb:1ab1:09c4 info
```

---

## CLI 子命令

| 子命令 | 说明 |
| --- | --- |
| `scpi info` | 打印 Profile 信息、传输层描述、`*IDN?` 响应及命令总数 |
| `scpi list [filter]` | 列出 Profile 中的命令（可按 id、header、summary 过滤） |
| `scpi describe <id>` | 以 JSON 格式输出单条命令的定义 |
| `scpi get <id> [args]` | 执行类型化查询，输出解析后的值、单位和原始响应 |
| `scpi set <id> <args...>` | 校验参数并发送类型化写入命令，返回实际发送的 SCPI 文本 |
| `scpi errors` | 轮询 `error_query`（如 `:SYSTem:ERRor?`）直到无错误 |
| `scpi query '<scpi>'` | 发送原始 SCPI 查询；若命令头在 Profile 中有定义则自动解析类型 |
| `scpi write '<scpi>'` | 发送原始 SCPI 写入命令（拒绝带 `?` 的查询命令） |
| `scpi call <macro> [args]` | 调用 Profile 附带的 Lua 宏函数 |
| `scpi discover [options]` | 扫描 USBTMC 设备；加 `--lan` 同时通过 mDNS 与 TCP 子网扫描发现局域网仪器（无需 `--profile`） |
| `scpi serve` | 启动基于 stdin/stdout 的 JSON-lines 守护进程 |
| `scpi repl` | 交互式调试命令行 |

---

## 传输层格式

| 传输规范 | 实现 | 说明 |
| --- | --- | --- |
| `usb:<vid>:<pid>[:<serial>]` | `nusb` 原生 USBTMC | 跨平台直接读写 USB 端点，无需 VISA |
| `tcp://192.168.1.20:5555` | TCP Socket | 适用于网口仪器（端口依手册而定，常见 5555 或 5025） |
| `usbtmc:/dev/usbtmc0` | Linux 字符设备 | 使用 Linux 内核 `usbtmc` 驱动模块 |
| `mock` | 内存空桩 | 写入均成功，查询返回空默认值 |
| `mock:<fixtures.json>` | 脚本化 Mock | 按预设 JSON 返回查询响应 |

脚本化 Mock 文件示例：

```json
{
  "fallback": "+0",
  "responses": {
    "*IDN?": "RIGOL TECHNOLOGIES,DM3058E,DM3R000000001,01.01",
    ":MEASure:VOLTage:DC?": ["+1.2345E-01", "+1.2348E-01"],
    ":SYSTem:ERRor?": ["-113,\"Undefined header\"", "+0,\"No error\""]
  }
}
```

数组形式的响应按查询次数依次消费，到达末尾后重复返回最后一项。

---

## 读取模式与 I/O 队列策略（`io`）

不同仪器的 I/O 队列行为差异较大。例如 Keysight 33600A：
1. **不能在无待读查询时调用 `read()`**：必须先发带 `?` 的查询再读响应，单独调用 `read()` 会一直阻塞到超时。
2. **不能积压未读响应**：上一条查询的响应未读完就发下一条命令，硬件会报 `-410, "Query INTERRUPTED"`。
3. **打开会话时不宜盲目清缓冲**：随意清空可能打断正在运行的扫频或序列。

这些约束在 Profile 的 `io` 字段中配置：

```json
"io": {
  "read_mode": "query_only",
  "allow_pending_responses": false,
  "clear_input": "never"
}
```

| 字段 | 可选值 | 含义 |
| --- | --- | --- |
| `read_mode` | `query_only` | 禁止单独调用 `read()`，仅允许配对查询（Keysight 33600A 使用此模式） |
| | `blocking` | 独立 `read()` 阻塞等待直到读到换行符或超时 |
| | `nonblocking` | 独立 `read()` 非阻塞返回：有完整行则返回，否则立即返回 `None` |
| `allow_pending_responses` | `false` | 存在未消费响应时拒绝新命令，并禁止通过 `write` 发送带 `?` 的查询 |
| | `true` | 允许先通过 `write` 发送查询，稍后用 `read` 读取响应 |
| `clear_input` | `never` | 禁止调用 `clear-input` |
| | `manual` | 允许手动调用 `clear-input`，但打开会话时不自动清空 |
| | `on_open` | 打开会话时清空一次输入缓冲，之后允许手动清空 |

未指定 `io` 时，默认为 `blocking`、`allow_pending_responses: true`、`clear_input: "manual"`。加载 Profile 时会拒绝相互冲突的组合（如 `query_only` 搭配 `allow_pending_responses: true` 或 `on_open`）。

---

## `scpi serve` JSON-lines 协议

宿主通过 stdin/stdout 与 `scpi serve` 交换单行 JSON。

请求示例（stdin）：

```json
{"id": 1, "op": "get", "path": "meas.vdc"}
{"id": 2, "op": "set", "path": "rate.dc_voltage", "args": ["F"]}
{"id": 3, "op": "query", "scpi": ":SYSTem:ERRor?"}
{"id": 4, "op": "call", "name": "measure", "args": []}
{"id": 5, "op": "shutdown"}
```

响应示例（stdout）：

```json
{"id": 1, "ok": true, "path": "meas.vdc", "value": 0.12345, "unit": "V", "raw": "+1.234500E-01"}
{"id": 2, "ok": true, "path": "rate.dc_voltage", "scpi": ":RATE:VOLTage:DC F"}
{"id": 1, "ok": false, "error": {"kind": "invalid_argument", "message": "value 15.0 exceeds max 10.0"}}
```

支持的操作：`ping`、`info`、`commands`、`describe`、`get`、`set`、`errors`、`read`、`clear`、`query`、`write`、`call`、`shutdown`。

---

## Profile 与 Lua 编写说明

### 单条命令示例

```json
{
  "id": "meas.vdc.range",
  "header": ":MEASure:VOLTage:DC",
  "query_header": ":MEASure:VOLTage:DC:RANGe",
  "summary": "DC voltage range (0=200mV .. 4=1000V)",
  "writable": true,
  "readable": true,
  "args": [
    {
      "name": "range",
      "type": "int",
      "min": 0,
      "max": 4,
      "special": ["MIN", "MAX", "DEF"]
    }
  ],
  "result": "int",
  "safety_output": "ch1.output"
}
```

- `header` / `query_header`：写入头与查询头（查询头自动追加 `?`）。
- `args`：支持 `int`、`float`、`bool`、`string`、`enum`、`any`，可设 `min`、`max`、`values`、`special`、`quoted`、`unit`。
- `safety_output`：引用同 Profile 下的布尔写入命令（如 `ch1.output`），写入前先置 `false`，写入成功后再置 `true`。
- `requires`：依赖的硬件选件列表。

### Lua 宏接口

当 Profile 声明 `"lua": "script.lua"` 时，脚本可通过全局 `scpi` 表调用：
`scpi.write(cmd)`、`scpi.query(cmd)`、`scpi.read()`、`scpi.clear()`、`scpi.get(id)`、`scpi.set(id, ...)`、`scpi.errors()`、`scpi.sleep(ms)`、`scpi.idn()`。

```lua
function measure()
  local mode = scpi.query(":FUNCtion?")
  if mode:find("VOLT") then
    return tonumber(scpi.query(":MEASure:VOLTage:DC?"))
  else
    return tonumber(scpi.query(":MEASure:CURRent:DC?"))
  end
end
```

---

## 运行测试

```bash
cargo test
cargo clippy --all-targets
cargo test -p scpi-core --no-default-features
```

---

## 许可证

遵循 **GNU Affero General Public License v3.0 (AGPL-3.0-or-later)**，详见 [../LICENSE](../LICENSE)。

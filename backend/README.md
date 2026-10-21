# ee-fat-whale SCPI 后端执行器

> “让懂硬件的人专注写配置，让通用引擎专注稳通信与防翻车。”

这里是 `ee-fat-whale` 的底层核心引擎，使用 Rust 编写。它是一个**纯粹数据驱动的通用 SCPI 执行器（Executor）**。

在设计之初，我们就确立了一条极其严格的工程边界：**Rust 引擎内部绝不硬编码任何具体仪器的私有指令。**
所有仪器的脾气秉性（支持哪些 SCPI 指令、参数范围多少、是否需要关闭输出联锁、读取时是一问一答还是允许缓冲）全部由外部的 **Profile JSON（以及可选的 Lua 宏）** 声明。

因此，当你需要为实验室接入一台全新的示波器或频谱仪时，**你完全不需要动 Rust 代码、不需要重新编译二进制文件**，只需按照规范写好一份仪器档案即可直接跑起来。

---

## 架构分层与设计哲学

```text
DSH 插件宿主（Node.js / 界面 / 会话调度 / LLM 工具分发）
        │
        ▼ 派生子进程：scpi serve --profile <path> --transport <spec>
Rust 通用执行器（scpi-core / scpi-cli）
   ├── Profile 引擎：解析 JSON 档案，载入类型定义与边界规则（可选 Lua 宏运行时）
   ├── SCPI 协议状态机：长短格式识别、参数拼装、响应按类型解算、严格双层范围校验
   ├── 硬件安全与队列：*IDN? 握手、硬件输出安全联锁、错误队列轮询与防挂死策略
   └── 传输层抽象：Mock（离线演练）/ TCP（以太网）/ nusb（跨平台 USBTMC）/ Linux 设备节点
        │
        ▼ USB 线缆 / 网线 / 虚拟管道
实验室真实仪器 / 仿真桩
```

我们遵循以下几个核心设计原则：

1. **Rust 侧零仪器私有代码**：通用引擎只负责实现通用的通信机制、严格的类型检验与错误处理。增加新设备只需增加 Profile。
2. **插件侧零 SCPI 协议逻辑**：JavaScript 上层完全不解析和拼装原始 SCPI 字符串，它只把大模型的意图以结构化的 JSON-lines 形式下发给执行器。
3. **强类型与智能归一化**：
   - 参数在发给仪器前被严格校验（如电压限定在 `0.001..10.0`），避免越界操作打坏设备。
   - 响应内容会被自动解析为强类型（浮点数、整数、布尔、枚举或字符串），不仅解析数值，还能剥离或匹配单位。
4. **SCPI 长短格式自适应识别**：
   - 仪器手册中通常将缩写大写、全拼小写（例如 `:MEASure:VOLTage:DC?`）。
   - 无论你或脚本输入 `:meas:volt:dc?`、`:MEAS:VOLT:DC?` 还是 `:MEASure:VOLTage:DC?`，引擎都能智能归一化识别，并在通信总线上统一输出规范的长格式。
5. **极简轻量，Lua 可选配**：Lua 运行时完全作为 Cargo feature（默认开启）。如果在极致精简的环境下，可以通过 `--no-default-features` 编译出一个完全无脚本依赖的纯 JSON 静态二进制。

---

## 目录结构

```text
backend/
├── Cargo.toml                 # Cargo Workspace 配置
├── crates/
│   ├── scpi-core/             # 核心库：协议解析、传输驱动与硬件抽象
│   │   ├── src/profile.rs     # JSON 档案载入、类型系统、参数校验与命令匹配
│   │   ├── src/scpi.rs        # SCPI 命令头语法树、长短格式双向映射匹配
│   │   ├── src/instrument.rs  # 单台仪器高层会话：读写调度、错误队列管理与安全联锁
│   │   ├── src/lua.rs         # 可选的 Lua 5.4 宏运行时（支持高级复合序列）
│   │   ├── src/value.rs       # 强类型值系统（支持科学计数法、特殊 token）
│   │   ├── src/error.rs       # 细粒度错误类型定义
│   │   └── src/transport/     # 硬件传输层实现（nusb / TCP / Linux usbtmc / Mock）
│   └── scpi-cli/              # CLI 命令行工具与 JSON-lines 守护进程
│       └── src/main.rs        # scpi 命令行工具实现
└── (注意：生产仪器包统一存放在仓库根目录 bundles/ 下)
```

---

## 快速上手（无需硬件，纯本地 Mock 体验）

你手头现在没有示波器或万用表？完全没关系！引擎内置了完备的 Mock 机制和测试用例，你可以立即体验：

```bash
# 1. 编译 release 版本二进制
cd backend
cargo build --release

# 2. 解压随附的 DM3058 万用表档案到临时目录
TEMP_DIR=$(mktemp -d)
tar -xzf ../bundles/dm3058.tar.gz -C "$TEMP_DIR"
PROFILE="$TEMP_DIR/dm3058.json"

# 3. 查看仪器基本信息（结合 mock fixture 模拟返回 *IDN?）
./target/release/scpi \
  --profile "$PROFILE" \
  --transport mock:../test/fixtures/dm3058.mock.json \
  info

# 4. 执行类型化查询（读取直流电压）
./target/release/scpi \
  --profile "$PROFILE" \
  --transport mock:../test/fixtures/dm3058.mock.json \
  get meas.vdc

# 5. 执行类型化写入（设定电压量程为 2V）
./target/release/scpi \
  --profile "$PROFILE" \
  --transport mock:../test/fixtures/dm3058.mock.json \
  set meas.vdc.range 2

# 6. 调用 Lua 宏脚本（先查询当前挡位，再执行对应测量）
./target/release/scpi \
  --profile "$PROFILE" \
  --transport mock:../test/fixtures/dm3058.mock.json \
  call measure

# 7. 发送原始 SCPI 查询并让引擎按档案解析响应
./target/release/scpi \
  --profile "$PROFILE" \
  --transport mock:../test/fixtures/dm3058.mock.json \
  query ':meas:volt:dc?'
```

**连接真实硬件：**
如果你手头有一台通过 USB 连接的真实 Rigol DM3058（VID:PID 为 `1ab1:09c4`），只需要将 transport 换成跨平台的 nusb 规范：

```bash
./target/release/scpi --profile "$PROFILE" --transport usb:1ab1:09c4 info
```

---

## 常用 CLI 子命令清单

`scpi` CLI 工具既是上层插件调用的后台内核，也是工程师排查硬件问题的趁手命令行工具：

| 子命令 | 功能描述与典型用途 |
| --- | --- |
| `scpi info` | 打印当前 Profile、通信传输类型、硬件 `*IDN?` 识别串及指令统计 |
| `scpi list [filter]` | 列出当前 Profile 支持的所有指令（支持按 ID、命令头或描述模糊过滤） |
| `scpi describe <id>` | 查看某条具体指令的完整元数据（包含参数类型、范围限制、安全联锁等） |
| `scpi get <id> [args]` | 执行类型化查询，输出包含数值、单位和原始字符串的结构化 JSON |
| `scpi set <id> <args...>` | 执行类型化写入，会自动做范围校验，并打印实际发往硬件的总线文本 |
| `scpi errors` | 轮询清空并打印仪器的硬件错误队列（对应 `:SYSTem:ERRor?`） |
| `scpi query '<scpi>'` | 发送原始 SCPI 查询指令（如果匹配档案则自动应用类型解析） |
| `scpi write '<scpi>'` | 发送原始 SCPI 写入指令（引擎会拒绝带有 `?` 的查询指令以防队列错乱） |
| `scpi call <macro> [args]`| 执行 Profile 中内置的 Lua 复合宏脚本 |
| `scpi discover` | 快速扫描当前系统所有 USB 端口上的 USBTMC 设备（无需指定 profile） |
| `scpi serve` | 启动 JSON-lines 后台服务模式（通过标准输入输出与插件长连接） |
| `scpi repl` | 进入交互式控制台，方便在终端中逐条调试硬件 |

> **提示**：除了 `discover` 命令之外，所有操作都需要显式传入 `--profile <path>` 参数。Rust 执行器本身对设备“一无所知”，必须由档案告诉它怎么做。

---

## 传输层驱动说明

| 传输规范（Transport Spec） | 驱动实现 | 适用环境与注意事项 |
| --- | --- | --- |
| `usb:<vid>:<pid>[:<serial>]` | 基于 nusb 的原生 USBTMC | **跨平台推荐**。在 Linux/macOS/Windows 上直接读写 USB 描述符，无需安装 VISA 运行时。 |
| `tcp://192.168.1.20:5555` | 原生 TCP Socket 套接字 | 适用于网口直连或局域网仪器。端口号请按仪器厂商手册指定（如 5555 或 5025）。 |
| `usbtmc:/dev/usbtmc0` | Linux 内核字符设备 | 适用于启用了 Linux 原生内核 `usbtmc` 驱动模块的系统。 |
| `mock` | 纯内存静默桩 | 任何写入均成功，任何查询返回空默认值。 |
| `mock:<fixtures.json>` | 脚本化响应文件桩 | 预设每次查询的具体返回内容，供自动化测试和演示使用。 |

**测试用脚本化 Mock 格式示例：**
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
*（数组形式表示按查询顺序逐次返回，读到最后一项后持续返回最后一项）*

---

## 关键工程细节：读取机制与硬件队列策略

在实际实验室硬件控制中，**不同品牌、甚至同一品牌不同型号的仪器，其 I/O 队列的脾气差异极大**。
这是导致大多数简陋 SCPI 库频繁挂死或报超时的罪魁祸首。

典型案例就是 **Keysight (Agilent) 33600A 波形发生器** 与普通国产仪器的区别：
1. **绝不允许投机读取**：33600A 必须严格遵循“写带 `?` 指令 → 紧接着读一条响应”的配对模式。如果你在没有下发查询时贸然调用 `read()`，仪器在硬件层面根本没有数据可发，总线会直接卡死直到超时。
2. **绝不允许响应积压在缓冲区**：如果上一条查询的响应尚未读出，你就下发了下一条指令，33600A 会立刻在硬件层面报 `-410, "Query INTERRUPTED"` 错误并重置状态。
3. **不能在建立连接时随意清空输入缓冲**：盲目向某些仪器发送总线 Clear 可能会打断正在运行的扫频或突发输出。

为了彻底驯服不同硬件的脾气，我们将这些行为沉淀到了档案的 `io` 策略段中，由底层执行器强制执行：

```json
"io": {
  "read_mode": "query_only",
  "allow_pending_responses": false,
  "clear_input": "never"
}
```

### 策略字段详解

| 配置项 | 可选值 | 工程含义与硬件考量 |
| --- | --- | --- |
| `read_mode` | `query_only` | **严格配对模式**：禁止独立 `read` 操作。所有查询必须走配对的 query 通道。**Keysight 33600A 必须设为该模式**。 |
| | `blocking` | 独立 `read` 会阻塞等待直到读到完整的换行符响应或超时。 |
| | `nonblocking` | 独立 `read` 立即返回：有完整行就返回数据，没有立即返回 `None`。适合 Rigol 万用表。 |
| `allow_pending_responses` | `false` | **严禁滞留响应**：严格一问一答。如果还有未消费的响应，拒绝下发下一条查询；同时禁止使用原始 `write` 下发带 `?` 的指令。 |
| | `true` | 允许通过 `write` 发送查询后暂留响应，稍后显式调用 `read` 消费。 |
| `clear_input` | `never` | **禁止清空输入**：禁止任何清缓冲动作，调用 `clear-input` 会被直接拒绝。 |
| | `manual` | 允许显式手动调用 `clear-input`，但连接建立时不自动清空。 |
| | `on_open` | 刚打开物理会话时自动清空一次历史残留数据，之后允许手动清空。 |

**默认行为**：如果档案未填写 `io` 段，系统默认采用宽容策略（`blocking` + `allow_pending_responses: true` + `manual`）。同时校验器会自动拦截相互矛盾的配置（例如配置了 `query_only` 却试图设置 `allow_pending_responses: true`）。

---

## JSON-lines 通信协议（`scpi serve`）

当上层插件以子进程形式启动 `scpi serve` 时，双方通过标准输入输出以单行 JSON 格式高效通信。

### 请求格式（输入 stdin，一行一个 JSON）

```json
{"id": 1, "op": "get", "path": "meas.vdc"}
{"id": 2, "op": "set", "path": "rate.dc_voltage", "args": ["F"]}
{"id": 3, "op": "query", "scpi": ":SYSTem:ERRor?"}
{"id": 4, "op": "call", "name": "measure", "args": []}
{"id": 5, "op": "shutdown"}
```

### 响应格式（输出 stdout，一行一个 JSON）

成功响应：
```json
{"id": 1, "ok": true, "path": "meas.vdc", "value": 0.12345, "unit": "V", "raw": "+1.234500E-01"}
{"id": 2, "ok": true, "path": "rate.dc_voltage", "scpi": ":RATE:VOLTage:DC F"}
```

失败响应：
```json
{"id": 1, "ok": false, "error": {"kind": "invalid_argument", "message": "value 15.0 exceeds max 10.0"}}
```

支持的标准操作：`ping`、`info`、`commands`、`get`、`set`、`errors`、`read`、`clear`、`query`、`write`、`call`、`shutdown`。

---

## Profile JSON 档案编写规范

一份规范的 Profile JSON 文件是连接仪器与大模型的唯一媒介。

### 核心顶层字段

- `schema_version`: 当前固定为 `1`。
- `id` / `name`: 唯一英文标识与友好的显示名称（如 `rigol.dm3058` / `Rigol DM3058`）。
- `idn_match`: 数组，仪器在响应 `*IDN?` 时必须包含的子串（忽略大小写），用于防呆防插错。
- `usb`: 可选对象，形如 `{"vid": "1ab1", "pid": "09c4"}`，供总线扫描时快速比对。
- `terminator`: 换行终止符，默认为 `"\n"`。
- `timeout_ms`: 仪器通讯超时（毫秒），默认为 `10000`。
- `io`: 仪器读取与队列策略（见前节详细说明）。
- `channels`: 硬件模拟通道数量（0 表示无通道概念如万用表，1 表示单通道，>1 表示多通道，将触发智能通道折叠）。
- `commands`: 具体的指令定义数组。

### 单条命令（Command）配置项

```json
{
  "id": "meas.vdc.range",
  "header": ":MEASure:VOLTage:DC",
  "query_header": ":MEASure:VOLTage:DC:RANGe",
  "summary": "直流电压测量量程挡位 (0=200mV .. 4=1000V)",
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

- **`header` 与 `query_header`**：`header` 是写入时的标准 SCPI 头；查询时默认直接在 `header` 末尾加 `?`。如果某仪器读写命令不对称（如 DM3058 读量程要用 `:MEAS:VOLT:DC:RANGe?`），可以使用 `query_header` 明确指定查询命令头。
- **`args`**：参数列表，支持 `int`、`float`、`bool`、`string`、`enum`、`any`。支持指定 `min`、`max` 和 `special`（如放行 `MIN`、`MAX` 等 SCPI 保留字）。
- **`safety_output`**：极为重要的安全联锁。指定一个布尔输出命令（例如 `ch1.output`）。在执行本命令写入前，引擎会自动先关掉该通道输出，写入完成并确认无误后再重新打开，防止危险毛刺打坏负载。
- **`requires`**：硬件选件依赖数组（例如 `["AWG"]`）。如果仪器实际探测未开通该选件，该指令将被自动屏蔽。

---

## Lua 宏运行时说明

当需要根据仪器当前状态执行多步判断时（例如“先读取当前是在测电流还是测电压，再根据挡位决定采集指令”），可以使用 Lua 宏。

Profile 中通过 `"lua": "script.lua"` 引入同级脚本。引擎向 Lua 环境注入了线程安全的 `scpi` 全局表：

```lua
-- dm3058.lua 中的复合读取宏
function measure()
  local mode = scpi.query(":FUNCtion?")
  if mode:find("VOLT") then
    return tonumber(scpi.query(":MEASure:VOLTage:DC?"))
  else
    return tonumber(scpi.query(":MEASure:CURRent:DC?"))
  end
end
```

支持的 Lua API 包含：`scpi.write(cmd)`、`scpi.query(cmd)`、`scpi.get(id)`、`scpi.set(id, ...)`、`scpi.errors()`、`scpi.sleep(ms)`、`scpi.idn()`。

---

## 怎么给实验室新增一台仪器？（五步流程）

1. **准备模板**：解压 `bundles/dm3058.tar.gz` 或 `bundles/dg1022z.tar.gz`，复制其 JSON 骨架。
2. **填写设备信息**：修改 `id`、`name`、`idn_match`、`usb.vid` / `usb.pid` 以及通信端口。
3. **查阅手册填写常用指令**：先挑日常最核心的几条命令（比如频率、幅度、通道开关、测量读取），按照规范补充 `args` 类型和量程限制。
4. **打包与导入**：把 JSON（和可选的 Lua）打成 `.tar.gz` 或 `.zip`，在 DSH 插件设置页点击导入。
5. **离线与实机验证**：
   - 先用 `scpi list` 检查命令是否能正确载入。
   - 连上物理硬件后执行 `scpi info`，看到 `*IDN?` 握手成功即大功告成！

---

## 自动化测试与质量保障

我们构建了多层测试用例保障通信与状态机零死锁：

```bash
cargo test
cargo clippy --all-targets
cargo test -p scpi-core --no-default-features
```

测试覆盖了长短命令头模糊匹配、数值精度解算、布尔/枚举合法性验证、Keysight 查询配对保护、安全联锁自动状态切换等所有关键路径。

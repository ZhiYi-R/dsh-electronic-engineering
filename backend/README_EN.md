# ee-fat-whale SCPI Backend Executor

<p align="center">
  <a href="README.md">简体中文</a> | <b>English</b>
</p>

`backend/` contains the Rust SCPI engine and CLI for `ee-fat-whale`.

No instrument-specific commands are hardcoded in Rust. Supported SCPI headers, parameter types and bounds, `safety_output` interlocks, and I/O queue rules are all loaded at runtime from a Profile JSON file (plus an optional Lua script) via `--profile <path>`. Adding a new instrument only requires writing a profile.

---

## Architecture

```text
DSH Plugin Host (Node.js)
        │
        ▼ Subprocess: scpi serve --profile <path> --transport <spec>
Rust Executor (scpi-core / scpi-cli)
   ├── profile.rs    Loads Profile JSON, compiles types and range bounds
   ├── scpi.rs       SCPI header parser, short/long mnemonic matching
   ├── instrument.rs Session state, *IDN? check, safety_output interlocks, error queue
   ├── lua.rs        Optional Lua 5.4 runtime (Cargo feature: lua, enabled by default)
   └── transport/    Mock / TCP Socket / nusb USBTMC / Linux /dev/usbtmc*
```

Design notes:
1. **No built-in instrument profiles**: Every command except `discover` requires `--profile`.
2. **Typed arguments and responses**: Outgoing arguments are checked against declared types and `min`/`max` bounds; incoming responses are parsed into floats, integers, booleans, enums, or strings.
3. **SCPI short/long mnemonic matching**: Accepts `:meas:volt:dc?`, `:MEAS:VOLT:DC?`, or `:MEASure:VOLTage:DC?` and always writes the canonical long form from the profile to the bus.
4. **Optional Lua**: Build with `--no-default-features` to strip `mlua` and produce a JSON-only binary.

---

## Quickstart (Mock Transport)

You can run the CLI against the bundled profiles and mock fixtures without hardware:

```bash
cd backend
cargo build --release

# Extract the bundled DM3058 profile
TEMP_DIR=$(mktemp -d)
tar -xzf ../bundles/dm3058.tar.gz -C "$TEMP_DIR"
PROFILE="$TEMP_DIR/dm3058.json"

# Inspect profile info and *IDN?
./target/release/scpi \
  --profile "$PROFILE" \
  --transport mock:../test/fixtures/dm3058.mock.json \
  info

# Typed query (DC voltage)
./target/release/scpi \
  --profile "$PROFILE" \
  --transport mock:../test/fixtures/dm3058.mock.json \
  get meas.vdc

# Typed write (set voltage range to index 2)
./target/release/scpi \
  --profile "$PROFILE" \
  --transport mock:../test/fixtures/dm3058.mock.json \
  set meas.vdc.range 2

# Call a Lua macro
./target/release/scpi \
  --profile "$PROFILE" \
  --transport mock:../test/fixtures/dm3058.mock.json \
  call measure

# Send a raw SCPI query and parse the response via the profile
./target/release/scpi \
  --profile "$PROFILE" \
  --transport mock:../test/fixtures/dm3058.mock.json \
  query ':meas:volt:dc?'
```

To talk to a USB-connected Rigol DM3058 (`1ab1:09c4`):

```bash
./target/release/scpi --profile "$PROFILE" --transport usb:1ab1:09c4 info
```

---

## CLI Subcommands

| Command | Description |
| --- | --- |
| `scpi info` | Print profile metadata, transport description, `*IDN?`, and command count |
| `scpi list [filter]` | List commands in the profile (case-insensitive filter on id/header/summary) |
| `scpi describe <id>` | Print a command definition as JSON |
| `scpi get <id> [args]` | Run a typed query and print value, unit, and raw response |
| `scpi set <id> <args...>` | Validate arguments and send a typed write command |
| `scpi errors` | Drain the instrument error queue (`:SYSTem:ERRor?`) |
| `scpi query '<scpi>'` | Send a raw SCPI query and parse the result if the header is in the profile |
| `scpi write '<scpi>'` | Send a raw SCPI write command (rejects queries ending with `?`) |
| `scpi call <macro> [args]` | Call a Lua macro function defined by the profile |
| `scpi discover` | List USBTMC devices on the USB bus (no `--profile` needed) |
| `scpi serve` | Run the newline-delimited JSON stdio server |
| `scpi repl` | Interactive prompt for manual testing |

---

## I/O and Queue Policies (`io`)

Instruments differ in how their I/O buffers behave. For example, on the Keysight 33600A:
1. Calling `read()` without a preceding query hangs until transport timeout.
2. Sending a new command while a query response is still unread triggers `-410, "Query INTERRUPTED"`.
3. Clearing buffers on session open can interrupt an active sweep or burst.

Profiles declare these rules in the `io` section:

```json
"io": {
  "read_mode": "query_only",
  "allow_pending_responses": false,
  "clear_input": "never"
}
```

| Field | Values | Meaning |
| --- | --- | --- |
| `read_mode` | `query_only` | Reject standalone `read()`; allow paired queries only (Keysight 33600A). |
| | `blocking` | Standalone `read()` blocks until a newline or timeout. |
| | `nonblocking` | Standalone `read()` returns buffered line or `None` immediately. |
| `allow_pending_responses` | `false` | Require queries to be consumed immediately; reject raw query writes. |
| | `true` | Allow raw query writes to leave responses for a subsequent `read()`. |
| `clear_input` | `never` | Reject `clear-input`. |
| | `manual` | Allow explicit `clear-input`, do not clear on open. |
| | `on_open` | Clear once on session open and allow explicit clears later. |

---

## JSON-lines Protocol (`scpi serve`)

The plugin host talks to `scpi serve` over stdin/stdout using one JSON object per line.

Request (stdin):
```json
{"id": 1, "op": "get", "path": "meas.vdc"}
{"id": 2, "op": "set", "path": "rate.dc_voltage", "args": ["F"]}
{"id": 3, "op": "shutdown"}
```

Response (stdout):
```json
{"id": 1, "ok": true, "path": "meas.vdc", "value": 0.12345, "unit": "V", "raw": "+1.234500E-01"}
{"id": 2, "ok": true, "path": "rate.dc_voltage", "scpi": ":RATE:VOLTage:DC F"}
```

Supported `op` values: `ping`, `info`, `commands`, `describe`, `get`, `set`, `errors`, `read`, `clear`, `query`, `write`, `call`, `shutdown`.

---

## License

Licensed under the **GNU Affero General Public License v3.0 (AGPL-3.0-or-later)**. See [../LICENSE](../LICENSE) for details.

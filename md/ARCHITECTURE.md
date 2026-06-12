# Architecture

## Data Flow

```
  Vehicle Hardware
       │  UART (230400 baud)
       ▼
  serial.Serial / SimFromLog / SimSerial
       │  raw bytes
       ▼
┌─────────────────────────────────────┐
│  Decoder                            │
│  ┌──────────────────────────────┐   │
│  │  parse_byte()  state machine │   │
│  │  SOF→ID→DLC→DATA→EOF→VALID  │   │
│  └──────────────┬───────────────┘   │
│                 │ datagram dict      │
│  decode_datagram()                  │
│    ← global_can.yaml (ID lookup)    │
│    → bit-mask each signal           │
└──────┬──────────────────────────────┘
       │  signal samples
       ├──────────────────────────────► SQLite  (data/decoded_data.sqlite)
       │                                         ▲
       │  (--dbw flag only)                      │ poll every 250ms
       └──────────────────────────────► InfluxDB  can_signal_viewer.py
                                          │         Tkinter GUI
                                          ▼
                                        Grafana :3000
```

---

## Components

### Decoder (`scripts/decoder.py`)

The core of the pipeline. Owns a single `serial.Serial` (or simulator) instance and runs a byte-level state machine.

**State machine**

| State | Transition |
|-------|-----------|
| `SOF` | `0xAA` byte → `ID` |
| `ID` | collect 2 bytes big-endian → `DLC` |
| `DLC` | store length (0–8) → `DATA` |
| `DATA` | collect `DLC` bytes; abort on `0xAA`/`0xBB` mid-payload → `EOF` |
| `EOF` | `0xBB` or `0x00` → `VALID`; else → `SOF` |
| `VALID` | triggers `decode_datagram()`, resets to `SOF` |

**Signal extraction**

`global_can.yaml` is loaded fresh on every datagram. The payload is treated as a little-endian integer; each signal is extracted with:

```python
mask = (1 << bit_length) - 1
raw_value = (payload_int >> start_bit) & mask
```

Values are stored as raw integers. IEEE 754 float reinterpretation (e.g. WS22 signals) is left to the consumer (Grafana, post-processing scripts).

---

### SQLite Signal Store (`scripts/sqlite_signal_store.py`)

Two tables:

**`signal_samples`**

| Column | Type | Notes |
|--------|------|-------|
| `id` | INTEGER PK | auto-increment, used as cursor by viewer |
| `timestamp` | TEXT | ISO 8601 UTC |
| `can_id` | INTEGER | raw CAN message ID |
| `parent_name` | TEXT | board name (e.g. `front_controller`) |
| `message_name` | TEXT | message name (e.g. `drive_status`) |
| `signal_name` | TEXT | signal name (e.g. `pedal_percentage`) |
| `value` | INTEGER | raw decoded value |

**`decoder_stats`**

| `stat_key` | meaning |
|------------|---------|
| `parse_byte_calls` | total bytes seen (used for bytes/s) |
| `parsed_messages` | datagrams that reached VALID state |
| `matched_messages` | datagrams matched to a YAML entry |
| `unmatched_messages` | valid datagrams with unknown CAN ID |
| `malformed message` | datagrams aborted mid-parse |

---

### CAN Signal Viewer (`can_signal_viewer.py`)

Polls SQLite every 250 ms. Tracks the highest `signal_samples.id` it has seen and fetches only rows newer than that — no full table scans.

Byte throughput is computed from the delta in `parse_byte_calls` between polls and smoothed with an exponential moving average (α = 0.2).

---

### InfluxDB Writer (`scripts/db_write.py`)

Optional path enabled with `--dbw`. Each signal becomes one InfluxDB `Point`:

```
measurement = message_name
field       = signal_name → raw integer value
timestamp   = now (nanoseconds)
```

Infrastructure is defined in `docker-compose.yml`:

| Service | Port | Purpose |
|---------|------|---------|
| InfluxDB 2.7 | 8087 | time-series storage |
| Grafana 10.4.5 | 3000 | dashboards |

The Grafana datasource is auto-provisioned from `provisioning/datasources/datasource.yaml` on container start.

---

### CAN Configuration Pipeline

#### Source format (`can/fetched_cache/*.yaml`)

```yaml
Messages:
  <message_name>:
    id: <int>               # short message index (board messages) OR
    can_id_direct: true     # ... direct 11-bit CAN ID (external HW / synthetic)
    cycle: "fast|medium|slow|1s"
    critical: true|false
    target:
      <board>:
        watchdog: 0
    signals:
      <signal_name>:
        length: <bits>      # start_bit is computed sequentially by the generator
      <bitfield_name>:
        type: bitfield
        length: <total_bits>
        flags:
          - flag_name               # 1 bit
          - {name: flag_name, length: N}
```

#### Generator (`scripts/tools/global_can_gen.py`)

1. Parses `system_dbc.dbc` to build a `name → CAN ID` lookup.
2. Iterates all `*.yaml` files in `fetched_cache/` (sorted by `BOARD_ORDER`, extras appended alphabetically), skipping `system_can`.
3. For each message:
   - If `can_id_direct: true` → use `id` directly.
   - Otherwise → resolve the DBC name via candidate list and look up the ID.
4. Flattens signals sequentially (bitfields expand into individual flags at their assigned bit offsets).
5. Writes `can/global_can.yaml`.

#### Output format (`can/global_can.yaml`)

```yaml
messages:
  - id: 1365
    name: drive_status
    dlc: 5
    signals:
      - {name: pedal_percentage, start_bit: 0,  length: 16}
      - {name: brake_percentage, start_bit: 16, length: 16}
      - ...
```

---

### Simulators (`scripts/sim/`)

| Class | File | Use case |
|-------|------|----------|
| `SimSerial` | `sim_serial.py` | Unit tests — feed arbitrary bytes into decoder |
| `CanMessageSimulator` | `can_sim.py` | Integration — generates random but valid datagrams from board YAMLs |
| `SimFromLog` | `sim_from_log.py` | Regression — replays a `.log` capture with real inter-message timing |

Log format expected by `SimFromLog`:
```
<timestamp_ms>,RECV,<hex_payload>
```

---

### File Fetcher (`scripts/tools/file_fetcher.py`)

Calls the GitHub Contents API and base64-decodes each file into `can/fetched_cache/`. Only the four board files and two toolchain files are ever written:

```python
boards = ["front_controller", "rear_controller", "steering", "telemetry"]
# also: can/tools/system_can.py, can/tools/system_dbc.dbc
```

`ws22_motor_controller.yaml` and `telemetry_internal.yaml` are intentionally absent from this list and will never be overwritten by the fetcher.

---

## Entry Points Summary

| Script | How to run | Purpose |
|--------|-----------|---------|
| `main.py` | `python main.py [--mode sim] [--log-file F] [--dbw]` | Main decode loop |
| `can_signal_viewer.py` | `python can_signal_viewer.py` | Live GUI |
| `capture_isolated_signal.py` | `python capture_isolated_signal.py <id>` | Single-message capture |
| `scripts/tools/file_fetcher.py` | `python -m scripts.tools.file_fetcher [-b branch]` | Update fetched_cache |
| `scripts/tools/global_can_gen.py` | `python -m scripts.tools.global_can_gen` | Regenerate global_can.yaml |

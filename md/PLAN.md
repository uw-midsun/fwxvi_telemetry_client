# Telemetry Client — Rewrite Plan

## Goal

Replace the Python Tkinter frontend and decoder with a single self-contained Windows executable written in Rust. The exe reads a serial CAN stream, decodes signals against `global_can.yaml`, stores timestamped samples in SQLite, and provides a multi-tab GUI including a new interactive dashboard with saveable chart layouts.

Python source files are archived (not deleted) for debugging reference.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│  telemetry_client.exe                                           │
│                                                                 │
│  ┌─────────────────┐     channel      ┌────────────────────┐   │
│  │  Decoder thread  │ ─────────────►  │  SQLite writer     │   │
│  │                  │                 │  (signal_samples   │   │
│  │  serial port     │                 │   decoder_stats)   │   │
│  │  state machine   │                 └─────────┬──────────┘   │
│  │  YAML lookup     │                           │              │
│  └─────────────────┘                           │ poll 250ms   │
│                                                 ▼              │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  egui UI (main thread)                                   │  │
│  │                                                          │  │
│  │  [ Signal Table ]  [ Dashboard ]  [ Settings ]           │  │
│  │                                                          │  │
│  └──────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘

Files next to exe:
  global_can.yaml       — CAN definitions (path configurable in Settings)
  default_setup.json    — default dashboard layout loaded on first open
  settings.toml         — COM port, baud rate, YAML path (created on first run)
  data/decoded_data.sqlite
```

---

## Repository Layout (after migration)

```
fwxvi_telemetry_client/
│
├── src/                          # Rust source
│   ├── main.rs                   # Entry point, eframe::run_native
│   ├── app.rs                    # Top-level App state, tab routing
│   │
│   ├── decoder/
│   │   ├── mod.rs                # Serial state machine (SOF→ID→DLC→DATA→EOF)
│   │   └── can_config.rs         # global_can.yaml parser → Vec<CanMessage>
│   │
│   ├── db/
│   │   ├── mod.rs                # Schema init, connection helper
│   │   └── signal_store.rs       # insert_signal, query_signals, get_stats
│   │
│   ├── serial_manager.rs         # COM port enumeration + open/close
│   │
│   └── tabs/
│       ├── signal_table.rs       # Live signal table (ported from Python viewer)
│       ├── dashboard/
│       │   ├── mod.rs            # Dashboard tab root
│       │   ├── panel.rs          # Individual chart panel (type + config)
│       │   ├── charts.rs         # egui_plot renderers: line, gauge, histogram, scatter
│       │   ├── editor.rs         # Add/edit panel modal
│       │   ├── layout.rs         # Panel grid layout state
│       │   └── config.rs         # DashboardSetup serde structs (JSON/YAML)
│       └── settings.rs           # Settings tab UI
│
├── Cargo.toml
│
├── can/
│   ├── global_can.yaml           # Loaded at runtime by decoder
│   └── fetched_cache/            # (unchanged)
│
├── default_setup.json            # Shipped default dashboard layout
│
├── archive/
│   └── python/                   # Archived Python source
│       ├── main.py
│       ├── can_signal_viewer.py
│       ├── capture_isolated_signal.py
│       └── scripts/
│
└── md/
    ├── README.md
    ├── ARCHITECTURE.md
    └── PLAN.md                   # (this file)
```

---

## Rust Dependencies (`Cargo.toml`)

```toml
[dependencies]
eframe       = "0.29"                              # egui + winit + wgpu
egui_plot    = "0.29"                              # plotting primitives
rusqlite     = { version = "0.31", features = ["bundled"] }  # no DLL needed
serialport   = "4.5"                               # COM port enumeration + I/O
serde        = { version = "1", features = ["derive"] }
serde_json   = "1"
serde_yaml   = "0.9"
toml         = "0.8"                               # settings.toml
chrono       = "0.4"                               # timestamps
anyhow       = "1"                                 # error handling
crossbeam-channel = "0.5"                          # decoder → DB thread channel
```

---

## Tabs

### 1. Signal Table (port of existing Python viewer)

Columns: **Message · Signal · Value · Reads · Timestamp · CAN ID**

- Polls SQLite `signal_samples` every 250 ms using a cursor on `id`
- Exponential-smoothed bytes/s from `decoder_stats.parse_byte_calls`
- Status bar: DB path · signal count · last sample id · stats row

### 2. Dashboard (new)

See [Dashboard](#dashboard) section below.

### 3. Settings

| Setting | Control | Persisted in |
|---------|---------|-------------|
| COM port | dropdown (ports enumerated live) | `settings.toml` |
| Baud rate | editable int field | `settings.toml` |
| CAN YAML path | text field + browse button | `settings.toml` |
| Connect / Disconnect | button | — |

`settings.toml` is created next to the exe on first run with defaults (`COM9`, `230400`, `global_can.yaml`).

---

## Dashboard

### Concepts

| Term | Meaning |
|------|---------|
| **Setup** | A named collection of panels + global time settings, serialised to JSON |
| **Panel** | A single chart occupying a grid cell |
| **Signal selector** | `message_name.signal_name` picker populated from `global_can.yaml` |

### Panel types

| Type | egui primitive | Signals |
|------|---------------|---------|
| Line / time-series | `egui_plot::Plot` with `Line` | one or more |
| Gauge | custom arc draw using `egui::Painter` | one |
| Histogram | `egui_plot::Plot` with `Bar` | one |
| Scatter / X-Y | `egui_plot::Plot` with `Points` | exactly two (X, Y) |

### Time control

A toolbar at the top of the dashboard tab contains:

- **Live** toggle — when on, the view follows the latest N seconds (default 30 s)
- **Window** slider (5 s – 300 s) — width of the live rolling window
- **Range picker** (start / end timestamps) — unlocked when Live is off, allows scrubbing history

### Layout

```
┌──────────────────────────────────────────────────────────────────┐
│ [+ Add Panel]  [Save]  [Load]  [Setup: default_setup ▼]          │
│ ← Live [■]  Window [30s ──────●──────────] or  [Start] [End]    │
├──────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌─────────────────────────┐   ┌─────────────────────────────┐  │
│  │  Vehicle Velocity        │   │  Motor Temp                 │  │
│  │  [line chart]            │   │  [gauge]                    │  │
│  └─────────────────────────┘   └─────────────────────────────┘  │
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │  Bus Voltage over time  [line chart]                     │   │
│  └──────────────────────────────────────────────────────────┘   │
└──────────────────────────────────────────────────────────────────┘
```

Right-click any panel → **Edit** (opens panel editor modal) / **Remove**

### Panel editor modal

Fields:
- Title (text)
- Chart type (radio)
- Signal(s) — searchable list from `global_can.yaml`; for scatter: X signal and Y signal
- For gauge: min value, max value, unit label

### Setup file format (JSON)

```json
{
  "name": "default_setup",
  "live": true,
  "window_seconds": 30,
  "panels": [
    {
      "id": "p1",
      "title": "Vehicle Velocity",
      "chart_type": "line",
      "signals": ["ws22_velocity_measurement.vehicle_velocity"],
      "grid": { "col": 0, "row": 0, "col_span": 2, "row_span": 1 }
    },
    {
      "id": "p2",
      "title": "Motor Temp",
      "chart_type": "gauge",
      "signals": ["ws22_temperature.motor_temp"],
      "gauge": { "min": 0, "max": 120, "unit": "°C" },
      "grid": { "col": 2, "row": 0, "col_span": 1, "row_span": 1 }
    }
  ]
}
```

YAML is also accepted on **Load**; **Save** always writes JSON.

### Default setup

`default_setup.json` ships next to the exe and is loaded automatically on first open. It contains:
- One line chart: `ws22_velocity_measurement.vehicle_velocity`
- One gauge: `ws22_temperature.motor_temp`
- Live mode on, 30 s window

### Unsaved changes

- The `App` tracks a `dashboard_dirty: bool` flag set whenever a panel is added, removed, edited, or the time settings change.
- On window close (or **Load** over an unsaved setup), if `dashboard_dirty` is true → modal dialog:

```
"You have unsaved dashboard changes."
  [Save]   [Discard]   [Cancel]
```

---

## Decoder Rewrite (Rust)

Ports the Python `Decoder` class exactly, running in a dedicated `std::thread`:

```
SerialPort.read(1)
    → parse_byte(byte)  [same state machine: SOF/ID/DLC/DATA/EOF/VALID]
    → decode_datagram()
        → lookup CAN ID in Vec<CanMessage> (loaded once from global_can.yaml)
        → extract each signal via bit-mask
        → send SignalSample over crossbeam channel
    ← DB writer thread inserts into SQLite
```

Stats (`parse_byte_calls`, `parsed_messages`, etc.) are written to `decoder_stats` after each batch.

The decoder thread is started/stopped from the **Settings** tab Connect button.

---

## SQLite Schema

Unchanged from the existing Python implementation so historical data survives the migration:

```sql
CREATE TABLE IF NOT EXISTS signal_samples (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp    TEXT NOT NULL,
    can_id       INTEGER NOT NULL,
    parent_name  TEXT NOT NULL,
    message_name TEXT NOT NULL,
    signal_name  TEXT NOT NULL,
    value        INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS decoder_stats (
    stat_key   TEXT PRIMARY KEY,
    value      INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL
);
```

---

## Build & Distribution

```bash
cargo build --release
# output: target/release/telemetry_client.exe
```

Ship alongside the exe:
```
telemetry_client.exe
global_can.yaml           ← copy from can/global_can.yaml
default_setup.json
```

`settings.toml` and `data/decoded_data.sqlite` are created automatically on first run.

---

## Implementation Phases

### Phase 1 — Working MVP (decoder + signal table + settings)

1. Archive Python runtime files → `archive/python/` (`scripts/tools/` stays in place)
2. Scaffold `Cargo.toml`, `src/` module tree
3. DB layer — schema init, `insert_signal`, `query_signals`, `get_stats`, `increment_stat`
4. CAN config parser — `can_config.rs` deserialises `global_can.yaml` → `Vec<CanMessage>`
5. Serial decoder — state machine thread, signal extraction, crossbeam channel to DB writer
6. egui app skeleton — `eframe::run_native`, three tab stubs
7. Signal Table tab — port of Python viewer (`egui_extras::TableBuilder`, 250 ms poll)
8. Settings tab — COM port dropdown, baud field, YAML path, Connect button; `settings.toml` read/write
9. Build and smoke-test Phase 1

### Phase 2 — Dashboard charts

10. Dashboard config types — `DashboardSetup`, `Panel`, `ChartType` with serde
11. Default setup loading — read `default_setup.json` next to exe on startup
12. Chart renderers — line, gauge, histogram, scatter (`egui_plot`)
13. Panel grid layout — fixed column grid (drag-resize deferred)
14. Time-range control — live toggle, window slider, start/end timestamp pickers

### Phase 3 — Dashboard editor + persistence

15. Panel editor modal — signal search, chart type radio, gauge min/max
16. Add / remove panels — toolbar button, right-click context menu
17. Save / Load setup — file dialog (`rfd` crate), JSON write, JSON/YAML read
18. Unsaved changes prompt — dirty flag, egui modal on close / load
19. Build and smoke-test Phase 3

---

## Simulation Mode (Future)

Sim mode is not included in the initial implementation. When added it will work as follows:

- A `SimSource` trait abstracts over `Box<dyn SerialPort>` and a log-file reader
- The Settings tab gains a **Mode** toggle: `Serial | Log File`
- In log mode a file picker selects a `.log` from `logs/`; `SimFromLog` (ported from Python) replays bytes with real inter-message timing
- No changes to the decoder state machine — it is source-agnostic

---

## What Is NOT Changing

| Component | Status |
|-----------|--------|
| `can/fetched_cache/*.yaml` source files | unchanged |
| `scripts/tools/file_fetcher.py` | stays at `scripts/tools/` (run manually to update definitions) |
| `scripts/tools/global_can_gen.py` | stays at `scripts/tools/` (regenerates `global_can.yaml`) |
| `docker-compose.yml` / InfluxDB / Grafana | optional — exe does not depend on them |
| SQLite schema | identical, existing `decoded_data.sqlite` files remain valid |

# Code Structure Overview

## Module map

```
src/
├── main.rs                     eframe entry point; NativeOptions, window size
├── app.rs                      TelemetryApp: eframe::App impl, tab routing,
│                               connect/disconnect, replay wiring, per-frame update()
├── config.rs                   AppConfig (settings.toml), exe_dir(), db_path()
├── db/
│   ├── mod.rs                  open() — connection + PRAGMA + schema init
│   └── signal_store.rs         insert/query fns + SignalSample / SignalRow structs
├── decoder/
│   ├── mod.rs                  serial→datagram state machine + decoder thread run()
│   └── can_config.rs           CAN YAML load, SignalDef/CanMessage, extract_signal()
├── replay.rs                   ReplayController: read-only DB playback + snapshot()
└── tabs/
    ├── mod.rs                  re-exports the three tabs
    ├── signal_table.rs         SignalTableTab: live/replay signal grid
    ├── settings.rs             SettingsTab: port/baud/yaml/replay controls
    └── dashboard/
        ├── mod.rs              DashboardTab: toolbar, refresh loop, panel grid
        ├── config.rs           DashboardSetup / PanelConfig / ChartType / GridPos
        └── charts.rs           draw_panel() → line/gauge/histogram/scatter renderers
```

## Storage / ownership model

`TelemetryApp` (in [../src/app.rs](../src/app.rs)) owns everything:

- `cfg: AppConfig` — user settings, persisted to `settings.toml` beside the exe.
- `db_conn: Option<Connection>` — the **UI-thread's** read connection to the SQLite DB.
- `signal_table`, `dashboard`, `settings_tab` — one struct per tab, each holding its own
  view state and data cache.
- `decoder_tx: Option<Sender<DecoderCmd>>` — control channel to the decoder thread.
- `replay: Option<ReplayController>` — present only when a recorded DB is loaded.

Each tab struct follows the repo-wide "storage struct holds all mutable state" pattern:
the tab is constructed once (`::new()`) and mutated in place each frame via `show(...)`.

## Threading model

There are **two** actors touching the database:

1. **Decoder thread** (`decoder::run`, spawned in `TelemetryApp::connect`) — opens its
   **own write** `Connection`, reads serial bytes one at a time, and inserts decoded
   signals. Owns nothing from the UI; communication is one-way via `crossbeam_channel`
   for the stop command only. Data flows to the UI *through the database*, not a channel.
2. **UI thread** (`eframe` update loop) — opens a separate read `Connection` (`db_conn`)
   and polls it. Replay uses a third read-only `Connection` inside `ReplayController`.

They coordinate purely through SQLite in **WAL mode** (`PRAGMA journal_mode=WAL`,
`synchronous=NORMAL`, set in [../src/db/mod.rs](../src/db/mod.rs)), which allows one writer
concurrent with readers. This is why write-transaction batching (perf-01) matters: every
autocommit insert is a WAL commit that the reader may contend with.

## Two data-display paths

The signal table and dashboard read the DB **independently** with different cadences and
queries — there is no shared in-memory cache between them:

| Consumer | Query fn | Filter | Cadence |
|----------|----------|--------|---------|
| Signal table (live) | `query_since` | `id > last_id` | 250 ms poll |
| Dashboard (live/history) | `query_signal_history` | `message,signal,ts range` | 500 ms (`REFRESH_MS`) |
| Replay snapshot | inline window query | `timestamp <= current_ts` | on scrub / tick |

## Key types worth knowing

- **`SignalSample`** ([signal_store.rs](../src/db/signal_store.rs)) — a row about to be
  inserted (write side). **`SignalRow`** — a row read back (read side). They differ.
- **`DataCache = HashMap<String, Vec<[f64; 2]>>`** ([charts.rs](../src/tabs/dashboard/charts.rs))
  — the dashboard's per-signal point buffer. Key is `"message.signal"`, value is
  `[seconds_offset_from_window_start, value]`. Rebuilt wholesale on each refresh.
- **`PanelConfig`** — one chart. `signals: Vec<String>` are `"message.signal"` keys;
  `signal_parts()` splits on the first `.`.
- **`ReplayController`** — wraps a read-only DB opened from an arbitrary file, tracks
  `current_ts` playback position, produces a `Vec<SignalSnapshot>` for the table.

## Error handling

Library-style functions return `anyhow::Result`. The UI layer is deliberately
fault-tolerant: most read failures `.unwrap_or_default()` to an empty result rather than
propagate, so a transient DB lock never crashes a frame. Keep this convention when
editing query paths.

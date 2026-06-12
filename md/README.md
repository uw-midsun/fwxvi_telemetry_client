# FWXVI Telemetry Client

Real-time CAN telemetry acquisition and visualization for the UW Midnight Sun MSXVI electric vehicle. Decodes CAN messages from vehicle hardware over serial/XBee, stores signals locally in SQLite, and forwards to InfluxDB + Grafana for dashboarding.

---

## Directory Structure

```
fwxvi_telemetry_client/
│
├── main.py                        # Entry point — serial or log-replay decode loop
├── can_signal_viewer.py           # Tkinter GUI — live signal monitor (reads SQLite)
├── capture_isolated_signal.py     # CLI utility — capture a single CAN ID
│
├── can/
│   ├── global_can.yaml            # Generated master CAN registry (decoder input)
│   └── fetched_cache/             # Per-board YAML sources + toolchain files
│       ├── front_controller.yaml
│       ├── rear_controller.yaml
│       ├── steering.yaml
│       ├── telemetry.yaml
│       ├── telemetry_internal.yaml  # Local-only synthetic stats (XBee-side)
│       ├── ws22_motor_controller.yaml  # Local-only motor controller (fixed HW IDs)
│       ├── system_can.py           # Fetched — CAN ID enums
│       └── system_dbc.dbc          # Fetched — canonical CAN IDs for gen
│
├── scripts/
│   ├── decoder.py                 # Byte-level CAN datagram parser + signal extractor
│   ├── sqlite_signal_store.py     # SQLite writer (signal_samples + decoder_stats)
│   ├── db_write.py                # InfluxDB writer (optional, --dbw flag)
│   ├── xbee_read.py               # XBee RF receive stub
│   │
│   ├── sim/                       # Hardware-free testing
│   │   ├── can_sim.py             # Random CAN datagram generator
│   │   ├── sim_serial.py          # Queue-backed serial stub
│   │   └── sim_from_log.py        # Timed replay of captured .log files
│   │
│   └── tools/
│       ├── global_can_gen.py      # Generates global_can.yaml from fetched_cache/
│       └── file_fetcher.py        # Pulls board YAMLs + DBC from uw-midsun/fwxvi
│
├── data/
│   └── decoded_data.sqlite        # Runtime signal store (created on first run)
│
├── logs/                          # Captured serial logs for sim replay
│
├── provisioning/
│   └── datasources/
│       └── datasource.yaml        # Grafana InfluxDB datasource (auto-provisioned)
│
└── docker-compose.yml             # InfluxDB 2.7 + Grafana 10.4.5
```

---

## Quick Start

### 1. Prerequisites

```bash
pip install pyserial pyyaml influxdb-client requests
docker compose up -d        # starts InfluxDB :8087 and Grafana :3000
```

### 2. Fetch the latest CAN definitions

```bash
python -m scripts.tools.file_fetcher          # from default branch
python -m scripts.tools.file_fetcher -b main  # from a specific branch
python -m scripts.tools.global_can_gen        # regenerate global_can.yaml
```

### 3. Run the decoder

```bash
# Live serial decode (default COM9, 230400 baud)
python main.py

# Live + write to InfluxDB
python main.py --dbw

# Replay a captured log
python main.py --mode sim --log-file session1.log
```

### 4. View signals

```bash
python can_signal_viewer.py   # opens Tkinter GUI
```

Grafana dashboards: http://localhost:3000  
InfluxDB UI: http://localhost:8087

---

## CAN Configuration Workflow

Board-specific YAML files live in `can/fetched_cache/` and are the source of truth for message/signal definitions. `global_can.yaml` is a generated artifact — **never edit it by hand**.

```
fwxvi repo (uw-midsun/fwxvi)
    ↓  file_fetcher.py
can/fetched_cache/*.yaml + system_dbc.dbc
    ↓  global_can_gen.py
can/global_can.yaml
    ↓  decoder.py reads at runtime
```

Two files in `fetched_cache/` are **local-only** and must never be added to the fetcher's boards list:

| File | Reason |
|------|--------|
| `ws22_motor_controller.yaml` | WS22 has fixed hardware CAN IDs not in system DBC |
| `telemetry_internal.yaml` | Synthetic XBee-side stats, never transmitted on CAN bus |

Both use `can_id_direct: true` in their YAML so the generator uses their `id` field directly rather than performing a DBC lookup.

---

## Datagram Protocol

The telemetry firmware wraps CAN frames in a simple framing protocol over UART:

```
[ 0xAA ][ ID: 2 bytes, big-endian ][ DLC: 1 byte ][ DATA: 0–8 bytes ][ 0xBB ]
```

The decoder implements a state machine (`SOF → ID → DLC → DATA → EOF → VALID`) that tolerates arbitrary byte boundaries from `serial.read(1)`.

---

## See Also

- [ARCHITECTURE.md](ARCHITECTURE.md) — component internals and data flow

# Data Flow: Serial Byte → Plotted Point

This traces one signal value from the wire to the screen, calling out every place where
latency or wasted work can accumulate. File references are clickable.

## 1. Serial → Datagram (decoder thread)

[../src/decoder/mod.rs](../src/decoder/mod.rs)

```
port.read(1 byte) ──► Parser::push(byte) ──► Option<Datagram>
```

- Reads **one byte at a time** into `byte_buf: [u8; 1]` in a tight loop.
- `Parser` is a 5-state machine: `Sof → Id → Dlc → Data → Eof`.
- Framing: `0xAA` SOF, 2-byte big-endian ID, 1-byte DLC (≤8), DLC data bytes, `0xBB` EOF.
- On a complete datagram, `decode_datagram()` is called inline.

## 2. Datagram → SQLite rows (decoder thread)

`decode_datagram()`:

1. `increment_stat("parsed_messages", 1)` — **1 write**.
2. Linear scan `messages.iter().find(id)` to match the CAN definition.
3. `increment_stat("matched_messages" | "unmatched_messages", 1)` — **1 write**.
4. Build little-endian `u64` payload; `chrono::Utc::now().to_rfc3339()` timestamp string.
5. For each signal: `extract_signal()` then `insert_signal()` — **N writes**.

> **Write amplification:** each datagram = `2 + N` separate autocommit transactions
> (WAL commits). `parse_byte_calls` adds one more, but only flushed once/second.
> This is the target of [perf-01](perf-01-db-indexing-and-batching.md).

Timestamp is stored as RFC3339 **TEXT** (target of
[perf-03](perf-03-integer-epoch-timestamps.md)).

## 3. SQLite → UI (crosses the thread boundary)

The decoder's write connection and the UI's read connection are different handles to the
same WAL database. **No in-process channel carries data** — the UI discovers new rows by
polling. Latency floor is therefore the poll interval, not the decode speed.

## 4. UI read + render (UI thread, `eframe::App::update`)

[../src/app.rs](../src/app.rs) `update()` runs per frame:

```
if connected || replaying:  ctx.request_repaint_after(16 ms)   // ~60 fps
```

Then routes to the active tab. Two consumers:

### Signal Table — [../src/tabs/signal_table.rs](../src/tabs/signal_table.rs)
- `poll_live()` throttles to **250 ms**; `query_since(last_id, 500)` pulls new rows.
- `upsert()` does a **linear `rows.iter_mut().find()`** per new row (O(rows) each).
- Draws with `egui_extras::TableBuilder` using virtualized `body.rows()` (only visible
  rows are built — the table itself is fine).

### Dashboard — [../src/tabs/dashboard/mod.rs](../src/tabs/dashboard/mod.rs)
- `maybe_refresh()` throttles to **500 ms** (`REFRESH_MS`).
- Collects the unique signal set across all panels, then fires **one
  `query_signal_history` per signal** (N round trips), each `LIMIT 5000`.
- Each returned row is `parse_ts()`-d from string to `f64` (per-row parse).
- Rebuilds the entire `DataCache` wholesale.
- `draw_panels()` → `charts::draw_panel()` per panel, **every frame** (60 fps), even
  though `cache` only changed at 2 Hz. Charts `.clone()` point vectors / rebuild
  histogram bins on every one of those frames.

## Cadence summary (the important numbers)

| Constant | Value | Location | Meaning |
|----------|-------|----------|---------|
| repaint interval | 16 ms (~60 fps) | app.rs:122 | how often `update()` + all draws run while live |
| `REFRESH_MS` | 500 ms | dashboard/mod.rs:13 | how often dashboard re-queries the DB |
| signal-table poll | 250 ms | signal_table.rs:49 | how often the table re-queries |
| query row cap | 5000 | dashboard/mod.rs:133 | max points per signal per refresh |
| stat flush | 1 s | decoder/mod.rs:170 | how often `parse_byte_calls` is written |

**The core inefficiency:** render cadence (60 fps) is ~30× the data-refresh cadence
(2 Hz), so the expensive chart work (clone, bin, transform, cull) repeats on identical
data. [perf-02](perf-02-repaint-throttling.md) aligns the two;
[perf-04](perf-04-chart-caching-and-decimation.md) makes each render cheaper regardless.

## Replay path (alternative to steps 1–3)

When a DB file is loaded, the decoder is bypassed. `ReplayController::tick()` advances
`current_ts` by wall-time × speed; when the position changes, `snapshot()` runs a window
query (`ROW_NUMBER()` + `COUNT()` over `timestamp <= current_ts`) to get the latest value
per signal. That query scans the whole table each scrub — see
[perf-05](perf-05-replay-snapshot-query.md).

# Perf 03 — Integer Epoch Timestamps

**Goal:** store time as an integer (epoch milliseconds) instead of RFC3339 text, so SQLite
compares/sorts/indexes it numerically and the UI stops parsing a string per row.

**Impact:** medium — removes per-row `parse_ts()` cost (up to 5000 × N rows every 500 ms
on the dashboard) and makes range queries and the perf-01 index materially faster.

**Effort:** medium (cross-cutting — touches schema, insert, all three read paths, and
display formatting). **Risk:** medium (changes stored format; needs a migration story).

---

## Problem

Timestamps are stored as RFC3339 **TEXT** ([../src/db/mod.rs](../src/db/mod.rs) schema;
written in [../src/decoder/mod.rs](../src/decoder/mod.rs) via
`chrono::Utc::now().to_rfc3339()`). Consequences:

- Every read path converts back to `f64` with `parse_ts()`
  ([../src/replay.rs](../src/replay.rs)) — e.g. `dashboard/mod.rs` does it **per point**,
  every refresh.
- Range comparisons (`timestamp BETWEEN ? AND ?`, `timestamp <= ?`) are lexical string
  comparisons — correct only because RFC3339 is zero-padded, but far heavier than integer
  comparison and less index-efficient.

## Design decision — new column vs. reinterpret

Recommended: **add an integer column `ts_ms INTEGER` (epoch milliseconds)** and treat it
as the source of truth for ordering/filtering. Keep a human-readable value for the signal
table's "Timestamp" display by **formatting `ts_ms` at render time** rather than storing a
string.

Two migration options:

- **Fresh start (simplest):** the DB is a local decode cache under `data/`. If losing old
  recordings is acceptable, bump the schema to use `ts_ms` and delete/recreate. Document it.
- **In-place migration:** `ALTER TABLE signal_samples ADD COLUMN ts_ms INTEGER;` then a
  one-time backfill `UPDATE ... SET ts_ms = <parsed timestamp>` (do the parse in Rust in
  batches, or with SQLite's `strftime('%s', timestamp)` × 1000 for second precision).
  Keep the old `timestamp` column until all readers are migrated, then drop it.

## Plan

### Step 1 — schema

```sql
CREATE TABLE IF NOT EXISTS signal_samples (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms        INTEGER NOT NULL,          -- epoch milliseconds, UTC
    can_id       INTEGER NOT NULL,
    parent_name  TEXT    NOT NULL,
    message_name TEXT    NOT NULL,
    signal_name  TEXT    NOT NULL,
    value        INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_samples_msg_sig_ts
    ON signal_samples (message_name, signal_name, ts_ms);
CREATE INDEX IF NOT EXISTS idx_samples_ts ON signal_samples (ts_ms);
```

(Coordinate the composite index with [perf-01](perf-01-db-indexing-and-batching.md) — it's
the same index, trailing column swapped from `timestamp` to `ts_ms`.)

### Step 2 — write side

`SignalSample.timestamp: String` → `ts_ms: i64`. In `decode_datagram`:

```rust
let ts_ms = chrono::Utc::now().timestamp_millis();
```

Compute once per datagram (as today) and reuse for all signals.

### Step 3 — read sides

- **`query_signal_history`** ([../src/db/signal_store.rs](../src/db/signal_store.rs)):
  take `since_ms: i64, until_ms: i64`; return `Vec<(i64, i64)>` = `(ts_ms, value)`. The
  dashboard then does `(*ts_ms as f64 / 1000.0) - offset` — a cheap arithmetic op instead
  of `parse_ts()`. `parse_ts` on the string path disappears from the hot loop.
- **`query_since`** / `SignalRow`: return `ts_ms: i64`; the signal table formats it for
  display (Step 4).
- **`replay.rs`**: `MIN/MAX(ts_ms)` returns integers directly — `start_ts`/`end_ts` become
  `ms as f64 / 1000.0` (or keep everything in ms). `snapshot()` filters `WHERE ts_ms <= ?`
  with an integer bind; `format_ts_iso` is no longer needed for the query. The `parse_ts`
  helper can be retired once all callers move to integers.

### Step 4 — display formatting

The signal table currently shows the raw stored string. Add a small formatter:

```rust
fn fmt_ts_ms(ts_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ts_ms)
        .map(|dt: chrono::DateTime<chrono::Utc>| dt.format("%H:%M:%S%.3f").to_string())
        .unwrap_or_default()
}
```

Format only the rows actually drawn (the table is virtualized, so this is cheap).

## Validation

- Round-trip: decode a known feed, confirm plotted x-values and table timestamps match the
  previous string-based build.
- `EXPLAIN QUERY PLAN` shows integer range scan on `idx_samples_ts` / composite index.
- Benchmark a 500 ms dashboard refresh with a large window (≈5000 pts × several signals):
  per-refresh CPU should drop once `parse_ts` is gone from the loop.
- Sorting sanity: `ORDER BY ts_ms` and `ORDER BY id` should agree for a single writer.

## Dependencies

- Do **after** [perf-01](perf-01-db-indexing-and-batching.md) so you edit the index once.
- Simplifies [perf-05](perf-05-replay-snapshot-query.md) (integer range bind).
- Removes work that [perf-04](perf-04-chart-caching-and-decimation.md) would otherwise
  cache.

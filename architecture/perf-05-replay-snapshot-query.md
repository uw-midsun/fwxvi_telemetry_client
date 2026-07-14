# Perf 05 — Replay Snapshot Query Rework

**Goal:** make replay scrubbing fast on large databases by replacing the whole-table
window-function scan with an indexed "latest value per signal as of timestamp" lookup.

**Impact:** high **for replay of large DBs**, none for live mode. **Effort:** medium/high
(query redesign, possibly a helper structure). **Risk:** medium (must preserve the exact
"latest value + total sample count up to now" semantics).

> Tackle this **last** and re-measure. Once [perf-01](perf-01-db-indexing-and-batching.md)
> adds `idx_samples_ts`, small/medium recordings may already scrub acceptably and this
> becomes optional.

---

## Problem

`ReplayController::snapshot()` in [../src/replay.rs](../src/replay.rs) runs, on **every**
scrub/tick position change:

```sql
SELECT ... FROM (
  SELECT ...,
    ROW_NUMBER() OVER (PARTITION BY message_name, signal_name ORDER BY timestamp DESC) AS rn,
    COUNT(*)     OVER (PARTITION BY message_name, signal_name)                         AS sample_count
  FROM signal_samples
  WHERE timestamp <= ?1
) WHERE rn = 1;
```

This scans **all rows up to `current_ts`**, partitions and ranks them, every time the
position moves. As `current_ts` approaches the end of a long recording, that's nearly the
entire table — per frame while playing. It's the replay analogue of the missing-index
problem.

The query returns two things per signal: the **latest value** (`rn = 1`) and the **total
sample count** so far (`sample_count`, shown as "Reads" in the table).

## Plan

### Step 1 — split the two concerns

The window query conflates "latest row" with "running count." Separate them:

- **Latest value per signal ≤ ts:** for each known `(message, signal)`, the single most
  recent row. With `idx_samples_msg_sig_ts` (perf-01/03) this is a per-signal index seek,
  not a scan.
- **Sample count per signal ≤ ts:** a `COUNT(*)` per signal, or a cheaper approximation.

### Step 2 — enumerate signals once

The set of `(message, signal)` pairs is fixed by the CAN YAML / stable across a recording.
Query it once on `ReplayController::open`:

```sql
SELECT DISTINCT message_name, signal_name FROM signal_samples;
```

Cache as `Vec<(String, String)>`. (Alternatively derive from the loaded CAN config.)

### Step 3 — per-signal latest lookup

For each cached pair, a correlated lookup that the composite index serves directly:

```sql
SELECT can_id, value, ts_ms
FROM signal_samples
WHERE message_name = ?1 AND signal_name = ?2 AND ts_ms <= ?3
ORDER BY ts_ms DESC
LIMIT 1;
```

`prepare_cached` once, then bind per signal. This is N cheap index seeks (N = signal
count, typically small and bounded) instead of one giant scan. For a few hundred signals
this is still far cheaper than ranking the whole table.

### Step 4 — counts

Options, cheapest first:

- **Drop/soften "Reads" during scrub:** if an exact running count isn't essential mid-drag,
  compute it lazily (e.g. only when paused) so scrubbing stays snappy.
- **Indexed count:** `SELECT COUNT(*) ... WHERE message=? AND signal=? AND ts_ms<=?` per
  signal — with the composite index this is a covering count, cheaper than the window
  variant but still O(rows-in-range) per signal.
- **Incremental counting:** when playing forward, maintain per-signal counters and adjust
  by the rows crossed between the previous and new `current_ts` (a small range query),
  instead of recounting from the start. Reset on backward seeks.

### Step 5 — only re-query on real movement

`snapshot()` already gates via `needs_refresh()` / `last_rendered`. Confirm it isn't being
called every frame while merely *playing* at a slow speed with sub-millisecond deltas — the
`> 0.001` threshold in `needs_refresh` handles this, but verify after wiring the new path.

## Validation

- Correctness: for several scrub positions, the new per-signal result must match the old
  window-query result (latest value, timestamp, and — if kept — count) exactly. A one-off
  test that runs both queries and diffs is worthwhile.
- Performance: scrub to the end of a large recording and drag the slider — should feel
  instant. `EXPLAIN QUERY PLAN` on the per-signal query must show an index seek + `LIMIT`,
  no `SCAN`.
- Edge cases: signals with zero samples before `ts` (should be absent, as today);
  backward seeks; position exactly at `start_ts` / `end_ts`.

## Dependencies

- **Requires** [perf-01](perf-01-db-indexing-and-batching.md) (`idx_samples_ts` /
  composite index) — without it the per-signal `ORDER BY ... LIMIT 1` still scans.
- Cleaner with [perf-03](perf-03-integer-epoch-timestamps.md) (integer `ts_ms` bind,
  no `format_ts_iso`).

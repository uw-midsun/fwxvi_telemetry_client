# Perf 01 — DB Indexing & Write Batching

**Goal:** stop full-table scans on every dashboard/replay query, and collapse the
`2 + N`-transactions-per-datagram write pattern into batched transactions.

**Impact:** high. This is the main reason the app "gets slower the longer it records" —
both scan cost and WAL contention grow with table size / write rate.

**Effort:** medium. **Risk:** low (additive index + transaction wrapping; no query-shape
change).

---

## Problem A — no usable index

Schema in [../src/db/mod.rs](../src/db/mod.rs) creates only:

```sql
CREATE INDEX idx_signal_samples_id ON signal_samples(id);
```

`id` is already `INTEGER PRIMARY KEY AUTOINCREMENT`, i.e. the rowid — this index is
**redundant** and never helps. Meanwhile the real queries have no support:

- `query_signal_history` (dashboard): `WHERE message_name=? AND signal_name=? AND timestamp BETWEEN ? AND ?`
- `replay.snapshot()`: window functions over `WHERE timestamp <= ?`

Both do full table scans that grow linearly with recording length.

## Problem B — write amplification

`decode_datagram()` in [../src/decoder/mod.rs](../src/decoder/mod.rs) issues, per datagram:
`increment_stat` (parsed) + `increment_stat` (matched/unmatched) + one `insert_signal`
**per signal** — all autocommit, so each is its own WAL commit. At bus rates this is a lot
of fsync-class overhead and it contends with the UI's reader connection.

---

## Plan

### Step 1 — fix the index (drop redundant, add composite)

In `init_schema` ([../src/db/mod.rs](../src/db/mod.rs)):

```sql
-- remove: CREATE INDEX idx_signal_samples_id ...   (redundant with rowid PK)

CREATE INDEX IF NOT EXISTS idx_samples_msg_sig_ts
    ON signal_samples (message_name, signal_name, timestamp);

-- supports the replay snapshot's "latest as of ts" pattern (see perf-05)
CREATE INDEX IF NOT EXISTS idx_samples_ts
    ON signal_samples (timestamp);
```

Leave a one-time cleanup so existing DBs drop the old index:

```rust
conn.execute_batch("DROP INDEX IF EXISTS idx_signal_samples_id;")?;
```

> Column-order matters: put the two equality columns first, the range column
> (`timestamp`) last, so the composite index serves both the equality filter and the
> `ORDER BY timestamp`. After [perf-03](perf-03-integer-epoch-timestamps.md) the trailing
> column becomes the integer `ts_ms`, which is even better for range scans.

### Step 2 — batch decoder writes into one transaction per datagram

Wrap the per-datagram work in a single transaction. `decode_datagram` currently takes
`&Connection`; give it a transaction, or use an explicit `BEGIN/COMMIT`:

```rust
// in decode_datagram, replacing the loose insert loop
let tx = conn.unchecked_transaction()?;   // rusqlite: borrows &Connection
increment_stat(&tx, "parsed_messages", 1)?;
// ... match ...
increment_stat(&tx, "matched_messages", 1)?;
for sig in &msg.signals {
    insert_signal(&tx, &sample)?;
}
tx.commit()?;
```

`increment_stat` / `insert_signal` take anything `Deref<Target = Connection>`, and a
`Transaction` derefs to `Connection`, so their signatures don't need to change.

### Step 3 (optional, higher throughput) — time-boxed multi-datagram batches

If a single-datagram transaction is still too chatty at high bus load, accumulate for a
short window (e.g. 20–50 ms) and commit as one transaction. This trades a small amount of
worst-case data-loss-on-crash for far fewer commits. Sketch:

```rust
let mut tx = conn.unchecked_transaction()?;
let mut ops = 0usize;
let mut batch_started = Instant::now();
// inside the read loop, after producing a datagram:
//   apply its inserts against `tx`; ops += 1 + N;
// commit when either fires:
if ops >= 200 || batch_started.elapsed() >= Duration::from_millis(30) {
    tx.commit()?;
    tx = conn.unchecked_transaction()?;
    ops = 0; batch_started = Instant::now();
}
```

Be sure to `commit()` on the `DecoderCmd::Stop` path and on serial-error exit so the tail
isn't lost.

### Step 4 — move stat counters out of the hot path (optional)

`increment_stat` is an upsert with an `updated_at = now()` write every call. Consider
accumulating `parsed`/`matched`/`unmatched` in local `i64`s in the decoder thread and
flushing them alongside the existing 1-second `parse_byte_calls` flush, instead of writing
on every datagram. This removes two writes per datagram entirely.

---

## Validation

1. **Index is used:** `EXPLAIN QUERY PLAN` the dashboard query — should read
   `SEARCH signal_samples USING INDEX idx_samples_msg_sig_ts` instead of `SCAN`.
   ```sql
   EXPLAIN QUERY PLAN
   SELECT timestamp, value FROM signal_samples
   WHERE message_name=? AND signal_name=? AND timestamp>=? AND timestamp<=?
   ORDER BY id ASC;
   ```
   (Note: consider `ORDER BY timestamp` to match the index; see perf-03.)
2. **Write batching:** with a synthetic high-rate serial feed, confirm sustained
   `Bytes/s` in the signal-table header stays flat as the DB grows, and CPU on the decoder
   thread drops.
3. **Correctness:** row counts before/after must match; no dropped signals. Kill the app
   mid-record and confirm the DB still opens (WAL recovery) — at most the last uncommitted
   batch is lost.

## Dependencies

- Complements [perf-03](perf-03-integer-epoch-timestamps.md): the composite index's
  trailing column should become the integer timestamp there.
- Unblocks [perf-05](perf-05-replay-snapshot-query.md): the `idx_samples_ts` index is a
  prerequisite for a fast "latest as of ts" rewrite.

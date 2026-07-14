# Architecture & Performance Notes

Reference docs for the FWXVI Telemetry Client — a desktop [egui](https://github.com/emilk/egui)
app that decodes CAN datagrams off a serial port, stores decoded signals in SQLite,
and visualizes them live or from a recorded database (replay).

## Contents

### Code structure
- [overview.md](overview.md) — module map, storage/threading model, key types.
- [data-flow.md](data-flow.md) — end-to-end path from serial bytes to a plotted point,
  including every timing/cadence constant and where the two threads meet.

### Performance improvement plans
These came out of a profiling review of "the graphs are laggy / the app gets slower the
longer it records." They are ordered by impact-to-effort. Each is self-contained but the
**Dependencies** section of each doc notes where they interact.

| # | Plan | Primary win | Effort |
|---|------|-------------|--------|
| 1 | [perf-01-db-indexing-and-batching.md](perf-01-db-indexing-and-batching.md) | Stops full-table scans + per-signal write transactions | Medium |
| 2 | [perf-02-repaint-throttling.md](perf-02-repaint-throttling.md) | ~30× fewer chart re-renders | Low |
| 3 | [perf-03-integer-epoch-timestamps.md](perf-03-integer-epoch-timestamps.md) | Numeric (index-friendly) time, no per-row string parse | Medium |
| 4 | [perf-04-chart-caching-and-decimation.md](perf-04-chart-caching-and-decimation.md) | Render cost independent of sample count | Medium |
| 5 | [perf-05-replay-snapshot-query.md](perf-05-replay-snapshot-query.md) | Instant replay scrubbing on large DBs | Medium/High |

## Suggested implementation order

1. **perf-02** first — it is nearly free and removes ~29 of every 30 wasted frames, which
   is the most visible "lag." Do it before benchmarking anything else so measurements
   reflect real per-refresh work.
2. **perf-01** — the structural DB win; also unblocks perf-05.
3. **perf-03** — easier to land after perf-01's index exists; changes the same call sites.
4. **perf-04** — caching becomes simpler once perf-02 has throttled repaints.
5. **perf-05** — tackle last and re-measure; may be unnecessary for small DBs once the
   index from perf-01 exists.

> These files live inside the `fwxvi_telemetry_client` **submodule**. Commit them in that
> repo, not the parent `fwxvi` repo.

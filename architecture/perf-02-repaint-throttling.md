# Perf 02 — Repaint Throttling

**Goal:** stop re-running the full chart render pipeline ~30× more often than the data
actually changes.

**Impact:** high *perceived* improvement for very low effort — this is likely the single
most noticeable "lag" fix. **Effort:** low. **Risk:** low.

---

## Problem

[../src/app.rs](../src/app.rs) requests a repaint every 16 ms (~60 fps) whenever live or
replaying:

```rust
let needs_repaint = self.connected
    || self.replay.as_ref().map_or(false, |r| r.playing);
if needs_repaint {
    ctx.request_repaint_after(std::time::Duration::from_millis(16));
}
```

But the dashboard only refetches data every **500 ms** (`REFRESH_MS`,
[../src/tabs/dashboard/mod.rs](../src/tabs/dashboard/mod.rs)). So ~29 of every 30 frames
redo all of `egui_plot`'s work — point-vector clones, histogram binning, coordinate
transforms, culling — on **identical** data. That is wasted CPU/GPU and the source of the
jank, especially with several panels or large point counts.

## Plan

Two complementary changes; do at least **A**.

### A — repaint at the data cadence, not 60 fps

The dashboard's meaningful update rate is 2 Hz. When the dashboard tab is active and no
replay is scrubbing, request the next repaint aligned to the refresh interval instead of
16 ms:

```rust
// app.rs update(), replacing the fixed 16ms request
use std::time::Duration;

let interval = match self.active_tab {
    Tab::Dashboard if self.replay.is_none() => Duration::from_millis(REFRESH_MS as u64),
    _ if self.connected || self.replay.as_ref().map_or(false, |r| r.playing)
                                           => Duration::from_millis(16),
    _                                      => return, // idle: no timed repaint
};
ctx.request_repaint_after(interval);
```

Keep 60 fps for the signal table (it polls at 250 ms but the table is cheap) and for
replay scrubbing where smoothness matters. Expose `REFRESH_MS` (make it `pub(crate)`) or
mirror the value so `app.rs` can read it.

### B — event-driven repaint on actual data change (better)

Have `maybe_refresh()` report whether it swapped in new data, and only request a repaint
when it did. This makes the UI truly reactive — zero frames on a stalled feed.

```rust
// dashboard/mod.rs
fn maybe_refresh(&mut self, conn: &Connection) -> bool {  // returns "did data change"
    // ... existing guard ...
    self.cache = new_cache;
    true
}

pub fn show(&mut self, ui: &mut egui::Ui, conn: Option<&Connection>) -> bool {
    self.draw_toolbar(ui);
    ui.separator();
    let changed = conn.map_or(false, |c| self.maybe_refresh(c));
    self.draw_panels(ui);
    changed
}
```

Then in `app.rs`, only `ctx.request_repaint_after(REFRESH_MS)` when the tab reports a
change (plus a fallback timer so the throttle guard inside `maybe_refresh` still fires).
Because `maybe_refresh` already time-gates itself with `last_refresh`, a coarse fallback
repaint (e.g. every `REFRESH_MS`) is enough to keep polling alive.

## Interaction with charts

Combined with [perf-04](perf-04-chart-caching-and-decimation.md) this compounds: once the
cache only rebuilds on change **and** repaints only happen on change, the expensive
per-point work in `charts.rs` effectively runs at 2 Hz instead of 60 Hz — a ~30× drop with
no visible loss of liveness at a 500 ms update rate.

## Validation

- Watch CPU/GPU usage on the Dashboard tab with a live feed before/after — expect a large
  drop while the plot still visibly updates twice a second.
- Confirm interactivity is unaffected: hovering/zooming an `egui_plot` still feels
  responsive (egui auto-requests a repaint on input, so user interaction is not gated by
  the timer).
- Toolbar edits (window slider, Live/History toggle) must still apply promptly — verify
  they trigger a repaint (they do, via input events).

## Dependencies

- Independent; safe to land first.
- Amplified by [perf-04](perf-04-chart-caching-and-decimation.md).

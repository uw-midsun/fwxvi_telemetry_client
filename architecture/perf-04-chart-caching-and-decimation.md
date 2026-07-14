# Perf 04 — Chart Caching & Point Decimation

**Goal:** make chart render cost independent of (a) frame rate and (b) raw sample count.

**Impact:** medium–high on busy dashboards. **Effort:** medium. **Risk:** low–medium
(visual output must stay faithful after decimation).

---

## Problem

Every renderer in [../src/tabs/dashboard/charts.rs](../src/tabs/dashboard/charts.rs)
recomputes from raw points **on every draw call**, and (until
[perf-02](perf-02-repaint-throttling.md)) that's ~60 fps on 2 Hz data:

- `draw_line` — `PlotPoints::new(pts.clone())` clones the whole vector per signal, per frame.
- `draw_scatter` — clones **two** full vectors (`x_pts`, `y_pts`) per frame.
- `draw_histogram` — rebuilds a `BTreeMap` binning of all values per frame.
- No decimation anywhere: a `LIMIT 5000` window means up to 5000 points transformed and
  culled per line, regardless of how many pixels wide the plot is.

## Plan

### Part 1 — cache derived render data alongside the cache

Right now `DataCache = HashMap<String, Vec<[f64;2]>>` holds only raw points. Add a parallel
cache of *render-ready* artifacts, rebuilt in `maybe_refresh()` (2 Hz) instead of in the
draw path (per frame). Options, simplest first:

**Option A — cache decimated point vectors per signal** (covers line/scatter):

```rust
// dashboard state
struct RenderCache {
    // key "msg.sig" -> decimated [x, y] ready to hand to PlotPoints
    lines: HashMap<String, Vec<[f64; 2]>>,
    // key "msg.sig" -> pre-binned histogram bars
    hists: HashMap<String, Vec<(f64, f64)>>, // (bin_center, count)
}
```

Populate it at the end of `maybe_refresh` from the new raw `cache`. `draw_line` then does
`PlotPoints::new(render.lines[key].clone())` on an already-small vector (or, better, keep
`egui_plot::PlotPoints` per signal and clone the cheap handle). Histogram draw just maps
the cached bars to `Bar`.

**Option B — store owned `PlotPoints`/`Vec<Bar>` directly** in the cache and hand egui
references. Slightly more coupling to `egui_plot` types in the data layer, but removes even
the per-frame `clone` of the decimated vector.

Either way the rule is: **charts.rs must not touch raw points or rebin/rebuild on draw** —
it only renders what `maybe_refresh` prepared.

### Part 2 — decimate line/scatter to plot pixel width

A line plot can't show more distinct points than it has horizontal pixels. For each line,
target ≈ `2 × plot_width_px` points using **min/max-per-bucket** decimation (preserves
spikes, unlike naive stride sampling):

```rust
/// Reduce to ~target points, keeping the min & max of each x-bucket so
/// transient peaks/troughs survive.
fn decimate_minmax(pts: &[[f64; 2]], target: usize) -> Vec<[f64; 2]> {
    if pts.len() <= target || target < 4 { return pts.to_vec(); }
    let bucket = (pts.len() as f64 / (target as f64 / 2.0)).ceil() as usize;
    let mut out = Vec::with_capacity(target + 2);
    for chunk in pts.chunks(bucket) {
        let (mut lo, mut hi) = (chunk[0], chunk[0]);
        for p in chunk {
            if p[1] < lo[1] { lo = *p; }
            if p[1] > hi[1] { hi = *p; }
        }
        // emit in x-order so the line doesn't zig-zag backwards
        if lo[0] <= hi[0] { out.push(lo); out.push(hi); }
        else              { out.push(hi); out.push(lo); }
    }
    out
}
```

The plot's pixel width is known at draw time; a reasonable approach is to decimate to a
fixed generous target (e.g. 2000) in `maybe_refresh`, which already caps the worst case
well below the 5000 row limit and is independent of window size. If you want pixel-exact
decimation, capture the panel width from the previous frame and pass it into the refresh.

> Only decimate for display. Histogram binning already collapses values, so it needs
> caching (Part 1) but not decimation. Gauges use only the latest point — no change.

### Part 3 — trim the histogram allocation

`draw_histogram` builds a `BTreeMap<i64, usize>`. Once binning moves into `maybe_refresh`
(Part 1), it runs at 2 Hz. If bin counts are large, consider a fixed bin count over the
value range rather than one bin per distinct integer value.

## Validation

- Visual diff: overlay a decimated line against the raw line for a spiky signal — peaks
  must still be visible (that's the point of min/max bucketing).
- Confirm `charts.rs` no longer calls `.clone()` on raw vectors or builds a `BTreeMap` in
  the draw path (grep for them).
- Profile a dashboard with several line panels at 5000-point windows: frame time should be
  flat regardless of window size.

## Dependencies

- Best after [perf-02](perf-02-repaint-throttling.md) (so caching is refreshed at the
  right cadence) and [perf-03](perf-03-integer-epoch-timestamps.md) (so cached x-values are
  cheap integer→f64, not string parses).
- The cache-rebuild hook is the same `maybe_refresh` boundary perf-02 keys off of.

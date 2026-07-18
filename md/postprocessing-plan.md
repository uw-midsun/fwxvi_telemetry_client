# Plan: Cell-voltage (float) & bitfield-flag postprocessing

Status: **bitfield flags implemented; float auto-decode still deferred.**

- **Bitfield flags (done):** `bps_fault`, `ws22_status.error_flags`,
  `ws22_status.limit_flags` now decode to a live list of active flags. Rather than
  the generator/decoder scheme sketched below, flags are handled like `enum`: a
  `flags:` (bit → label) rule in `postprocess.yaml`, baked into `global_can.yaml`,
  surfaced at display time via `can_config::FlagLookup` / `format_flags`. The raw
  integer is still what the decoder stores; the signal table and the "Vehicle
  Status" dashboard widget render every set bit (e.g. "OVERVOLTAGE | OVERCURRENT").
  ⚠️ **On-wire layout assumption:** error labels are placed at enum bits 0-8 and
  limit labels at 9-15 (per the firmware `Ws22MotorFlags` enum). If firmware packs
  the limit register at its own bits 0-6 instead, edit the `flags:` bit indices in
  `postprocess.yaml` (and the baked `global_can*.yaml`) — no code change needed.
  Firmware defines both CFG_READ_ERROR and UVLO at bit 6 (bit 5 unused), so bit 6
  carries the combined "CFG_READ_ERROR/UVLO" label.
- **Float auto-decode (deferred):** the generator-driven scheme below is unbuilt;
  cell/AFE voltages currently use explicit per-signal `float:` rules in
  `postprocess.yaml` instead. The original design was reverted after a "no data at
  all" regression (see *Root cause* below); re-apply once the blocker is resolved.

## Goals

1. **Float signals** (`type: float` with `min`/`max`/`length` in the board YAML,
   e.g. cell voltages, pack voltage/SoC/current, AFE cell voltages) should be
   automatically decoded to real units — no per-signal entry in `postprocess.yaml`.
2. **Bitfield signals** shown as multiple simultaneous states:
   - `rear_controller_status.bps_fault` (11 bits)
   - `ws22_status.error_flags` / `ws22_status.limit_flags` (16 bits each)
   - Add the WS22 flags to the existing drive-state / BPS-fault ("Vehicle Status")
     widget, rendered as a labeled LED grid.

## ⚠️ Root cause of the "no data" regression (resolve FIRST)

`scripts/tools/global_can_gen.py` regenerates `can/global_can.yaml` from
`can/fetched_cache/`. In the current tree the fetched_cache is **ahead of the
firmware running on the device**:

- DBC assigns different CAN IDs (e.g. 1861 vs the device's 1860, 1361 vs 1365).
- Cell voltage is 8-bit in the board YAML but 16-bit on the device.

Regenerating therefore produces a config the decoder can't match against the live
stream → nothing decodes → no data. The working config is preserved in
`can/global_can copy.yaml` (device IDs 1860…, 16-bit layouts).

**Do not run `global_can_gen.py` until the device firmware matches the current
`fetched_cache` DBC.** Until then, apply postprocessing by *overlaying* onto the
working `global_can.yaml` (grafting by signal name, recomputing against the
working file's bit lengths) rather than regenerating.

> NOTE: Even after restoring the working IDs and overlaying postprocessing, data
> still did not appear in testing. Before re-attempting, confirm the true root
> cause of "no data" (device/firmware sync, serial link, or config) — the ID
> mismatch may not be the only factor.

## Firmware float encoding (authoritative)

From the firmware `set_*` macros, a value `v` in `[min, max]` over an `n`-bit
unsigned field is encoded as:

```
raw = clamp:
        v <= min          -> 0
        v >= max          -> 2^n - 1
        else              -> (v - min)/(max - min) * (2^n - 3) + 1     // range [1, 2^n-2]
```

So the exact client-side **decode** is:

```
scale  = (max - min) / (2^n - 3)
offset = min - scale
value  = raw * scale + offset          // then clamp to [min, max]
```

`raw == 0` and `raw == 2^n-1` are low/high saturation sentinels (clamp handles them).

Worked example — `max_cell_voltage`, device length **16**, min 20000, max 50000:
`scale = 30000/65533 ≈ 0.45778`, `offset ≈ 19999.542`; raw 1 → 20000, raw 65534 → 50000.

## Flag label definitions (authoritative, from firmware enums)

`rear_controller_status.bps_fault` (bits 0–10, `BpsFault`):

| bit | label | bit | label |
|----|-------|----|-------|
| 0 | OVERVOLTAGE | 6 | OVERCURRENT |
| 1 | UNBALANCE | 7 | UNDERVOLTAGE |
| 2 | OVERTEMP_AMBIENT | 8 | KILLSWITCH |
| 3 | COMMS_LOSS_AFE | 9 | RELAY_CLOSE_FAILED |
| 4 | COMMS_LOSS_CURR_SENSE | 10 | DISCONNECTED |
| 5 | OVERTEMP_CELL | | |

WS22 flags, from `Ws22MotorFlags` (single 16-bit space: error 0–8, limit 9–15).
**Open question:** the two CAN signals `error_flags` (bits 0-15) and `limit_flags`
(bits 0-15) are separate 16-bit registers, but the firmware enum packs both into
one 0–15 space. Confirm the on-wire layout before shipping. As implemented:

- `ws22_status.error_flags` — bits 0-8: HW_OVERCURRENT, SW_OVERCURRENT,
  DC_BUS_OVERVOLTAGE, BAD_HALL_SEQUENCE, WATCHDOG_RESET, (bit 5 unused),
  bit 6 = CFG_READ_ERR / UVLO (firmware enum defines both at bit 6 — likely a
  typo, one is probably bit 5), DESATURATION_FAULT, MOTOR_OVER_SPEED.
- `ws22_status.limit_flags` — bits 9-15: OUTPUT_VOLTAGE_PWM, MOTOR_CURRENT,
  VELOCITY, BUS_CURRENT, BUS_VOLTAGE_UPPER, BUS_VOLTAGE_LOWER, TEMPERATURE.

## Implementation steps (what was done / to redo)

### 1. `scripts/tools/global_can_gen.py`
- In `flattened_signals`, carry through `type`, `min`, `max` for non-bitfield signals.
- Add `apply_float_quantization(signals)` (uses the decode formula above) and call
  it after flattening, before the postprocess-rule loop (so an explicit rule can
  still override). Emit `scale`, `offset`, `min`, `max`; strip `type`.
- In the postprocess loop, pass through a new `flags` rule:
  `sig["flags"] = {str(k): str(v) for k, v in rule["flags"].items()}`.

### 2. `can/postprocess.yaml`
- Add `flags:` (bit → label) rules for `rear_controller_status.bps_fault`,
  `ws22_status.error_flags`, `ws22_status.limit_flags` (labels above).
- Float fields need no rule (handled by the generator).

### 3. `src/decoder/can_config.rs`
- `SignalDef`: add `offset: Option<f64>`, `min: Option<f64>`, `max: Option<f64>`,
  `flags: Option<HashMap<String,String>>` (all `#[serde(default)]`).
- Add `pub type FlagLookup = HashMap<(String,String), Vec<(u32,String)>>` and
  `build_flag_lookup(messages)` (parse/sort bit indices).

### 4. `src/decoder/mod.rs`
- Decode: `value = raw*scale.unwrap_or(1.0) + offset.unwrap_or(0.0)`, then
  `if let (Some(lo),Some(hi)) = (sig.min, sig.max) { value = value.clamp(lo,hi) }`.

### 5. `src/app.rs`
- Add `flag_lookup: FlagLookup` field; build in `new()` and rebuild in `connect()`
  alongside `enum_lookup`; pass into `dashboard.show(...)`.

### 6. `src/tabs/dashboard/mod.rs`
- Thread `flag_lookup` through `show` → `maybe_refresh` → `RenderCache::build`.

### 7. `src/tabs/dashboard/charts.rs`
- `RenderCache`: add `flags: HashMap<String, Vec<FlagState>>` (+ `struct FlagState { label, set }`).
- `build`: take `flag_lookup`; in the `Status` arm, if a signal is in `flag_lookup`
  decode each labeled bit's set/clear state; else fall back to enum label.
- `draw_status`: for flag signals render a labeled LED grid (chip = colored square
  + label, red when set, with an "OK / N active" summary); enum/plain signals stay
  as a text row.

### 8. `default_setup.json`
- `vehicle_status` panel: add `ws22_status.error_flags`, `ws22_status.limit_flags`
  to `signals`; widen (`"width": "full"`) to fit the LED grids.

### 9. Regenerate / overlay `can/global_can.yaml`
- Preferred once firmware is in sync: `python scripts/tools/global_can_gen.py`.
- Until then: overlay onto the working file by signal name, recomputing float
  scale/offset against the working file's bit lengths (a throwaway overlay script
  did this: 39 float signals + 3 bitfields).
- Copy the result to `target/debug/global_can.yaml` (the app reads from exe dir).

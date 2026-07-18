use crate::decoder::can_config::{self, EnumLookup, FlagLookup};
use egui_plot::{Bar, BarChart, Line, Plot, PlotPoints, Points};
use std::collections::{BTreeMap, HashMap};

use super::config::{ChartType, FieldConfig, PanelConfig};

pub type DataCache = HashMap<String, Vec<[f64; 2]>>;  // key: "msg.sig", val: [secs_offset, value]

// Decimation / binning targets. Chosen once at refresh cadence, not per frame.
const LINE_TARGET:    usize = 2000; // ~worst-case horizontal pixels; independent of window
const SCATTER_TARGET: usize = 4000;

// Solid accent used for all meter/pedal fills (per the "solid accent" style).
const ACCENT: egui::Color32 = egui::Color32::from_rgb(96, 165, 250);
const REGEN_ON:  egui::Color32 = egui::Color32::from_rgb(90, 200, 90);   // green: regenerating
const REGEN_OFF: egui::Color32 = egui::Color32::from_rgb(232, 200, 72);  // yellow: regen off

// ── Render cache ──────────────────────────────────────────────────────────────

/// Render-ready artifacts derived from the raw `DataCache`, rebuilt in
/// `maybe_refresh` (refresh cadence) rather than in the draw path (per frame).
/// The draw functions below only read from this — they never touch raw points,
/// clone full vectors, or rebin on draw.
/// Window aggregates for one signal (stat tiles).
struct SigStats {
    last: f64,
    min:  f64,
    max:  f64,
    mean: f64,
}

#[derive(Default)]
pub struct RenderCache {
    lines:   HashMap<String, Vec<[f64; 2]>>,   // decimated time series, key "msg.sig"
    hists:   HashMap<String, Vec<(f64, f64)>>, // (bin_center, count), key "msg.sig"
    scatter: HashMap<String, Vec<[f64; 2]>>,   // paired (x_val, y_val), key panel.id
    latest:  HashMap<String, f64>,             // latest value per signal
    labels:  HashMap<String, String>,          // enum label of latest value (status panels)
    stats:   HashMap<String, SigStats>,        // window aggregates (stat tiles)
    regen:   HashMap<String, bool>,            // pedal panel id → regenerating?
}

impl RenderCache {
    /// Build all render artifacts for the given panels from the raw cache.
    pub fn build(
        raw: &DataCache,
        panels: &[PanelConfig],
        enum_lookup: &EnumLookup,
        flag_lookup: &FlagLookup,
    ) -> Self {
        let mut rc = RenderCache::default();
        for panel in panels {
            match panel.chart_type {
                ChartType::Line => {
                    for key in &panel.signals {
                        // Latest value feeds the readout box beside the plot.
                        if let Some(v) = raw.get(key).and_then(|p| p.last()).map(|p| p[1]) {
                            rc.latest.insert(key.clone(), v);
                        }
                        if rc.lines.contains_key(key) { continue; }
                        if let Some(pts) = raw.get(key) {
                            rc.lines.insert(key.clone(), decimate_minmax(pts, LINE_TARGET));
                        }
                    }
                }
                ChartType::Gauge => {
                    if let Some(key) = panel.signals.first() {
                        if let Some(v) = raw.get(key).and_then(|p| p.last()).map(|p| p[1]) {
                            rc.latest.insert(key.clone(), v);
                        }
                    }
                }
                ChartType::Numeric => {
                    // Numeric readouts show the latest value of each listed signal.
                    for key in &panel.signals {
                        if let Some(v) = raw.get(key).and_then(|p| p.last()).map(|p| p[1]) {
                            rc.latest.insert(key.clone(), v);
                        }
                    }
                }
                ChartType::Bar => {
                    // Bar indicators (e.g. throttle) show the latest of the first signal.
                    if let Some(key) = panel.signals.first() {
                        if let Some(v) = raw.get(key).and_then(|p| p.last()).map(|p| p[1]) {
                            rc.latest.insert(key.clone(), v);
                        }
                    }
                }
                ChartType::Meters => {
                    // Latest value of every field signal; ranges live in the config.
                    for f in &panel.fields {
                        if let Some(v) = raw.get(&f.signal).and_then(|p| p.last()).map(|p| p[1]) {
                            rc.latest.insert(f.signal.clone(), v);
                        }
                    }
                    // Also allow bare `signals` (uses panel gauge range) for convenience.
                    for key in &panel.signals {
                        if let Some(v) = raw.get(key).and_then(|p| p.last()).map(|p| p[1]) {
                            rc.latest.insert(key.clone(), v);
                        }
                    }
                }
                ChartType::Pedals => {
                    for key in &panel.signals {
                        if let Some(v) = raw.get(key).and_then(|p| p.last()).map(|p| p[1]) {
                            rc.latest.insert(key.clone(), v);
                        }
                    }
                    // Regen state from the (optional) 3rd signal: drive_state.
                    // Prefer the enum label == REGEN; fall back to the raw value 6.
                    let regen = panel.signals.get(2).map(|key| {
                        match raw.get(key).and_then(|p| p.last()).map(|p| p[1]) {
                            Some(v) => {
                                let label = PanelConfig::signal_parts(key)
                                    .and_then(|(m, s)| enum_lookup.get(&(m.to_string(), s.to_string())))
                                    .and_then(|map| map.get(&(v as i64).to_string()));
                                match label {
                                    Some(l) => l.eq_ignore_ascii_case("regen"),
                                    None    => v as i64 == 6,
                                }
                            }
                            None => false,
                        }
                    }).unwrap_or(false);
                    rc.regen.insert(panel.id.clone(), regen);
                }
                ChartType::Status => {
                    // Latest value per signal plus its display label, resolved here so
                    // the draw path stays lookup-free. Bitfield signals list every
                    // active flag; enum signals map to a single label.
                    for key in &panel.signals {
                        let Some(v) = raw.get(key).and_then(|p| p.last()).map(|p| p[1]) else { continue };
                        rc.latest.insert(key.clone(), v);
                        let parts = PanelConfig::signal_parts(key);
                        if let Some(bits) = parts
                            .and_then(|(m, s)| flag_lookup.get(&(m.to_string(), s.to_string())))
                        {
                            rc.labels.insert(key.clone(), can_config::format_flags(v, bits));
                            continue;
                        }
                        let label = parts
                            .and_then(|(m, s)| enum_lookup.get(&(m.to_string(), s.to_string())))
                            .and_then(|map| map.get(&(v as i64).to_string()));
                        if let Some(l) = label {
                            rc.labels.insert(key.clone(), l.clone());
                        }
                    }
                }
                ChartType::Leds | ChartType::Bars => {
                    // Both render the latest value of every listed signal.
                    for key in &panel.signals {
                        if let Some(v) = raw.get(key).and_then(|p| p.last()).map(|p| p[1]) {
                            rc.latest.insert(key.clone(), v);
                        }
                    }
                }
                ChartType::Matrix => {
                    // Latest value of every cell signal across all rows.
                    for row in &panel.rows {
                        for cell in &row.cells {
                            if let Some(v) = raw.get(&cell.signal).and_then(|p| p.last()).map(|p| p[1]) {
                                rc.latest.insert(cell.signal.clone(), v);
                            }
                        }
                    }
                }
                ChartType::Stat => {
                    for key in &panel.signals {
                        if let Some(pts) = raw.get(key) {
                            if let Some(s) = sig_stats(pts) {
                                rc.stats.insert(key.clone(), s);
                            }
                        }
                    }
                }
                ChartType::Histogram => {
                    if let Some(key) = panel.signals.first() {
                        if let Some(pts) = raw.get(key) {
                            rc.hists.insert(key.clone(), bin_histogram(pts));
                        }
                    }
                }
                ChartType::Scatter => {
                    if let (Some(xk), Some(yk)) = (panel.signals.first(), panel.signals.get(1)) {
                        if let (Some(xp), Some(yp)) = (raw.get(xk), raw.get(yk)) {
                            let n = xp.len().min(yp.len());
                            // Pair by index, so both series must be decimated together —
                            // uniform stride keeps the (x,y) correspondence intact.
                            let pairs: Vec<[f64; 2]> =
                                (0..n).map(|i| [xp[i][1], yp[i][1]]).collect();
                            rc.scatter.insert(panel.id.clone(), stride_decimate(pairs, SCATTER_TARGET));
                        }
                    }
                }
            }
        }
        rc
    }
}

/// Reduce to ~target points, keeping the min & max of each x-bucket so
/// transient peaks/troughs survive (unlike naive stride sampling). Assumes
/// `pts` is sorted ascending by x (the query orders by ts_ms).
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

/// Uniform-stride reduction that preserves index pairing (for scatter).
fn stride_decimate(pts: Vec<[f64; 2]>, target: usize) -> Vec<[f64; 2]> {
    if pts.len() <= target || target == 0 { return pts; }
    let step = pts.len() as f64 / target as f64;
    (0..target).map(|i| pts[(i as f64 * step) as usize]).collect()
}

/// Window aggregates over a signal's points; `None` if there are no points.
fn sig_stats(pts: &[[f64; 2]]) -> Option<SigStats> {
    let last = pts.last()?[1];
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut sum = 0.0;
    for p in pts {
        min = min.min(p[1]);
        max = max.max(p[1]);
        sum += p[1];
    }
    Some(SigStats { last, min, max, mean: sum / pts.len() as f64 })
}

/// Bin values into one integer bucket per value (matches the prior draw-time
/// behavior), returning (bin_center, count) pairs sorted by center.
fn bin_histogram(pts: &[[f64; 2]]) -> Vec<(f64, f64)> {
    let mut bins: BTreeMap<i64, usize> = BTreeMap::new();
    for p in pts {
        *bins.entry(p[1] as i64).or_default() += 1;
    }
    bins.into_iter().map(|(x, c)| (x as f64, c as f64)).collect()
}

// ── Layout helpers ──────────────────────────────────────────────────────────

/// Natural body height for a panel, so table-style widgets grow with their row
/// count while plots keep a fixed comfortable height.
pub fn panel_height(panel: &PanelConfig) -> f32 {
    match panel.chart_type {
        ChartType::Line | ChartType::Scatter | ChartType::Histogram | ChartType::Bars => 240.0,
        ChartType::Gauge => 200.0,
        ChartType::Leds  => 200.0,
        ChartType::Pedals => 210.0,
        ChartType::Bar    => 120.0,
        ChartType::Numeric => {
            let n = panel.signals.len().max(1) as f32;
            (n * 74.0).clamp(120.0, 340.0)
        }
        ChartType::Stat => {
            let n = panel.signals.len().max(1) as f32;
            (n * 92.0).clamp(120.0, 360.0)
        }
        ChartType::Status => {
            let n = panel.signals.len().max(1) as f32;
            (n * 30.0 + 14.0).clamp(56.0, 360.0)
        }
        ChartType::Matrix => {
            // One row per data row plus the header row.
            let n = (panel.rows.len() + 1).max(1) as f32;
            (n * 30.0 + 14.0).clamp(56.0, 360.0)
        }
        ChartType::Meters => {
            let n = meter_row_count(panel).max(1) as f32;
            (n * 28.0 + 10.0).clamp(56.0, 460.0)
        }
    }
}

fn meter_row_count(panel: &PanelConfig) -> usize {
    if !panel.fields.is_empty() { panel.fields.len() } else { panel.signals.len() }
}

// ── Public entry point ────────────────────────────────────────────────────────

pub fn draw_panel(ui: &mut egui::Ui, panel: &PanelConfig, render: &RenderCache, height: f32) {
    // Table-style widgets read best inside a soft rounded card; plots stay
    // frameless so nothing draws that harsh dark box around them.
    let card = matches!(
        panel.chart_type,
        ChartType::Meters | ChartType::Pedals | ChartType::Status
            | ChartType::Numeric | ChartType::Stat | ChartType::Leds | ChartType::Matrix
    );
    let frame = if card {
        egui::Frame::none()
            .fill(ui.visuals().faint_bg_color)
            .rounding(egui::Rounding::same(6.0))
            .inner_margin(egui::Margin::same(8.0))
    } else {
        egui::Frame::none()
    };

    frame.show(ui, |ui| {
        ui.set_min_height(height);
        match panel.chart_type {
            ChartType::Line      => draw_line(ui, panel, render, height),
            ChartType::Gauge     => draw_gauge(ui, panel, render, height),
            ChartType::Histogram => draw_histogram(ui, panel, render, height),
            ChartType::Scatter   => draw_scatter(ui, panel, render, height),
            ChartType::Numeric   => draw_numeric(ui, panel, render, height),
            ChartType::Bar       => draw_bar(ui, panel, render, height),
            ChartType::Status    => draw_status(ui, panel, render, height),
            ChartType::Leds      => draw_leds(ui, panel, render, height),
            ChartType::Bars      => draw_bars(ui, panel, render, height),
            ChartType::Stat      => draw_stat(ui, panel, render, height),
            ChartType::Meters    => draw_meters(ui, panel, render, height),
            ChartType::Pedals    => draw_pedals(ui, panel, render, height),
            ChartType::Matrix    => draw_matrix(ui, panel, render, height),
        }
    });
}

// ── Line chart ────────────────────────────────────────────────────────────────

/// Fixed (non-interactive) line plot that auto-fits its bounds to the data as
/// new points arrive, with a compact live-value readout pinned to its right.
fn draw_line(ui: &mut egui::Ui, panel: &PanelConfig, render: &RenderCache, height: f32) {
    let readout_w = 104.0_f32;
    let gap       = 8.0_f32;

    ui.horizontal_top(|ui| {
        let plot_w = (ui.available_width() - readout_w - gap).max(120.0);

        let mut plot = Plot::new(&panel.id)
            .width(plot_w)
            .height(height)
            .show_background(false)          // no dark canvas box
            .allow_drag(false)               // fixed: can't pan…
            .allow_zoom(false)               // …or zoom…
            .allow_scroll(false)             // …or scroll…
            .allow_boxed_zoom(false)
            .auto_bounds([true, true].into()) // …but always fits the current data
            .legend(egui_plot::Legend::default());

        // Predefined y-range: force these endpoints into view so the axis holds a
        // fixed span, while auto-bounds still lets it grow to fit out-of-range data.
        if let Some([y_min, y_max]) = panel.y_range {
            plot = plot.include_y(y_min).include_y(y_max);
        }

        plot.show(ui, |plot_ui| {
                for (i, sig_key) in panel.signals.iter().enumerate() {
                    if let Some(pts) = render.lines.get(sig_key) {
                        // pts is already decimated (<= ~LINE_TARGET), so this clone is cheap.
                        let pp = PlotPoints::new(pts.clone());
                        let label = sig_key.rsplit('.').next().unwrap_or(sig_key);
                        let mut line = Line::new(pp).name(label);
                        // Explicit per-signal color if configured; else egui auto-assigns.
                        if let Some(c) = panel.colors.get(i).and_then(|s| parse_color(s)) {
                            line = line.color(c);
                        }
                        plot_ui.line(line);
                    }
                }
            });

        // Live-value readout: current value of every signal in the plot.
        ui.allocate_ui_with_layout(
            egui::vec2(readout_w, height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.add_space(4.0);
                for sig_key in &panel.signals {
                    let name = sig_key.rsplit('.').next().unwrap_or(sig_key);
                    let text = render.latest.get(sig_key).map(|&v| format_number(v))
                        .unwrap_or_else(|| "—".to_string());
                    ui.label(egui::RichText::new(name).size(11.0).weak());
                    ui.label(egui::RichText::new(text).size(19.0).strong());
                    ui.add_space(8.0);
                }
            },
        );
    });
}

// ── Gauge ─────────────────────────────────────────────────────────────────────

fn draw_gauge(ui: &mut egui::Ui, panel: &PanelConfig, render: &RenderCache, height: f32) {
    let sig_key = panel.signals.first().map(|s| s.as_str()).unwrap_or("");
    let latest  = render.latest.get(sig_key).copied().unwrap_or(0.0);

    let min  = panel.gauge.as_ref().map(|g| g.min).unwrap_or(0.0);
    let max  = panel.gauge.as_ref().map(|g| g.max).unwrap_or(100.0);
    let unit = panel.gauge.as_ref().map(|g| g.unit.as_str()).unwrap_or("");

    let (response, painter) = ui.allocate_painter(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let rect = response.rect;

    // Radius that fits the rect; leave room for labels below endpoints
    let radius    = (rect.width() / 2.8).min((rect.height() - 50.0) / 1.5).max(20.0);
    let stroke_w  = (radius * 0.16).clamp(4.0, 20.0);
    let center    = egui::pos2(rect.center().x, rect.top() + 24.0 + radius);

    // Arc: 5π/6 (lower-left, 8 o'clock) → sweep +4π/3 clockwise → π/6 (lower-right, 4 o'clock)
    // Midpoint at 3π/2 = top of arc.  In egui (y-down) increasing angle = clockwise on screen.
    let arc_start = std::f32::consts::PI * 5.0 / 6.0;
    let arc_sweep = std::f32::consts::PI * 4.0 / 3.0;

    let t = ((latest - min) / (max - min)).clamp(0.0, 1.0) as f32;

    // Background (gray) arc
    draw_arc(&painter, center, radius, arc_start, arc_sweep, stroke_w,
             egui::Color32::from_gray(55));
    // Filled arc
    if t > 0.001 {
        draw_arc(&painter, center, radius, arc_start, arc_sweep * t, stroke_w,
                 gauge_color(t));
    }

    // Needle-tip dot
    let tip_angle = arc_start + arc_sweep * t;
    let tip = center + egui::vec2(tip_angle.cos(), tip_angle.sin()) * radius;
    painter.circle_filled(tip, stroke_w * 0.55, egui::Color32::WHITE);

    // Value text
    painter.text(
        center + egui::vec2(0.0, radius * 0.18),
        egui::Align2::CENTER_CENTER,
        format!("{:.0}{}", latest, if unit.is_empty() { "".into() } else { format!(" {unit}") }),
        egui::FontId::proportional((radius * 0.34).max(12.0)),
        ui.visuals().text_color(),
    );

    // Min / max labels at arc endpoints
    let end_angle = arc_start + arc_sweep;
    let label_r   = radius + stroke_w * 1.1;
    let min_pos   = center + egui::vec2(arc_start.cos(), arc_start.sin()) * label_r;
    let max_pos   = center + egui::vec2(end_angle.cos(), end_angle.sin()) * label_r;
    let font      = egui::FontId::proportional(11.0);
    painter.text(min_pos, egui::Align2::CENTER_CENTER, format!("{min:.0}"), font.clone(), egui::Color32::GRAY);
    painter.text(max_pos, egui::Align2::CENTER_CENTER, format!("{max:.0}"), font, egui::Color32::GRAY);
}

fn gauge_color(t: f32) -> egui::Color32 {
    if t < 0.5 {
        let r = (t * 2.0 * 200.0) as u8;
        egui::Color32::from_rgb(r, 200, 50)
    } else {
        let g = ((1.0 - t) * 2.0 * 200.0) as u8;
        egui::Color32::from_rgb(200, g, 50)
    }
}

fn draw_arc(
    painter:     &egui::Painter,
    center:      egui::Pos2,
    radius:      f32,
    start_angle: f32,
    sweep:       f32,
    stroke_w:    f32,
    color:       egui::Color32,
) {
    if sweep.abs() < 0.001 { return; }
    let segs = ((sweep.abs() * radius / 3.0).ceil() as usize).clamp(8, 300);
    let pts: Vec<egui::Pos2> = (0..=segs)
        .map(|i| {
            let a = start_angle + sweep * (i as f32 / segs as f32);
            center + egui::vec2(a.cos(), a.sin()) * radius
        })
        .collect();
    for w in pts.windows(2) {
        painter.line_segment([w[0], w[1]], egui::Stroke::new(stroke_w, color));
    }
}

// ── Numeric readout ─────────────────────────────────────────────────────────

/// Big-number panel: shows the latest value of each listed signal as text.
fn draw_numeric(ui: &mut egui::Ui, panel: &PanelConfig, render: &RenderCache, height: f32) {
    let n = panel.signals.len().max(1);
    // Scale the value text to fill the panel; shrink as more signals stack.
    let value_size = (height / n as f32 * 0.42).clamp(16.0, 72.0);
    let label_size = (value_size * 0.32).clamp(10.0, 20.0);
    let gap        = value_size * 0.25;

    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), height),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            // Roughly vertically center the stack of readouts.
            let content_h = (value_size + label_size + gap) * n as f32;
            ui.add_space(((height - content_h) * 0.5).max(4.0));

            for sig_key in &panel.signals {
                let label = sig_key.rsplit('.').next().unwrap_or(sig_key);
                let text = match render.latest.get(sig_key) {
                    Some(&v) => format_number(v),
                    None     => "—".to_string(),
                };
                ui.label(egui::RichText::new(text).size(value_size).strong());
                ui.label(egui::RichText::new(label).size(label_size).weak());
                ui.add_space(gap);
            }
        },
    );
}

/// Parse a color spec from the config: a `#RRGGBB` / `#RRGGBBAA` hex string or
/// one of a small set of named colors. Returns `None` for anything unrecognized,
/// so the caller falls back to egui's auto-assigned line color.
fn parse_color(s: &str) -> Option<egui::Color32> {
    let t = s.trim();
    if let Some(hex) = t.strip_prefix('#') {
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .filter_map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
            .collect();
        return match bytes.len() {
            3 => Some(egui::Color32::from_rgb(bytes[0], bytes[1], bytes[2])),
            4 => Some(egui::Color32::from_rgba_unmultiplied(bytes[0], bytes[1], bytes[2], bytes[3])),
            _ => None,
        };
    }
    // Named colors chosen to read on both light and dark backgrounds.
    let c = match t.to_ascii_lowercase().as_str() {
        "red"     => egui::Color32::from_rgb(229, 72, 72),
        "orange"  => egui::Color32::from_rgb(240, 150, 50),
        "yellow"  => egui::Color32::from_rgb(232, 200, 72),
        "green"   => egui::Color32::from_rgb(90, 200, 90),
        "blue"    => egui::Color32::from_rgb(96, 165, 250),
        "cyan"    => egui::Color32::from_rgb(70, 190, 200),
        "purple"  => egui::Color32::from_rgb(170, 110, 220),
        "magenta" => egui::Color32::from_rgb(220, 90, 200),
        "gray" | "grey" => egui::Color32::from_rgb(150, 150, 150),
        "white"   => egui::Color32::from_rgb(230, 230, 230),
        _ => return None,
    };
    Some(c)
}

/// Format a value for display, trimming trailing zeros (e.g. 3.500 -> "3.5").
fn format_number(v: f64) -> String {
    let s = format!("{:.3}", v);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

// ── Bar indicator ─────────────────────────────────────────────────────────────

/// Horizontal fill bar for a single value (e.g. throttle %). Uses the panel's
/// gauge min/max/unit config for the range and label.
fn draw_bar(ui: &mut egui::Ui, panel: &PanelConfig, render: &RenderCache, height: f32) {
    let sig_key = panel.signals.first().map(|s| s.as_str()).unwrap_or("");
    let value   = render.latest.get(sig_key).copied().unwrap_or(0.0);

    let min  = panel.gauge.as_ref().map(|g| g.min).unwrap_or(0.0);
    let max  = panel.gauge.as_ref().map(|g| g.max).unwrap_or(100.0);
    let unit = panel.gauge.as_ref().map(|g| g.unit.as_str()).unwrap_or("");

    let (response, painter) = ui.allocate_painter(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let rect = response.rect;

    // Bar track geometry: a centered horizontal bar with padding.
    let pad     = 24.0_f32.min(rect.width() * 0.1);
    let bar_h   = (rect.height() * 0.28).clamp(16.0, 64.0);
    let track   = egui::Rect::from_min_max(
        egui::pos2(rect.left() + pad, rect.center().y - bar_h / 2.0),
        egui::pos2(rect.right() - pad, rect.center().y + bar_h / 2.0),
    );
    let rounding = egui::Rounding::same(bar_h * 0.25);

    // Track background
    painter.rect_filled(track, rounding, egui::Color32::from_gray(50));

    // Filled portion
    let t = if (max - min).abs() < f64::EPSILON {
        0.0
    } else {
        ((value - min) / (max - min)).clamp(0.0, 1.0) as f32
    };
    if t > 0.0 {
        let fill = egui::Rect::from_min_max(
            track.min,
            egui::pos2(track.left() + track.width() * t, track.bottom()),
        );
        painter.rect_filled(fill, rounding, gauge_color(t));
    }

    // Value text centered over the bar
    painter.text(
        track.center(),
        egui::Align2::CENTER_CENTER,
        format!("{}{}", format_number(value), if unit.is_empty() { String::new() } else { format!(" {unit}") }),
        egui::FontId::proportional((bar_h * 0.55).max(11.0)),
        egui::Color32::WHITE,
    );

    // Min / max endpoint labels beneath the track
    let font = egui::FontId::proportional(11.0);
    painter.text(
        egui::pos2(track.left(), track.bottom() + 10.0),
        egui::Align2::LEFT_CENTER,
        format!("{min:.0}"),
        font.clone(),
        egui::Color32::GRAY,
    );
    painter.text(
        egui::pos2(track.right(), track.bottom() + 10.0),
        egui::Align2::RIGHT_CENTER,
        format!("{max:.0}"),
        font,
        egui::Color32::GRAY,
    );
}

// ── Meters: generic label | value | bar table ──────────────────────────────────

/// One resolved meter row: display label, latest value, its range and unit.
struct MeterRow<'a> {
    label: &'a str,
    key:   &'a str,
    min:   f64,
    max:   f64,
    unit:  &'a str,
}

/// Resolve the rows for a Meters panel: prefer the rich `fields` list, else
/// fall back to `signals` using the panel-level gauge range.
fn meter_rows<'a>(panel: &'a PanelConfig) -> Vec<MeterRow<'a>> {
    let (gmin, gmax, gunit) = panel.gauge.as_ref()
        .map(|g| (g.min, g.max, g.unit.as_str()))
        .unwrap_or((0.0, 100.0, ""));

    if !panel.fields.is_empty() {
        panel.fields.iter().map(|f: &FieldConfig| MeterRow {
            label: f.label.as_deref()
                .unwrap_or_else(|| f.signal.rsplit('.').next().unwrap_or(&f.signal)),
            key:   &f.signal,
            min:   f.min.unwrap_or(gmin),
            max:   f.max.unwrap_or(gmax),
            unit:  f.unit.as_deref().unwrap_or(gunit),
        }).collect()
    } else {
        panel.signals.iter().map(|s| MeterRow {
            label: s.rsplit('.').next().unwrap_or(s),
            key:   s,
            min:   gmin,
            max:   gmax,
            unit:  gunit,
        }).collect()
    }
}

/// Generic table: each row is `label | value | bar`. Bar is a solid accent fill
/// clamped to the row's range; the text always shows the true value.
fn draw_meters(ui: &mut egui::Ui, panel: &PanelConfig, render: &RenderCache, height: f32) {
    let rows = meter_rows(panel);
    let n    = rows.len().max(1);
    let row_h = (height / n as f32).clamp(20.0, 40.0);

    let text_col  = ui.visuals().text_color();
    let track_col = if ui.visuals().dark_mode {
        egui::Color32::from_gray(60)
    } else {
        egui::Color32::from_gray(205)
    };

    for row in &rows {
        let (resp, painter) = ui.allocate_painter(
            egui::vec2(ui.available_width(), row_h),
            egui::Sense::hover(),
        );
        let rect = resp.rect;

        // Columns: label (left) | value (mid) | bar (right, fixed-ish width).
        let bar_w  = (rect.width() * 0.40).clamp(50.0, 200.0);
        let colgap = 8.0_f32;

        let bar_area = egui::Rect::from_min_max(
            egui::pos2(rect.right() - bar_w, rect.top()),
            rect.max,
        );
        let val_right = bar_area.left() - colgap;

        let font       = egui::FontId::proportional((row_h * 0.42).clamp(11.0, 15.0));
        let value_font = egui::FontId::proportional((row_h * 0.46).clamp(11.0, 16.0));

        // Label (left, vertically centered)
        painter.text(
            egui::pos2(rect.left(), rect.center().y),
            egui::Align2::LEFT_CENTER,
            row.label,
            font.clone(),
            text_col,
        );

        // Value text (right-aligned before the bar); shows the true value.
        let value = render.latest.get(row.key).copied();
        let value_text = match value {
            Some(v) => if row.unit.is_empty() {
                format_number(v)
            } else {
                format!("{} {}", format_number(v), row.unit)
            },
            None => "—".to_string(),
        };
        painter.text(
            egui::pos2(val_right, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            value_text,
            value_font,
            text_col,
        );

        // Bar track + solid-accent fill, clamped to [min, max].
        let bar_h    = (row_h * 0.42).clamp(8.0, 16.0);
        let track    = egui::Rect::from_center_size(
            egui::pos2(bar_area.center().x, rect.center().y),
            egui::vec2(bar_w, bar_h),
        );
        let rounding = egui::Rounding::same(bar_h * 0.5);
        painter.rect_filled(track, rounding, track_col);

        if let Some(v) = value {
            let span = row.max - row.min;
            let t = if span.abs() < f64::EPSILON {
                0.0
            } else {
                ((v - row.min) / span).clamp(0.0, 1.0) as f32
            };
            if t > 0.0 {
                let fill = egui::Rect::from_min_max(
                    track.min,
                    egui::pos2(track.left() + track.width() * t, track.bottom()),
                );
                painter.rect_filled(fill, rounding, ACCENT);
            }
        }
    }
}

// ── Pedals: throttle + brake ────────────────────────────────────────────────────

/// Two vertical fill bars — throttle and brake — in one widget. The brake bar
/// is tinted green while regenerating (drive_state == REGEN) and yellow when
/// regen is off. Convention: signals = [throttle, brake, (optional) drive_state].
fn draw_pedals(ui: &mut egui::Ui, panel: &PanelConfig, render: &RenderCache, height: f32) {
    let throttle = panel.signals.first().and_then(|k| render.latest.get(k)).copied().unwrap_or(0.0);
    let brake    = panel.signals.get(1).and_then(|k| render.latest.get(k)).copied().unwrap_or(0.0);
    let regen    = render.regen.get(&panel.id).copied().unwrap_or(false);

    let min = panel.gauge.as_ref().map(|g| g.min).unwrap_or(0.0);
    let max = panel.gauge.as_ref().map(|g| g.max).unwrap_or(100.0);

    let text_col  = ui.visuals().text_color();
    let track_col = if ui.visuals().dark_mode {
        egui::Color32::from_gray(60)
    } else {
        egui::Color32::from_gray(205)
    };

    let (resp, painter) = ui.allocate_painter(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let rect = resp.rect;

    let brake_col = if regen { REGEN_ON } else { REGEN_OFF };
    let cols: [(&str, f64, egui::Color32); 2] = [
        ("Throttle", throttle, ACCENT),
        ("Brake",    brake,    brake_col),
    ];

    let label_h = 18.0_f32;
    let value_h = 18.0_f32;
    let track_top    = rect.top() + value_h + 4.0;
    let track_bottom = rect.bottom() - label_h;
    let track_h      = (track_bottom - track_top).max(20.0);

    let slot_w = rect.width() / cols.len() as f32;
    let bar_w  = slot_w.min(64.0).max(24.0);

    for (i, (name, value, color)) in cols.iter().enumerate() {
        let cx = rect.left() + slot_w * (i as f32 + 0.5);

        let track = egui::Rect::from_min_max(
            egui::pos2(cx - bar_w / 2.0, track_top),
            egui::pos2(cx + bar_w / 2.0, track_bottom),
        );
        let rounding = egui::Rounding::same(6.0);
        painter.rect_filled(track, rounding, track_col);

        let span = max - min;
        let t = if span.abs() < f64::EPSILON {
            0.0
        } else {
            ((value - min) / span).clamp(0.0, 1.0) as f32
        };
        if t > 0.0 {
            let fill = egui::Rect::from_min_max(
                egui::pos2(track.left(), track.bottom() - track_h * t),
                track.max,
            );
            painter.rect_filled(fill, rounding, *color);
        }

        // Value above the bar
        painter.text(
            egui::pos2(cx, rect.top() + value_h * 0.5),
            egui::Align2::CENTER_CENTER,
            format!("{}%", format_number(*value)),
            egui::FontId::proportional(14.0),
            text_col,
        );
        // Name below the bar
        painter.text(
            egui::pos2(cx, track_bottom + label_h * 0.5),
            egui::Align2::CENTER_CENTER,
            *name,
            egui::FontId::proportional(12.0),
            text_col,
        );
    }
}

// ── Status: text table ──────────────────────────────────────────────────────────

/// One `field  →  value` row per signal in an aligned two-column table. Uses the
/// enum label (e.g. drive_state → "DRIVE") when the CAN config defines one,
/// otherwise the numeric value.
fn draw_status(ui: &mut egui::Ui, panel: &PanelConfig, render: &RenderCache, _height: f32) {
    egui::Grid::new(&panel.id)
        .num_columns(2)
        .spacing(egui::vec2(18.0, 8.0))
        .striped(true)
        .show(ui, |ui| {
            for sig_key in &panel.signals {
                let name = pretty_name(sig_key.rsplit('.').next().unwrap_or(sig_key));
                let value = render.labels.get(sig_key).cloned()
                    .or_else(|| render.latest.get(sig_key).map(|&v| format_number(v)))
                    .unwrap_or_else(|| "—".to_string());

                ui.label(egui::RichText::new(name).weak());
                ui.label(egui::RichText::new(value).strong());
                ui.end_row();
            }
        });
}

// ── Matrix: 2-D value grid ────────────────────────────────────────────────────

/// A grid of latest values: a header row of `columns`, then one row per
/// `MatrixRow`. Each cell shows its signal's latest value with an optional unit,
/// or "—" when there's no sample. Cells map positionally to the columns.
fn draw_matrix(ui: &mut egui::Ui, panel: &PanelConfig, render: &RenderCache, _height: f32) {
    let ncols = panel.columns.len();
    egui::Grid::new(&panel.id)
        .num_columns(ncols + 1)
        .spacing(egui::vec2(18.0, 8.0))
        .striped(true)
        .show(ui, |ui| {
            // Header: empty corner cell, then the column labels.
            ui.label("");
            for col in &panel.columns {
                ui.label(egui::RichText::new(col).strong());
            }
            ui.end_row();

            for row in &panel.rows {
                ui.label(egui::RichText::new(&row.label).weak());
                for c in 0..ncols {
                    let text = match row.cells.get(c) {
                        Some(cell) => match render.latest.get(&cell.signal) {
                            Some(&v) => match &cell.unit {
                                Some(u) => format!("{} {}", format_number(v), u),
                                None    => format_number(v),
                            },
                            None => "—".to_string(),
                        },
                        None => "—".to_string(),
                    };
                    ui.label(egui::RichText::new(text).strong());
                }
                ui.end_row();
            }
        });
}

/// Turn a snake_case signal name into a spaced Title Case label.
fn pretty_name(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, word) in s.split('_').enumerate() {
        if i > 0 { out.push(' '); }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

// ── LED grid ──────────────────────────────────────────────────────────────────

/// One square per signal: lit when the latest value is nonzero, dark when zero,
/// outlined when no sample exists. Made for bitset messages (e.g. discharge bits).
fn draw_leds(ui: &mut egui::Ui, panel: &PanelConfig, render: &RenderCache, height: f32) {
    let n = panel.signals.len().max(1);

    let (response, painter) = ui.allocate_painter(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let rect = response.rect;

    // Fit an n-cell grid: start from a target cell size, then shrink if the
    // resulting rows overflow the panel height.
    let gap = 4.0;
    let mut cell = 30.0_f32;
    let mut cols = ((rect.width() - gap) / (cell + gap)).floor().max(1.0) as usize;
    let mut rows = n.div_ceil(cols);
    let grid_h = rows as f32 * (cell + gap) - gap;
    if grid_h > rect.height() - 8.0 {
        cell = ((rect.height() - 8.0 + gap) / rows as f32 - gap).max(8.0);
        cols = ((rect.width() - gap) / (cell + gap)).floor().max(1.0) as usize;
        rows = n.div_ceil(cols);
    }

    let grid_w = cols.min(n) as f32 * (cell + gap) - gap;
    let origin = egui::pos2(
        rect.center().x - grid_w / 2.0,
        rect.center().y - (rows as f32 * (cell + gap) - gap) / 2.0,
    );

    for (i, sig_key) in panel.signals.iter().enumerate() {
        let (r, c) = (i / cols, i % cols);
        let min = origin + egui::vec2(c as f32 * (cell + gap), r as f32 * (cell + gap));
        let sq = egui::Rect::from_min_size(min, egui::vec2(cell, cell));
        let rounding = egui::Rounding::same(cell * 0.18);

        match render.latest.get(sig_key) {
            Some(&v) if v != 0.0 => {
                painter.rect_filled(sq, rounding, egui::Color32::from_rgb(90, 200, 90));
            }
            Some(_) => {
                painter.rect_filled(sq, rounding, egui::Color32::from_gray(45));
            }
            None => {
                painter.rect_stroke(sq, rounding, egui::Stroke::new(1.0, egui::Color32::from_gray(70)));
            }
        }

        // Cell index, readable on both lit and dark squares
        if cell >= 14.0 {
            painter.text(
                sq.center(),
                egui::Align2::CENTER_CENTER,
                i.to_string(),
                egui::FontId::proportional((cell * 0.4).clamp(8.0, 14.0)),
                egui::Color32::from_gray(220),
            );
        }
    }
}

// ── Category bars ─────────────────────────────────────────────────────────────

/// One vertical bar per signal at its latest value — outliers pop visually.
/// Hovering a bar shows the signal name. Uses gauge min/max as y-bounds if set.
fn draw_bars(ui: &mut egui::Ui, panel: &PanelConfig, render: &RenderCache, height: f32) {
    let bars: Vec<Bar> = panel.signals.iter().enumerate()
        .map(|(i, sig_key)| {
            let v = render.latest.get(sig_key).copied().unwrap_or(0.0);
            let name = sig_key.rsplit('.').next().unwrap_or(sig_key);
            Bar::new(i as f64, v).width(0.7).name(name)
        })
        .collect();

    let mut plot = Plot::new(&panel.id)
        .height(height)
        .show_x(false);
    if let Some(g) = &panel.gauge {
        plot = plot.include_y(g.min).include_y(g.max);
    }
    plot.show(ui, |plot_ui| {
        plot_ui.bar_chart(BarChart::new(bars));
    });
}

// ── Stat tile ─────────────────────────────────────────────────────────────────

/// Numeric readout plus min/avg/max aggregates over the query window.
fn draw_stat(ui: &mut egui::Ui, panel: &PanelConfig, render: &RenderCache, height: f32) {
    let n = panel.signals.len().max(1);
    let value_size = (height / n as f32 * 0.34).clamp(16.0, 60.0);
    let small_size = (value_size * 0.34).clamp(10.0, 16.0);
    let gap        = value_size * 0.3;

    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), height),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            let content_h = (value_size + small_size * 2.0 + gap) * n as f32;
            ui.add_space(((height - content_h) * 0.5).max(4.0));

            for sig_key in &panel.signals {
                let name = sig_key.rsplit('.').next().unwrap_or(sig_key);
                match render.stats.get(sig_key) {
                    Some(s) => {
                        ui.label(egui::RichText::new(format_number(s.last)).size(value_size).strong());
                        ui.label(egui::RichText::new(format!(
                            "min {}   avg {}   max {}",
                            format_number(s.min), format_number(s.mean), format_number(s.max),
                        )).size(small_size));
                    }
                    None => {
                        ui.label(egui::RichText::new("—").size(value_size).strong());
                        ui.label(egui::RichText::new("no data in window").size(small_size).weak());
                    }
                }
                ui.label(egui::RichText::new(name).size(small_size).weak());
                ui.add_space(gap);
            }
        },
    );
}

// ── Histogram ─────────────────────────────────────────────────────────────────

fn draw_histogram(ui: &mut egui::Ui, panel: &PanelConfig, render: &RenderCache, height: f32) {
    let sig_key = panel.signals.first().map(|s| s.as_str()).unwrap_or("");
    let bars: Vec<Bar> = render.hists.get(sig_key)
        .map(|bins| bins.iter().map(|&(x, count)| Bar::new(x, count).width(0.9)).collect())
        .unwrap_or_default();

    Plot::new(&panel.id)
        .height(height)
        .y_axis_label("count")
        .show(ui, |plot_ui| {
            let label = sig_key.rsplit('.').next().unwrap_or(sig_key);
            plot_ui.bar_chart(BarChart::new(bars).name(label));
        });
}

// ── Scatter ───────────────────────────────────────────────────────────────────

fn draw_scatter(ui: &mut egui::Ui, panel: &PanelConfig, render: &RenderCache, height: f32) {
    let x_key = panel.signals.first().map(|s| s.as_str()).unwrap_or("");
    let y_key = panel.signals.get(1).map(|s| s.as_str()).unwrap_or("");

    let empty: Vec<[f64; 2]> = Vec::new();
    let pairs = render.scatter.get(&panel.id).unwrap_or(&empty);
    let pts: PlotPoints = pairs.iter().copied().collect();

    let x_label = x_key.rsplit('.').next().unwrap_or(x_key);
    let y_label = y_key.rsplit('.').next().unwrap_or(y_key);

    Plot::new(&panel.id)
        .height(height)
        .x_axis_label(x_label)
        .y_axis_label(y_label)
        .show(ui, |plot_ui| {
            plot_ui.points(Points::new(pts).radius(2.5).name("scatter"));
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimate_minmax_preserves_global_extremes_and_order() {
        // A spiky ramp with an isolated peak and trough that stride sampling would miss.
        let mut pts: Vec<[f64; 2]> = (0..1000).map(|i| [i as f64, (i % 7) as f64]).collect();
        pts[500] = [500.0, 999.0]; // sharp peak
        pts[750] = [750.0, -999.0]; // sharp trough

        let out = decimate_minmax(&pts, 100);

        assert!(out.len() <= 102, "should be within target (+2): got {}", out.len());
        assert!(out.len() < pts.len(), "should actually reduce");
        // Peaks survive min/max bucketing.
        assert!(out.iter().any(|p| p[1] == 999.0), "global max must survive");
        assert!(out.iter().any(|p| p[1] == -999.0), "global min must survive");
        // x stays non-decreasing so the line doesn't zig-zag backwards.
        assert!(out.windows(2).all(|w| w[0][0] <= w[1][0]), "x must be non-decreasing");
    }

    #[test]
    fn decimate_minmax_passes_small_input_through() {
        let pts: Vec<[f64; 2]> = (0..10).map(|i| [i as f64, i as f64]).collect();
        assert_eq!(decimate_minmax(&pts, 2000), pts);
    }

    #[test]
    fn stride_decimate_caps_length_and_keeps_first() {
        let pts: Vec<[f64; 2]> = (0..1000).map(|i| [i as f64, -(i as f64)]).collect();
        let out = stride_decimate(pts.clone(), 100);
        assert_eq!(out.len(), 100);
        assert_eq!(out[0], pts[0]); // pairing preserved: index 0 maps to first point
        // Values stay paired (y == -x for every emitted point).
        assert!(out.iter().all(|p| p[1] == -p[0]));
    }

    #[test]
    fn sig_stats_aggregates_window() {
        let pts = [[0.0, 2.0], [1.0, 8.0], [2.0, 5.0]];
        let s = sig_stats(&pts).unwrap();
        assert_eq!(s.last, 5.0);
        assert_eq!(s.min, 2.0);
        assert_eq!(s.max, 8.0);
        assert_eq!(s.mean, 5.0);
        assert!(sig_stats(&[]).is_none());
    }

    #[test]
    fn bin_histogram_counts_by_integer_bucket() {
        let pts = [[0.0, 1.2], [0.0, 1.8], [0.0, 2.4], [0.0, 5.0]];
        let bins = bin_histogram(&pts);
        // 1.2 & 1.8 -> bin 1 (count 2), 2.4 -> bin 2 (count 1), 5.0 -> bin 5 (count 1)
        assert_eq!(bins, vec![(1.0, 2.0), (2.0, 1.0), (5.0, 1.0)]);
    }

    #[test]
    fn parse_color_handles_names_hex_and_unknown() {
        assert_eq!(parse_color("red"), Some(egui::Color32::from_rgb(229, 72, 72)));
        assert_eq!(parse_color("  Blue "), Some(egui::Color32::from_rgb(96, 165, 250)));
        assert_eq!(parse_color("#FF8000"), Some(egui::Color32::from_rgb(255, 128, 0)));
        assert_eq!(
            parse_color("#10203040"),
            Some(egui::Color32::from_rgba_unmultiplied(16, 32, 48, 64))
        );
        assert_eq!(parse_color("chartreuse"), None); // unknown -> auto color
        assert_eq!(parse_color("#12"), None);        // malformed -> auto color
    }

    #[test]
    fn pretty_name_titlecases_snake_case() {
        assert_eq!(pretty_name("drive_state"), "Drive State");
        assert_eq!(pretty_name("bps_fault"), "Bps Fault");
        assert_eq!(pretty_name("soc"), "Soc");
    }
}

use crate::decoder::can_config::EnumLookup;
use egui_plot::{Bar, BarChart, Line, Plot, PlotPoints, Points};
use std::collections::{BTreeMap, HashMap};

use super::config::{ChartType, PanelConfig};

pub type DataCache = HashMap<String, Vec<[f64; 2]>>;  // key: "msg.sig", val: [secs_offset, value]

// Decimation / binning targets. Chosen once at refresh cadence, not per frame.
const LINE_TARGET:    usize = 2000; // ~worst-case horizontal pixels; independent of window
const SCATTER_TARGET: usize = 4000;

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
    latest:  HashMap<String, f64>,             // latest value per signal, for gauges
    labels:  HashMap<String, String>,          // enum label of latest value (status panels)
    stats:   HashMap<String, SigStats>,        // window aggregates (stat tiles)
}

impl RenderCache {
    /// Build all render artifacts for the given panels from the raw cache.
    pub fn build(raw: &DataCache, panels: &[PanelConfig], enum_lookup: &EnumLookup) -> Self {
        let mut rc = RenderCache::default();
        for panel in panels {
            match panel.chart_type {
                ChartType::Line => {
                    for key in &panel.signals {
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
                ChartType::Status => {
                    // Latest value per signal plus its enum label (if the CAN config
                    // defines one), resolved here so the draw path stays lookup-free.
                    for key in &panel.signals {
                        let Some(v) = raw.get(key).and_then(|p| p.last()).map(|p| p[1]) else { continue };
                        rc.latest.insert(key.clone(), v);
                        let label = PanelConfig::signal_parts(key)
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

// ── Public entry point ────────────────────────────────────────────────────────

pub fn draw_panel(ui: &mut egui::Ui, panel: &PanelConfig, render: &RenderCache, height: f32) {
    egui::Frame::dark_canvas(ui.style())
        .show(ui, |ui| {
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
            }
        });
}

// ── Line chart ────────────────────────────────────────────────────────────────

fn draw_line(ui: &mut egui::Ui, panel: &PanelConfig, render: &RenderCache, height: f32) {
    Plot::new(&panel.id)
        .height(height)
        .x_axis_label("s")
        .legend(egui_plot::Legend::default())
        .show(ui, |plot_ui| {
            for sig_key in &panel.signals {
                if let Some(pts) = render.lines.get(sig_key) {
                    // pts is already decimated (<= ~LINE_TARGET), so this clone is cheap.
                    let pp = PlotPoints::new(pts.clone());
                    let label = sig_key.rsplit('.').next().unwrap_or(sig_key);
                    plot_ui.line(Line::new(pp).name(label));
                }
            }
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
        egui::Color32::WHITE,
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

// ── Status indicator ──────────────────────────────────────────────────────────

/// One row per signal: colored state dot + enum label (or value) + signal name.
/// Color heuristic: enum labels containing FAULT/ERR/INVALID are red; without an
/// enum, nonzero is treated as a fault (fault-code convention) and zero as OK.
fn draw_status(ui: &mut egui::Ui, panel: &PanelConfig, render: &RenderCache, height: f32) {
    const RED:   egui::Color32 = egui::Color32::from_rgb(220, 90, 90);
    const GREEN: egui::Color32 = egui::Color32::from_rgb(90, 200, 90);

    let n = panel.signals.len().max(1);
    let row_size   = (height / n as f32 * 0.34).clamp(14.0, 32.0);
    let gap        = row_size * 0.5;

    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), height),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            let content_h = (row_size + gap) * n as f32;
            ui.add_space(((height - content_h) * 0.5).max(4.0));

            for sig_key in &panel.signals {
                let name = sig_key.rsplit('.').next().unwrap_or(sig_key);
                let value = render.latest.get(sig_key).copied();
                let label = render.labels.get(sig_key).cloned()
                    .or_else(|| value.map(format_number))
                    .unwrap_or_else(|| "—".to_string());

                let color = match value {
                    None => egui::Color32::GRAY,
                    Some(v) => {
                        let bad = match render.labels.get(sig_key) {
                            Some(l) => {
                                let u = l.to_uppercase();
                                u.contains("FAULT") || u.contains("ERR") || u.contains("INVALID")
                            }
                            None => v != 0.0,
                        };
                        if bad { RED } else { GREEN }
                    }
                };

                ui.horizontal(|ui| {
                    // Center the row roughly by padding to the panel midpoint.
                    ui.add_space((ui.available_width() * 0.12).max(8.0));
                    ui.label(egui::RichText::new("●").size(row_size).color(color));
                    ui.label(egui::RichText::new(label).size(row_size * 0.85).strong());
                    ui.label(egui::RichText::new(name).size((row_size * 0.5).max(10.0)).weak());
                });
                ui.add_space(gap);
            }
        },
    );
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
}

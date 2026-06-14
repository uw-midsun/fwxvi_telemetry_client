use egui_plot::{Bar, BarChart, Line, Plot, PlotPoints, Points};
use std::collections::HashMap;

use super::config::{ChartType, PanelConfig};

pub type DataCache = HashMap<String, Vec<[f64; 2]>>;  // key: "msg.sig", val: [secs_offset, value]

// ── Public entry point ────────────────────────────────────────────────────────

pub fn draw_panel(ui: &mut egui::Ui, panel: &PanelConfig, data: &DataCache, height: f32) {
    egui::Frame::dark_canvas(ui.style())
        .show(ui, |ui| {
            ui.set_min_height(height);
            match panel.chart_type {
                ChartType::Line      => draw_line(ui, panel, data, height),
                ChartType::Gauge     => draw_gauge(ui, panel, data, height),
                ChartType::Histogram => draw_histogram(ui, panel, data, height),
                ChartType::Scatter   => draw_scatter(ui, panel, data, height),
            }
        });
}

// ── Line chart ────────────────────────────────────────────────────────────────

fn draw_line(ui: &mut egui::Ui, panel: &PanelConfig, data: &DataCache, height: f32) {
    Plot::new(&panel.id)
        .height(height)
        .x_axis_label("s")
        .legend(egui_plot::Legend::default())
        .show(ui, |plot_ui| {
            for sig_key in &panel.signals {
                if let Some(pts) = data.get(sig_key) {
                    let pp = PlotPoints::new(pts.clone());
                    let label = sig_key.rsplit('.').next().unwrap_or(sig_key);
                    plot_ui.line(Line::new(pp).name(label));
                }
            }
        });
}

// ── Gauge ─────────────────────────────────────────────────────────────────────

fn draw_gauge(ui: &mut egui::Ui, panel: &PanelConfig, data: &DataCache, height: f32) {
    let sig_key = panel.signals.first().map(|s| s.as_str()).unwrap_or("");
    let latest  = data.get(sig_key).and_then(|pts| pts.last()).map(|p| p[1]).unwrap_or(0.0);

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

// ── Histogram ─────────────────────────────────────────────────────────────────

fn draw_histogram(ui: &mut egui::Ui, panel: &PanelConfig, data: &DataCache, height: f32) {
    let sig_key = panel.signals.first().map(|s| s.as_str()).unwrap_or("");
    let values: Vec<f64> = data.get(sig_key)
        .map(|pts| pts.iter().map(|p| p[1]).collect())
        .unwrap_or_default();

    let mut bins: std::collections::BTreeMap<i64, usize> = std::collections::BTreeMap::new();
    for &v in &values {
        *bins.entry(v as i64).or_default() += 1;
    }

    let bars: Vec<Bar> = bins.iter()
        .map(|(&x, &count)| Bar::new(x as f64, count as f64).width(0.9))
        .collect();

    Plot::new(&panel.id)
        .height(height)
        .y_axis_label("count")
        .show(ui, |plot_ui| {
            let label = sig_key.rsplit('.').next().unwrap_or(sig_key);
            plot_ui.bar_chart(BarChart::new(bars).name(label));
        });
}

// ── Scatter ───────────────────────────────────────────────────────────────────

fn draw_scatter(ui: &mut egui::Ui, panel: &PanelConfig, data: &DataCache, height: f32) {
    let x_key = panel.signals.first().map(|s| s.as_str()).unwrap_or("");
    let y_key = panel.signals.get(1).map(|s| s.as_str()).unwrap_or("");

    let x_pts = data.get(x_key).cloned().unwrap_or_default();
    let y_pts = data.get(y_key).cloned().unwrap_or_default();
    let n = x_pts.len().min(y_pts.len());

    let pts: PlotPoints = (0..n).map(|i| [x_pts[i][1], y_pts[i][1]]).collect();

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

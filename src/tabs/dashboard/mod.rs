mod charts;
mod config;

pub use config::DashboardSetup;
use config::{PanelConfig, ROW_UNITS};

use crate::decoder::can_config::{EnumLookup, FlagLookup};
use charts::{DataCache, RenderCache};
use rusqlite::Connection;

const REFRESH_MS: u128 = 500;
const ROW_GAP:    f32  = 8.0;

// Accent for the drop-location outline shown while dragging a widget.
const DROP_ACCENT: egui::Color32 = egui::Color32::from_rgb(96, 165, 250);

pub struct DashboardTab {
    setup:            DashboardSetup,
    setup_path:       std::path::PathBuf,
    // Time control UI state
    live:             bool,
    window_secs:      f64,
    manual_start:     String,
    manual_end:       String,
    // Data
    cache:            DataCache,
    render:           RenderCache,   // render-ready artifacts, rebuilt at refresh cadence
    window_start_ts:  f64,   // unix epoch; x-axis values are relative to this
    last_refresh:     std::time::Instant,
}

impl DashboardTab {
    pub fn new() -> Self {
        let setup_path = crate::config::exe_dir().join("default_setup.json");
        let setup      = DashboardSetup::load(&setup_path).unwrap_or_default();

        let live         = setup.live;
        let window_secs  = setup.window_seconds;

        Self {
            setup,
            setup_path,
            live,
            window_secs,
            manual_start:    String::new(),
            manual_end:      String::new(),
            cache:           DataCache::new(),
            render:          RenderCache::default(),
            window_start_ts: 0.0,
            last_refresh:    std::time::Instant::now()
                             - std::time::Duration::from_secs(10),
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        conn: Option<&Connection>,
        enum_lookup: &EnumLookup,
        flag_lookup: &FlagLookup,
    ) {
        self.draw_toolbar(ui);
        ui.separator();

        if let Some(conn) = conn {
            self.maybe_refresh(conn, enum_lookup, flag_lookup);
        }

        self.draw_panels(ui);

        // Drive the next passive repaint at our own data cadence instead of the app-wide
        // 60fps. `last_refresh` is reset whenever maybe_refresh() actually pulls new data,
        // so this wakes us up exactly when the next refresh is due. Only live mode moves on
        // its own; history mode is static and repaints on user input alone.
        if self.live {
            let elapsed   = self.last_refresh.elapsed().as_millis();
            let remaining = REFRESH_MS.saturating_sub(elapsed).max(1);
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(remaining as u64));
        }
    }

    // ── Toolbar ───────────────────────────────────────────────────────────────

    fn draw_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.selectable_label(self.live, "Live").clicked() {
                self.live = true;
            }
            if ui.selectable_label(!self.live, "History").clicked() {
                self.live = false;
            }

            ui.separator();

            if self.live {
                ui.label("Window:");
                ui.add(
                    egui::Slider::new(&mut self.window_secs, 5.0..=300.0)
                        .suffix(" s")
                        .logarithmic(true),
                );
            } else {
                ui.label("From:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.manual_start)
                        .hint_text("2025-01-01T00:00:00+00:00")
                        .desired_width(180.0),
                );
                ui.label("To:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.manual_end)
                        .hint_text("2025-01-01T01:00:00+00:00")
                        .desired_width(180.0),
                );
                if ui.button("Refresh").clicked() {
                    // Force refresh by backdating last_refresh
                    self.last_refresh = std::time::Instant::now()
                        - std::time::Duration::from_secs(10);
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(&self.setup.name);
            });
        });
    }

    // ── Data fetch ────────────────────────────────────────────────────────────

    fn maybe_refresh(&mut self, conn: &Connection, enum_lookup: &EnumLookup, flag_lookup: &FlagLookup) {
        let elapsed    = self.last_refresh.elapsed().as_millis();
        let stale      = self.live && elapsed >= REFRESH_MS;
        let first_load = self.cache.is_empty() && !self.setup.panels.is_empty();

        if !stale && !first_load { return; }

        let Some((since_ms, until_ms)) = self.time_range() else { return; };

        self.last_refresh    = std::time::Instant::now();
        self.window_start_ts = since_ms as f64 / 1000.0;

        // Unique signals across all panels (both `signals` and `fields`)
        let signal_keys: std::collections::HashSet<String> = self.setup.panels.iter()
            .flat_map(|p| p.all_signals().map(str::to_string))
            .collect();

        let mut new_cache = DataCache::new();
        let offset = self.window_start_ts;

        for sig_key in signal_keys {
            if let Some((msg, sig)) = PanelConfig::signal_parts(&sig_key) {
                if let Ok(rows) = crate::db::signal_store::query_signal_history(
                    conn, msg, sig, since_ms, until_ms, 5000,
                ) {
                    // Integer ms → relative seconds: a cheap arithmetic op per point
                    // instead of an RFC3339 parse.
                    let pts: Vec<[f64; 2]> = rows.iter()
                        .map(|(ts_ms, v)| [*ts_ms as f64 / 1000.0 - offset, *v])
                        .collect();
                    new_cache.insert(sig_key, pts);
                }
            }
        }

        // Rebuild render-ready artifacts here (refresh cadence), so the draw path
        // never decimates, rebins, or clones raw vectors.
        self.render = RenderCache::build(&new_cache, &self.setup.panels, enum_lookup, flag_lookup);
        self.cache  = new_cache;
    }

    /// Returns the `(since_ms, until_ms)` window in epoch milliseconds, or `None`
    /// when the manual history fields are empty or not valid RFC3339.
    fn time_range(&self) -> Option<(i64, i64)> {
        if self.live {
            let now_ms   = chrono::Utc::now().timestamp_millis();
            let since_ms = now_ms - (self.window_secs * 1000.0) as i64;
            Some((since_ms, now_ms))
        } else {
            let since_ms = parse_rfc3339_ms(self.manual_start.trim())?;
            let until_ms = parse_rfc3339_ms(self.manual_end.trim())?;
            Some((since_ms, until_ms))
        }
    }

    // ── Panel grid ────────────────────────────────────────────────────────────

    fn draw_panels(&mut self, ui: &mut egui::Ui) {
        if self.setup.panels.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.weak("No panels configured. Panels can be added in a future release.");
            });
            return;
        }

        // Pack panels into rows purely by their declared order: keep adding to the
        // current row until the next panel's width would overflow ROW_UNITS, then
        // wrap. No coordinates — position falls out of the panel ordering.
        let mut rows: Vec<Vec<usize>> = Vec::new();
        let mut used = 0usize;
        for (i, p) in self.setup.panels.iter().enumerate() {
            let span = p.width.span().min(ROW_UNITS);
            if rows.is_empty() || used + span > ROW_UNITS {
                rows.push(Vec::new());
                used = 0;
            }
            rows.last_mut().unwrap().push(i);
            used += span;
        }

        // A completed drag → (dragged panel index, panel index dropped onto).
        let mut reorder: Option<(usize, usize)> = None;
        let render = &self.render;
        let panels = &self.setup.panels;

        egui::ScrollArea::vertical().show(ui, |ui| {
            for indices in &rows {
                let total_w = ui.available_width();
                // Sum of spans actually present in this row, so a partly-filled
                // final row still stretches its panels across the width.
                let row_units: usize = indices.iter()
                    .map(|&i| panels[i].width.span().min(ROW_UNITS))
                    .sum::<usize>()
                    .max(1);

                // Subtle boundary drawn around every widget; brighter accent when a
                // drag is hovering this panel to show where the dragged one will land.
                let border_col = if ui.visuals().dark_mode {
                    egui::Color32::from_gray(70)
                } else {
                    egui::Color32::from_gray(190)
                };
                let rounding = egui::Rounding::same(6.0);

                ui.horizontal_top(|ui| {
                    for &idx in indices {
                        let panel = &panels[idx];
                        let span  = panel.width.span().min(ROW_UNITS);
                        let w     = (total_w * span as f32 / row_units as f32 - ROW_GAP)
                                    .max(80.0);
                        let body  = charts::panel_height(panel);

                        // Each panel is a drag source (grab its header to move it) and
                        // a drop target, so panels can be rearranged relative to each
                        // other. The payload is the panel's index.
                        let dnd_id = egui::Id::new(("dash_panel", idx));
                        let alloc = ui.allocate_ui(egui::vec2(w, body + 30.0), |ui| {
                            egui::Frame::none()
                                .stroke(egui::Stroke::new(1.0, border_col))
                                .rounding(rounding)
                                .inner_margin(egui::Margin::same(6.0))
                                .show(ui, |ui| {
                                    ui.dnd_drag_source(dnd_id, idx, |ui| {
                                        ui.vertical(|ui| {
                                            ui.label(
                                                egui::RichText::new(&panel.title).size(17.0).strong(),
                                            );
                                            charts::draw_panel(ui, panel, render, body);
                                        });
                                    })
                                    .response
                                })
                                .inner
                        });
                        let panel_rect = alloc.response.rect;
                        let resp       = alloc.inner;

                        // Drop-target highlight: while a *different* panel is dragged
                        // over this one, outline it to preview the landing spot.
                        let hovered = resp.dnd_hover_payload::<usize>()
                            .map_or(false, |src| *src != idx);
                        if hovered {
                            ui.painter().rect_stroke(
                                panel_rect,
                                rounding,
                                egui::Stroke::new(2.5, DROP_ACCENT),
                            );
                        }

                        if let Some(src) = resp.dnd_release_payload::<usize>() {
                            reorder = Some((*src, idx));
                        }
                    }
                });

                ui.add_space(ROW_GAP);
            }
        });

        if let Some((from, to)) = reorder {
            if from != to && from < self.setup.panels.len() {
                let panel = self.setup.panels.remove(from);
                // Removing shifts indices after `from` down by one.
                let dest = if from < to { to - 1 } else { to };
                let dest = dest.min(self.setup.panels.len());
                self.setup.panels.insert(dest, panel);
                self.persist();
            }
        }
    }

    /// Save the current setup (e.g. after a drag reorder) back to the file it
    /// was loaded from, so the arrangement survives a restart. Errors are
    /// swallowed — a failed save shouldn't disrupt the live UI.
    fn persist(&self) {
        let _ = self.setup.save(&self.setup_path);
    }
}

// Parse an RFC3339 timestamp (as typed in the History fields) to epoch milliseconds.
fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

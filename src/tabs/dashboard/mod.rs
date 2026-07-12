mod charts;
mod config;

pub use config::DashboardSetup;
use config::PanelConfig;

use crate::decoder::can_config::EnumLookup;
use charts::{DataCache, RenderCache};
use rusqlite::Connection;
use std::collections::BTreeMap;

const GRID_COLS:    usize = 3;
const PANEL_HEIGHT: f32   = 280.0;
const REFRESH_MS:   u128  = 500;

pub struct DashboardTab {
    setup:            DashboardSetup,
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
        let setup = crate::config::exe_dir()
            .join("default_setup.json")
            .pipe_ref(|p| DashboardSetup::load(p).unwrap_or_default());

        let live         = setup.live;
        let window_secs  = setup.window_seconds;

        Self {
            setup,
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

    pub fn show(&mut self, ui: &mut egui::Ui, conn: Option<&Connection>, enum_lookup: &EnumLookup) {
        self.draw_toolbar(ui);
        ui.separator();

        if let Some(conn) = conn {
            self.maybe_refresh(conn, enum_lookup);
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

    fn maybe_refresh(&mut self, conn: &Connection, enum_lookup: &EnumLookup) {
        let elapsed    = self.last_refresh.elapsed().as_millis();
        let stale      = self.live && elapsed >= REFRESH_MS;
        let first_load = self.cache.is_empty() && !self.setup.panels.is_empty();

        if !stale && !first_load { return; }

        let Some((since_ms, until_ms)) = self.time_range() else { return; };

        self.last_refresh    = std::time::Instant::now();
        self.window_start_ts = since_ms as f64 / 1000.0;

        // Unique signals across all panels
        let signal_keys: std::collections::HashSet<String> = self.setup.panels.iter()
            .flat_map(|p| p.signals.iter().cloned())
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
        self.render = RenderCache::build(&new_cache, &self.setup.panels, enum_lookup);
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

    fn draw_panels(&self, ui: &mut egui::Ui) {
        if self.setup.panels.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.weak("No panels configured. Panels can be added in a future release.");
            });
            return;
        }

        // Group panel indices by row
        let mut rows: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (i, p) in self.setup.panels.iter().enumerate() {
            rows.entry(p.grid.row).or_default().push(i);
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            for indices in rows.values() {
                let mut sorted = indices.clone();
                sorted.sort_by_key(|&i| self.setup.panels[i].grid.col);

                let total_w = ui.available_width();

                ui.horizontal(|ui| {
                    for &idx in &sorted {
                        let panel    = &self.setup.panels[idx];
                        let col_span = panel.grid.col_span.clamp(1, GRID_COLS);
                        let w        = (total_w * col_span as f32 / GRID_COLS as f32 - 6.0)
                                       .max(80.0);

                        ui.allocate_ui(egui::vec2(w, PANEL_HEIGHT + 28.0), |ui| {
                            ui.vertical(|ui| {
                                ui.strong(&panel.title);
                                charts::draw_panel(ui, panel, &self.render, PANEL_HEIGHT);
                            });
                        });
                    }
                });

                ui.add_space(6.0);
            }
        });
    }
}

// Parse an RFC3339 timestamp (as typed in the History fields) to epoch milliseconds.
fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

// Simple pipe helper to avoid a temp variable when chaining method calls on a value
trait PipeRef: Sized {
    fn pipe_ref<F, R>(&self, f: F) -> R where F: FnOnce(&Self) -> R { f(self) }
}
impl<T> PipeRef for T {}

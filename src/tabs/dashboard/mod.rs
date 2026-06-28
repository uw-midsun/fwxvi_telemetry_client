mod charts;
mod config;

pub use config::DashboardSetup;
use config::PanelConfig;

use charts::DataCache;
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
            window_start_ts: 0.0,
            last_refresh:    std::time::Instant::now()
                             - std::time::Duration::from_secs(10),
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, conn: Option<&Connection>) {
        self.draw_toolbar(ui);
        ui.separator();

        if let Some(conn) = conn {
            self.maybe_refresh(conn);
        }

        self.draw_panels(ui);
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

    fn maybe_refresh(&mut self, conn: &Connection) {
        let elapsed    = self.last_refresh.elapsed().as_millis();
        let stale      = self.live && elapsed >= REFRESH_MS;
        let first_load = self.cache.is_empty() && !self.setup.panels.is_empty();

        if !stale && !first_load { return; }

        let (since, until) = self.time_range();
        if since.is_empty() || until.is_empty() { return; }

        self.last_refresh    = std::time::Instant::now();
        self.window_start_ts = crate::replay::parse_ts(&since);

        // Unique signals across all panels
        let signal_keys: std::collections::HashSet<String> = self.setup.panels.iter()
            .flat_map(|p| p.signals.iter().cloned())
            .collect();

        let mut new_cache = DataCache::new();
        let offset = self.window_start_ts;

        for sig_key in signal_keys {
            if let Some((msg, sig)) = PanelConfig::signal_parts(&sig_key) {
                if let Ok(rows) = crate::db::signal_store::query_signal_history(
                    conn, msg, sig, &since, &until, 5000,
                ) {
                    let pts: Vec<[f64; 2]> = rows.iter()
                        .map(|(ts, v)| [crate::replay::parse_ts(ts) - offset, *v])
                        .collect();
                    new_cache.insert(sig_key, pts);
                }
            }
        }

        self.cache = new_cache;
    }

    fn time_range(&self) -> (String, String) {
        if self.live {
            let now   = chrono::Utc::now();
            let since = now - chrono::Duration::seconds(self.window_secs as i64);
            (since.to_rfc3339(), now.to_rfc3339())
        } else {
            (self.manual_start.trim().to_string(), self.manual_end.trim().to_string())
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
                                charts::draw_panel(ui, panel, &self.cache, PANEL_HEIGHT);
                            });
                        });
                    }
                });

                ui.add_space(6.0);
            }
        });
    }
}

// Simple pipe helper to avoid a temp variable when chaining method calls on a value
trait PipeRef: Sized {
    fn pipe_ref<F, R>(&self, f: F) -> R where F: FnOnce(&Self) -> R { f(self) }
}
impl<T> PipeRef for T {}

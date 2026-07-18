use crate::db::signal_store::{latest_value_id, max_signal_id};
use egui_extras::{Column, TableBuilder};
use rusqlite::Connection;

// The 36 pack cells are split across two AFEs, each exposing 18 voltages over
// six CAN messages (status_A..F), three voltages per message.
const AFE_LETTERS: [char; 6] = ['A', 'B', 'C', 'D', 'E', 'F'];
const CELLS_PER_AFE: usize   = 18;

// The 18 thermistors arrive demuxed by the decoder onto AFE_temperature as
// temperature_0..17 (see decoder::TEMP_MSG). Thermistors 8 and 17 sit outside the
// pack — they measure external/ambient temperature, so they're flagged and left
// out of the "hottest cell" highlight.
const TEMP_MESSAGE:  &str            = "AFE_temperature";
const THERMISTORS:   usize           = 18;
const EXTERNAL_THERM: [usize; 2]     = [8, 17];

/// One cell's identity: the (message, signal) that carries its voltage. The
/// cell's global index (0..35) is just its position in `CellsTab::cells`.
struct CellRef {
    message: String,
    signal:  String,
}

pub struct CellsTab {
    cells:     Vec<CellRef>,
    values:    Vec<Option<f64>>,
    // Latest temperature for thermistors 0..17, indexed by global thermistor id.
    therms:    Vec<Option<f64>>,
    last_poll: std::time::Instant,
    // Only samples with id > baseline_id are shown, so a fresh connect never
    // displays cell voltages left in the sqlite file by a previous session.
    baseline_id: i64,
}

impl CellsTab {
    pub fn new() -> Self {
        let cells = build_cell_refs();
        let values = vec![None; cells.len()];
        Self {
            cells,
            values,
            therms: vec![None; THERMISTORS],
            // Backdate so the first show() polls immediately.
            last_poll: std::time::Instant::now() - std::time::Duration::from_secs(1),
            baseline_id: 0,
        }
    }

    /// Start a fresh session: blank the grid and ignore every row already in the
    /// DB, so a new (or reconnected) serial capture never shows stale voltages
    /// from a previous session. Mirrors `SignalTableTab::reset_to_latest`.
    pub fn reset_to_latest(&mut self, conn: &Connection) {
        for v in &mut self.values {
            *v = None;
        }
        for t in &mut self.therms {
            *t = None;
        }
        self.baseline_id = max_signal_id(conn).unwrap_or(self.baseline_id);
    }

    fn poll(&mut self, conn: &Connection) {
        if self.last_poll.elapsed().as_millis() < 250 {
            return;
        }
        self.last_poll = std::time::Instant::now();
        for (i, c) in self.cells.iter().enumerate() {
            // Keep the current value unless a newer-than-baseline sample exists;
            // samples from a prior session (id <= baseline) are ignored.
            if let Some((v, _)) = latest_value_id(conn, &c.message, &c.signal)
                .ok()
                .flatten()
                .filter(|(_, id)| *id > self.baseline_id)
            {
                self.values[i] = Some(v);
            }
        }
        for (i, t) in self.therms.iter_mut().enumerate() {
            if let Some((v, _)) = latest_value_id(conn, TEMP_MESSAGE, &format!("temperature_{i}"))
                .ok()
                .flatten()
                .filter(|(_, id)| *id > self.baseline_id)
            {
                *t = Some(v);
            }
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, conn: Option<&Connection>) {
        if let Some(conn) = conn {
            self.poll(conn);
            // Keep the view live even without user input.
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
        self.draw(ui);
    }

    fn draw(&self, ui: &mut egui::Ui) {
        // Locate the global min/max cells to highlight.
        let mut min_i: Option<usize> = None;
        let mut max_i: Option<usize> = None;
        for (i, v) in self.values.iter().enumerate() {
            let Some(v) = v else { continue };
            if min_i.map_or(true, |m| *v < self.values[m].unwrap()) { min_i = Some(i); }
            if max_i.map_or(true, |m| *v > self.values[m].unwrap()) { max_i = Some(i); }
        }

        ui.horizontal(|ui| {
            ui.heading("Cell Voltages");
            ui.weak(format!("({} cells)", self.cells.len()));
            if let (Some(mn), Some(mx)) = (min_i, max_i) {
                ui.separator();
                let spread = self.values[mx].unwrap() - self.values[mn].unwrap();
                ui.label(format!(
                    "min {} (cell {})   max {} (cell {})   Δ {}",
                    fmt_val(self.values[mn]), mn,
                    fmt_val(self.values[mx]), mx,
                    fmt_num(spread),
                ));
            }
        });
        // Hottest in-pack thermistor (externals excluded) for the temp highlight.
        let mut hot_i: Option<usize> = None;
        for (i, v) in self.therms.iter().enumerate() {
            if EXTERNAL_THERM.contains(&i) { continue; }
            let Some(v) = v else { continue };
            if hot_i.map_or(true, |h| *v > self.therms[h].unwrap()) { hot_i = Some(i); }
        }

        ui.separator();

        // Cell voltages on the left, thermistor temperatures on the right — the
        // cell grid is narrow, so the temps fill the otherwise-empty space.
        ui.horizontal_top(|ui| {
            ui.scope(|ui| {
                ui.set_max_width(380.0);
                self.draw_cell_table(ui, min_i, max_i);
            });
            ui.separator();
            self.draw_therm_table(ui, hot_i);
        });
    }

    /// Two AFE groups side by side: 18 rows, columns [Cell, V | Cell, V].
    fn draw_cell_table(&self, ui: &mut egui::Ui, min_i: Option<usize>, max_i: Option<usize>) {
        let available = ui.available_height();
        TableBuilder::new(ui)
            .id_salt("cell_table")
            .striped(true)
            .resizable(true)
            .max_scroll_height(available)
            .column(Column::initial(60.0).at_least(40.0))
            .column(Column::initial(120.0).at_least(60.0))
            .column(Column::initial(60.0).at_least(40.0))
            .column(Column::initial(120.0).at_least(60.0))
            .header(20.0, |mut h| {
                h.col(|ui| { ui.strong("Cell"); });
                h.col(|ui| { ui.strong("AFE1"); });
                h.col(|ui| { ui.strong("Cell"); });
                h.col(|ui| { ui.strong("AFE2"); });
            })
            .body(|body| {
                body.rows(20.0, CELLS_PER_AFE, |mut row| {
                    let r = row.index();
                    let left  = r;                    // AFE1: cells 0..17
                    let right = r + CELLS_PER_AFE;    // AFE2: cells 18..35

                    row.col(|ui| { ui.monospace(left.to_string()); });
                    self.value_cell(&mut row, left, min_i, max_i);
                    row.col(|ui| { ui.monospace(right.to_string()); });
                    self.value_cell(&mut row, right, min_i, max_i);
                });
            });
    }

    /// 18 thermistor readings in a `Thermistor | °C` table. The hottest in-pack
    /// sensor is tinted red; the two external sensors are marked and never win it.
    fn draw_therm_table(&self, ui: &mut egui::Ui, hot_i: Option<usize>) {
        let available = ui.available_height();
        TableBuilder::new(ui)
            .id_salt("therm_table")
            .striped(true)
            .resizable(false)
            .max_scroll_height(available)
            .column(Column::initial(110.0).at_least(80.0))
            .column(Column::initial(90.0).at_least(50.0))
            .header(20.0, |mut h| {
                h.col(|ui| { ui.strong("Thermistor"); });
                h.col(|ui| { ui.strong("Temp (°C)"); });
            })
            .body(|body| {
                body.rows(20.0, THERMISTORS, |mut row| {
                    let i = row.index();
                    let external = EXTERNAL_THERM.contains(&i);
                    row.col(|ui| {
                        if external {
                            ui.monospace(format!("{i}  (ext)"));
                        } else {
                            ui.monospace(i.to_string());
                        }
                    });
                    row.col(|ui| {
                        let text = fmt_val(self.therms[i]);
                        if self.therms[i].is_none() {
                            ui.weak(text);
                        } else if Some(i) == hot_i {
                            ui.colored_label(egui::Color32::from_rgb(220, 90, 90), text);
                        } else {
                            ui.monospace(text);
                        }
                    });
                });
            });
    }

    /// Render one voltage cell, coloring the pack min (blue) and max (red).
    fn value_cell(
        &self,
        row: &mut egui_extras::TableRow,
        idx: usize,
        min_i: Option<usize>,
        max_i: Option<usize>,
    ) {
        row.col(|ui| {
            let text = fmt_val(self.values[idx]);
            if self.values[idx].is_none() {
                ui.weak(text);
            } else if Some(idx) == max_i {
                ui.colored_label(egui::Color32::from_rgb(220, 90, 90), text);
            } else if Some(idx) == min_i {
                ui.colored_label(egui::Color32::from_rgb(90, 150, 230), text);
            } else {
                ui.monospace(text);
            }
        });
    }
}

fn build_cell_refs() -> Vec<CellRef> {
    let mut cells = Vec::with_capacity(2 * CELLS_PER_AFE);
    for afe in 1..=2 {
        for (grp, letter) in AFE_LETTERS.iter().enumerate() {
            let message = format!("AFE{afe}_status_{letter}");
            for k in 0..3 {
                let v = grp * 3 + k; // voltage_N index within this AFE
                cells.push(CellRef {
                    message: message.clone(),
                    signal: format!("voltage_{v}"),
                });
            }
        }
    }
    cells
}

fn fmt_val(v: Option<f64>) -> String {
    match v {
        Some(v) => fmt_num(v),
        None    => "—".to_string(),
    }
}

fn fmt_num(v: f64) -> String {
    let s = format!("{:.3}", v);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

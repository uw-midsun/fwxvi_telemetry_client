use crate::db::signal_store::{get_stats, query_since};
use crate::replay::SignalSnapshot;
use egui_extras::{Column, TableBuilder};
use rusqlite::Connection;
use std::collections::HashMap;

pub struct SignalTableTab {
    last_id:       i64,
    read_counts:   HashMap<String, u32>,
    rows:          Vec<DisplayRow>,
    last_poll:     std::time::Instant,
    bytes_per_sec: f64,
    last_bytes:    i64,
    last_rate_ts:  std::time::Instant,
}

struct DisplayRow {
    key:       String,
    message:   String,
    signal:    String,
    value:     i64,
    reads:     u32,
    timestamp: String,
    can_id:    u32,
}

impl SignalTableTab {
    pub fn new() -> Self {
        Self {
            last_id:       0,
            read_counts:   HashMap::new(),
            rows:          vec![],
            last_poll:     std::time::Instant::now(),
            bytes_per_sec: 0.0,
            last_bytes:    0,
            last_rate_ts:  std::time::Instant::now(),
        }
    }

    /// Reset all rows — called when switching replay position or loading a new file.
    pub fn clear(&mut self) {
        self.rows.clear();
        self.read_counts.clear();
    }

    // ── Live path ─────────────────────────────────────────────────────────────

    fn poll_live(&mut self, conn: &Connection) {
        if self.last_poll.elapsed().as_millis() < 250 {
            return;
        }
        self.last_poll = std::time::Instant::now();

        if let Ok(new_rows) = query_since(conn, self.last_id, 500) {
            for row in new_rows {
                self.last_id = self.last_id.max(row.id);
                self.upsert(row.message_name, row.signal_name, row.value, row.timestamp, row.can_id);
            }
        }

        if let Ok(stats) = get_stats(conn) {
            let now_bytes = *stats.get("parse_byte_calls").unwrap_or(&0);
            let elapsed = self.last_rate_ts.elapsed().as_secs_f64();
            if elapsed > 0.0 {
                const ALPHA: f64 = 0.2;
                let raw = (now_bytes - self.last_bytes) as f64 / elapsed;
                self.bytes_per_sec = ALPHA * raw + (1.0 - ALPHA) * self.bytes_per_sec;
            }
            self.last_bytes = now_bytes;
            self.last_rate_ts = std::time::Instant::now();
        }
    }

    // ── Replay path ───────────────────────────────────────────────────────────

    /// Replace current rows with a replay snapshot.
    /// Reads comes directly from the DB sample count — deterministic at any timestamp.
    pub fn load_snapshot(&mut self, snapshots: &[SignalSnapshot]) {
        self.rows.clear();
        for s in snapshots {
            let key = format!("{}::{}", s.message_name, s.signal_name);
            self.rows.push(DisplayRow {
                key,
                message:   s.message_name.clone(),
                signal:    s.signal_name.clone(),
                value:     s.value,
                reads:     s.reads as u32,
                timestamp: s.timestamp.clone(),
                can_id:    s.can_id,
            });
        }
    }

    // ── Shared upsert ─────────────────────────────────────────────────────────

    fn upsert(&mut self, message: String, signal: String, value: i64, timestamp: String, can_id: u32) {
        let key = format!("{message}::{signal}");
        let count = self.read_counts.entry(key.clone()).or_insert(0);
        *count += 1;
        let reads = *count;

        if let Some(r) = self.rows.iter_mut().find(|r| r.key == key) {
            r.value     = value;
            r.reads     = reads;
            r.timestamp = timestamp;
        } else {
            self.rows.push(DisplayRow { key, message, signal, value, reads, timestamp, can_id });
        }
    }

    // ── Render ────────────────────────────────────────────────────────────────

    /// Live render: polls DB then draws table.
    pub fn show(&mut self, ui: &mut egui::Ui, conn: &Connection) {
        self.poll_live(conn);
        self.draw(ui, false);
    }

    /// Replay render: just draws current rows (caller manages when to call load_snapshot).
    pub fn show_replay(&mut self, ui: &mut egui::Ui) {
        self.draw(ui, true);
    }

    fn draw(&self, ui: &mut egui::Ui, is_replay: bool) {
        ui.horizontal(|ui| {
            if is_replay {
                ui.label(format!("Signals: {}  (replay)", self.rows.len()));
            } else {
                ui.label(format!(
                    "Signals: {}  |  Bytes/s: {:.1}",
                    self.rows.len(),
                    self.bytes_per_sec
                ));
            }
        });
        ui.separator();

        let available = ui.available_height();
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .max_scroll_height(available)
            .column(Column::initial(200.0).at_least(80.0))
            .column(Column::initial(180.0).at_least(80.0))
            .column(Column::initial(90.0).at_least(50.0))
            .column(Column::initial(70.0).at_least(40.0))
            .column(Column::initial(190.0).at_least(100.0))
            .column(Column::initial(70.0).at_least(40.0))
            .header(20.0, |mut h| {
                h.col(|ui| { ui.strong("Message"); });
                h.col(|ui| { ui.strong("Signal"); });
                h.col(|ui| { ui.strong("Value"); });
                h.col(|ui| { ui.strong("Reads"); });
                h.col(|ui| { ui.strong("Timestamp"); });
                h.col(|ui| { ui.strong("CAN ID"); });
            })
            .body(|body| {
                body.rows(18.0, self.rows.len(), |mut row| {
                    let r = &self.rows[row.index()];
                    row.col(|ui| { ui.label(&r.message); });
                    row.col(|ui| { ui.label(&r.signal); });
                    row.col(|ui| { ui.label(r.value.to_string()); });
                    row.col(|ui| { ui.label(r.reads.to_string()); });
                    row.col(|ui| { ui.label(&r.timestamp); });
                    row.col(|ui| { ui.label(format!("0x{:03X}", r.can_id)); });
                });
            });
    }
}

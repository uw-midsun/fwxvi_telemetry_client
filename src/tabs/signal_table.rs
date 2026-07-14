use crate::db::signal_store::{get_stats, max_signal_id, query_since};
use crate::decoder::can_config::EnumLookup;
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
    value:     f64,
    reads:     u32,
    ts_ms:     i64,
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

    /// Start a fresh live session: drop any rows/counts and fast-forward past
    /// every row already in the DB, so a new capture never replays a previous
    /// session's data left over in an old sqlite file.
    pub fn reset_to_latest(&mut self, conn: &Connection) {
        self.clear();
        self.last_id = max_signal_id(conn).unwrap_or(self.last_id);
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
                self.upsert(row.message_name, row.signal_name, row.value, row.ts_ms, row.can_id);
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
                ts_ms:     s.ts_ms,
                can_id:    s.can_id,
            });
        }
    }

    // ── Shared upsert ─────────────────────────────────────────────────────────

    fn upsert(&mut self, message: String, signal: String, value: f64, ts_ms: i64, can_id: u32) {
        let key = format!("{message}::{signal}");
        let count = self.read_counts.entry(key.clone()).or_insert(0);
        *count += 1;
        let reads = *count;

        if let Some(r) = self.rows.iter_mut().find(|r| r.key == key) {
            r.value = value;
            r.reads = reads;
            r.ts_ms = ts_ms;
        } else {
            self.rows.push(DisplayRow { key, message, signal, value, reads, ts_ms, can_id });
        }
    }

    // ── Render ────────────────────────────────────────────────────────────────

    pub fn show(&mut self, ui: &mut egui::Ui, conn: &Connection, enum_lookup: &EnumLookup) {
        self.poll_live(conn);
        self.draw(ui, false, enum_lookup);
    }

    pub fn show_replay(&mut self, ui: &mut egui::Ui, enum_lookup: &EnumLookup) {
        self.draw(ui, true, enum_lookup);
    }

    fn draw(&self, ui: &mut egui::Ui, is_replay: bool, enum_lookup: &EnumLookup) {
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
                    row.col(|ui| { ui.label(format_value(r.value, &r.message, &r.signal, enum_lookup)); });
                    row.col(|ui| { ui.label(r.reads.to_string()); });
                    row.col(|ui| { ui.label(fmt_ts_ms(r.ts_ms)); });
                    row.col(|ui| { ui.label(format!("0x{:03X}", r.can_id)); });
                });
            });
    }
}

fn fmt_ts_ms(ts_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ts_ms)
        .map(|dt: chrono::DateTime<chrono::Utc>| dt.format("%H:%M:%S%.3f").to_string())
        .unwrap_or_default()
}

fn format_value(value: f64, message: &str, signal: &str, enum_lookup: &EnumLookup) -> String {
    if let Some(map) = enum_lookup.get(&(message.to_string(), signal.to_string())) {
        let key = (value as i64).to_string();
        if let Some(label) = map.get(&key) {
            return label.clone();
        }
    }
    let s = format!("{:.6}", value);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

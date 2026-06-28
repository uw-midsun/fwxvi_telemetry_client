use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub struct SignalSnapshot {
    pub can_id:       u32,
    pub message_name: String,
    pub signal_name:  String,
    pub value:        f64,
    pub timestamp:    String,
    pub reads:        u64,   // COUNT of samples for this signal up to current_ts
}

pub struct ReplayController {
    pub conn:       Connection,
    #[allow(dead_code)]
    pub db_path:    PathBuf,
    pub start_ts:   f64,   // unix epoch seconds
    pub end_ts:     f64,
    pub current_ts: f64,
    pub playing:    bool,
    pub speed:      f32,
    last_tick:      Instant,
    last_rendered:  f64,   // current_ts at last snapshot — NaN means force refresh
}

pub const SPEEDS: &[f32] = &[0.25, 0.5, 1.0, 2.0, 5.0, 10.0];

impl ReplayController {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("Cannot open {}", path.display()))?;

        // Verify the table exists before querying it
        let table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='signal_samples'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false);

        if !table_exists {
            anyhow::bail!(
                "No 'signal_samples' table found. This may not be a telemetry database."
            );
        }

        // MIN/MAX return NULL on an empty table, so use Option<String>
        let (start_opt, end_opt): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT MIN(timestamp), MAX(timestamp) FROM signal_samples",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .context("Failed to read signal_samples")?;

        let start_str = start_opt
            .ok_or_else(|| anyhow::anyhow!("signal_samples table is empty — no data recorded yet"))?;
        let end_str = end_opt.unwrap_or_else(|| start_str.clone());

        let start_ts = parse_ts(&start_str);
        let end_ts   = parse_ts(&end_str);

        Ok(Self {
            conn,
            db_path: path.to_path_buf(),
            start_ts,
            end_ts,
            current_ts: start_ts,
            playing: false,
            speed: 1.0,
            last_tick: Instant::now(),
            last_rendered: f64::NAN,
        })
    }

    /// Advance playback position. Returns true if position changed.
    pub fn tick(&mut self) -> bool {
        let elapsed = self.last_tick.elapsed().as_secs_f64();
        self.last_tick = Instant::now();

        if !self.playing {
            return false;
        }

        let prev = self.current_ts;
        self.current_ts = (self.current_ts + elapsed * self.speed as f64).min(self.end_ts);
        if self.current_ts >= self.end_ts {
            self.playing = false;
        }
        self.current_ts != prev
    }

    pub fn seek(&mut self, ts: f64) {
        self.current_ts = ts.clamp(self.start_ts, self.end_ts);
        self.last_rendered = f64::NAN; // force snapshot refresh
    }

    pub fn seek_relative(&mut self, delta_secs: f64) {
        self.seek(self.current_ts + delta_secs);
    }

    pub fn reset(&mut self) {
        self.seek(self.start_ts);
        self.playing = false;
    }

    /// Returns true when the current position differs from the last rendered position.
    pub fn needs_refresh(&self) -> bool {
        (self.current_ts - self.last_rendered).abs() > 0.001
    }

    /// Query all signals at the current playback position (latest value per signal up to current_ts).
    pub fn snapshot(&mut self) -> Vec<SignalSnapshot> {
        self.last_rendered = self.current_ts;
        let ts_str = format_ts_iso(self.current_ts);

        let mut stmt = match self.conn.prepare_cached(
            "SELECT can_id, message_name, signal_name, value, timestamp, sample_count
             FROM (
                 SELECT can_id, message_name, signal_name, value, timestamp,
                        ROW_NUMBER() OVER (
                            PARTITION BY message_name, signal_name
                            ORDER BY timestamp DESC
                        ) AS rn,
                        COUNT(*) OVER (
                            PARTITION BY message_name, signal_name
                        ) AS sample_count
                 FROM signal_samples
                 WHERE timestamp <= ?1
             )
             WHERE rn = 1",
        ) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Replay snapshot query failed: {e}");
                return vec![];
            }
        };

        stmt.query_map(rusqlite::params![ts_str], |r| {
            Ok(SignalSnapshot {
                can_id:       r.get::<_, i64>(0)? as u32,
                message_name: r.get(1)?,
                signal_name:  r.get(2)?,
                value:        r.get::<_, f64>(3)?,
                timestamp:    r.get(4)?,
                reads:        r.get::<_, i64>(5)? as u64,
            })
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    // ── Helpers for the UI ────────────────────────────────────────────────────

    #[allow(dead_code)]
    pub fn progress(&self) -> f64 {
        if (self.end_ts - self.start_ts).abs() < 0.001 {
            return 0.0;
        }
        (self.current_ts - self.start_ts) / (self.end_ts - self.start_ts)
    }

    pub fn duration_secs(&self) -> f64 {
        self.end_ts - self.start_ts
    }

    pub fn format_current(&self) -> String {
        format_ts_display(self.current_ts)
    }

    pub fn format_end(&self) -> String {
        format_ts_display(self.end_ts)
    }

    pub fn format_start(&self) -> String {
        format_ts_display(self.start_ts)
    }
}

// ── Timestamp helpers ─────────────────────────────────────────────────────────

pub fn parse_ts(s: &str) -> f64 {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.timestamp_millis() as f64 / 1000.0)
        .unwrap_or(0.0)
}

fn format_ts_iso(unix: f64) -> String {
    let secs  = unix as i64;
    let nanos = ((unix - secs as f64) * 1e9) as u32;
    chrono::DateTime::from_timestamp(secs, nanos)
        .map(|dt: chrono::DateTime<chrono::Utc>| dt.to_rfc3339())
        .unwrap_or_default()
}

fn format_ts_display(unix: f64) -> String {
    chrono::DateTime::from_timestamp(unix as i64, 0)
        .map(|dt: chrono::DateTime<chrono::Utc>| dt.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "--:--:--".to_string())
}

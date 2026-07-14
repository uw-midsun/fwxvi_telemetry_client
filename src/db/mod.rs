pub mod signal_store;

use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
    init_schema(&conn)?;
    Ok(conn)
}

fn init_schema(conn: &Connection) -> Result<()> {
    // Fresh DBs get the ts_ms schema directly. Existing DBs are handled by
    // migrate_timestamp_to_ts_ms below (CREATE IF NOT EXISTS is a no-op there).
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS signal_samples (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            ts_ms        INTEGER NOT NULL,          -- epoch milliseconds, UTC
            can_id       INTEGER NOT NULL,
            parent_name  TEXT    NOT NULL,
            message_name TEXT    NOT NULL,
            signal_name  TEXT    NOT NULL,
            value        REAL    NOT NULL
        );

        CREATE TABLE IF NOT EXISTS decoder_stats (
            stat_key   TEXT PRIMARY KEY,
            value      INTEGER NOT NULL DEFAULT 0,
            updated_at TEXT    NOT NULL
        );",
    )?;

    migrate_timestamp_to_ts_ms(conn)?;

    conn.execute_batch(
        // Drop the redundant index on the rowid PK (cleanup for existing DBs).
        "DROP INDEX IF EXISTS idx_signal_samples_id;

        -- Serves the dashboard query: equality on (message_name, signal_name)
        -- with a range/ORDER BY on the trailing ts_ms column.
        CREATE INDEX IF NOT EXISTS idx_samples_msg_sig_ts
            ON signal_samples (message_name, signal_name, ts_ms);

        -- Serves the replay snapshot's \"latest as of ts\" pattern (perf-05).
        CREATE INDEX IF NOT EXISTS idx_samples_ts
            ON signal_samples (ts_ms);",
    )?;
    Ok(())
}

/// One-time migration from the old RFC3339 TEXT `timestamp` column to integer
/// epoch-millisecond `ts_ms`. No-op on fresh DBs and already-migrated DBs.
fn migrate_timestamp_to_ts_ms(conn: &Connection) -> Result<()> {
    let cols: Vec<String> = {
        let mut stmt = conn.prepare("PRAGMA table_info(signal_samples)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        rows.filter_map(|r| r.ok()).collect()
    };
    let has_ts_ms   = cols.iter().any(|c| c == "ts_ms");
    let has_old_col = cols.iter().any(|c| c == "timestamp");
    if has_ts_ms || !has_old_col {
        return Ok(()); // fresh table (created above) or already migrated
    }

    // Backfill ts_ms from the RFC3339 text, then drop the old column. Indexes that
    // reference `timestamp` must be dropped before the column can be. Rows whose
    // timestamp fails to parse fall back to 0 (COALESCE) so ts_ms is never NULL.
    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_samples_msg_sig_ts;
         DROP INDEX IF EXISTS idx_samples_ts;
         DROP INDEX IF EXISTS idx_signal_samples_id;

         ALTER TABLE signal_samples ADD COLUMN ts_ms INTEGER;
         UPDATE signal_samples
            SET ts_ms = COALESCE(
                CAST((julianday(timestamp) - 2440587.5) * 86400000.0 AS INTEGER), 0);
         ALTER TABLE signal_samples DROP COLUMN timestamp;",
    )?;
    Ok(())
}

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
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS signal_samples (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp    TEXT    NOT NULL,
            can_id       INTEGER NOT NULL,
            parent_name  TEXT    NOT NULL,
            message_name TEXT    NOT NULL,
            signal_name  TEXT    NOT NULL,
            value        INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_signal_samples_id ON signal_samples(id);

        CREATE TABLE IF NOT EXISTS decoder_stats (
            stat_key   TEXT PRIMARY KEY,
            value      INTEGER NOT NULL DEFAULT 0,
            updated_at TEXT    NOT NULL
        );",
    )?;
    Ok(())
}

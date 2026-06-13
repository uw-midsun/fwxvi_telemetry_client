use anyhow::Result;
use rusqlite::Connection;

pub struct SignalSample {
    pub timestamp:    String,
    pub can_id:       u32,
    pub parent_name:  String,
    pub message_name: String,
    pub signal_name:  String,
    pub value:        i64,
}

pub struct SignalRow {
    pub id:           i64,
    pub timestamp:    String,
    pub can_id:       u32,
    pub message_name: String,
    pub signal_name:  String,
    pub value:        i64,
}

pub fn insert_signal(conn: &Connection, s: &SignalSample) -> Result<()> {
    conn.execute(
        "INSERT INTO signal_samples
         (timestamp, can_id, parent_name, message_name, signal_name, value)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            s.timestamp,
            s.can_id,
            s.parent_name,
            s.message_name,
            s.signal_name,
            s.value
        ],
    )?;
    Ok(())
}

/// Fetch rows newer than `after_id`, up to `limit`.
pub fn query_since(conn: &Connection, after_id: i64, limit: usize) -> Result<Vec<SignalRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, timestamp, can_id, message_name, signal_name, value
         FROM signal_samples
         WHERE id > ?1
         ORDER BY id ASC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![after_id, limit as i64], |r| {
        Ok(SignalRow {
            id:           r.get(0)?,
            timestamp:    r.get(1)?,
            can_id:       r.get::<_, i64>(2)? as u32,
            message_name: r.get(3)?,
            signal_name:  r.get(4)?,
            value:        r.get(5)?,
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Fetch (timestamp, value) pairs for one signal within a time window (used by dashboard in Phase 2).
#[allow(dead_code)]
pub fn query_signal_history(
    conn: &Connection,
    message_name: &str,
    signal_name: &str,
    since_ts: &str,
    until_ts: &str,
    limit: usize,
) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT timestamp, value FROM signal_samples
         WHERE message_name = ?1
           AND signal_name   = ?2
           AND timestamp    >= ?3
           AND timestamp    <= ?4
         ORDER BY id ASC
         LIMIT ?5",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![message_name, signal_name, since_ts, until_ts, limit as i64],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
    )?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

pub fn increment_stat(conn: &Connection, key: &str, delta: i64) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO decoder_stats (stat_key, value, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(stat_key) DO UPDATE SET
             value      = value + excluded.value,
             updated_at = excluded.updated_at",
        rusqlite::params![key, delta, now],
    )?;
    Ok(())
}

pub fn get_stats(conn: &Connection) -> Result<std::collections::HashMap<String, i64>> {
    let mut stmt = conn.prepare_cached("SELECT stat_key, value FROM decoder_stats")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

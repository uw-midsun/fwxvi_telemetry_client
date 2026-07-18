use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

pub struct SignalSample {
    pub ts_ms:        i64,   // epoch milliseconds, UTC
    pub can_id:       u32,
    pub parent_name:  String,
    pub message_name: String,
    pub signal_name:  String,
    pub value:        f64,
}

pub struct SignalRow {
    pub id:           i64,
    pub ts_ms:        i64,
    pub can_id:       u32,
    pub message_name: String,
    pub signal_name:  String,
    pub value:        f64,
}

pub fn insert_signal(conn: &Connection, s: &SignalSample) -> Result<()> {
    conn.execute(
        "INSERT INTO signal_samples
         (ts_ms, can_id, parent_name, message_name, signal_name, value)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            s.ts_ms,
            s.can_id,
            s.parent_name,
            s.message_name,
            s.signal_name,
            s.value
        ],
    )?;
    Ok(())
}

/// Highest row id currently in the table, or 0 when empty. Used to skip past a
/// previous session's rows when a new live capture starts, so the signal table
/// doesn't "replay" stale data it inherited from an old sqlite file.
pub fn max_signal_id(conn: &Connection) -> Result<i64> {
    let mut stmt = conn.prepare_cached("SELECT COALESCE(MAX(id), 0) FROM signal_samples")?;
    let id = stmt.query_row([], |r| r.get::<_, i64>(0))?;
    Ok(id)
}

/// Fetch rows newer than `after_id`, up to `limit`.
pub fn query_since(conn: &Connection, after_id: i64, limit: usize) -> Result<Vec<SignalRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, ts_ms, can_id, message_name, signal_name, value
         FROM signal_samples
         WHERE id > ?1
         ORDER BY id ASC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![after_id, limit as i64], |r| {
        Ok(SignalRow {
            id:           r.get(0)?,
            ts_ms:        r.get(1)?,
            can_id:       r.get::<_, i64>(2)? as u32,
            message_name: r.get(3)?,
            signal_name:  r.get(4)?,
            value:        r.get(5)?,
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

pub fn query_signal_history(
    conn: &Connection,
    message_name: &str,
    signal_name: &str,
    since_ms: i64,
    until_ms: i64,
    limit: usize,
) -> Result<Vec<(i64, f64)>> {
    // ORDER BY ts_ms matches the trailing column of idx_samples_msg_sig_ts, so the
    // composite index serves both the filter and the ordering without a temp sort.
    let mut stmt = conn.prepare_cached(
        "SELECT ts_ms, value FROM signal_samples
         WHERE message_name = ?1
           AND signal_name   = ?2
           AND ts_ms        >= ?3
           AND ts_ms        <= ?4
         ORDER BY ts_ms ASC
         LIMIT ?5",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![message_name, signal_name, since_ms, until_ms, limit as i64],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)),
    )?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Latest recorded value for one signal, or `None` if it has no samples. Uses
/// idx_samples_msg_sig_ts: equality on (message, signal) + ORDER BY ts_ms DESC.
/// Latest value together with its sample row id, so callers can ignore samples
/// left over from a previous session (id <= a baseline taken at connect time).
/// `None` when the signal has no samples.
pub fn latest_value_id(
    conn: &Connection,
    message_name: &str,
    signal_name: &str,
) -> Result<Option<(f64, i64)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT value, id FROM signal_samples
         WHERE message_name = ?1 AND signal_name = ?2
         ORDER BY id DESC
         LIMIT 1",
    )?;
    let v = stmt
        .query_row(rusqlite::params![message_name, signal_name], |r| {
            Ok((r.get::<_, f64>(0)?, r.get::<_, i64>(1)?))
        })
        .optional()?;
    Ok(v)
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

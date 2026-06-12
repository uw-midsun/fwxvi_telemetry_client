import sqlite3
from datetime import datetime, timezone
from pathlib import Path


class SignalSampleStore:
    def __init__(self, db_path: Path | None = None):
        if db_path is None:
            db_path = Path(__file__).resolve().parents[1] / "data/decoded_data.sqlite"

        self.conn = sqlite3.connect(db_path)
        self.conn.execute(
            """
            CREATE TABLE IF NOT EXISTS signal_samples (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                can_id INTEGER NOT NULL,
                parent_name TEXT NOT NULL,
                message_name TEXT NOT NULL,
                signal_name TEXT NOT NULL,
                value INTEGER NOT NULL
            )
            """
        )
        self.conn.execute(
            """
            CREATE TABLE IF NOT EXISTS decoder_stats (
                stat_key TEXT PRIMARY KEY,
                value INTEGER NOT NULL,
                updated_at TEXT NOT NULL
            )
            """
        )
        now = datetime.now(timezone.utc).isoformat()
        for stat_key in (
            "parse_byte_calls",
            "parsed_messages",
            "matched_messages",
            "unmatched_messages",
            "malformed message",
        ):
            self.conn.execute(
                """
                INSERT OR IGNORE INTO decoder_stats (stat_key, value, updated_at)
                VALUES (?, 0, ?)
                """,
                (stat_key, now),
            )
        self.conn.commit()

    def insert_signal(
        self, timestamp, can_id, parent_name, message_name, signal_name, value
    ):
        self.conn.execute(
            """
            INSERT INTO signal_samples (
                timestamp,
                can_id,
                parent_name,
                message_name,
                signal_name,
                value
            ) VALUES (?, ?, ?, ?, ?, ?)
            """,
            (timestamp, can_id, parent_name, message_name, signal_name, value),
        )
        self.conn.commit()

    def increment_stat(self, stat_key: str, amount: int = 1):
        now = datetime.now(timezone.utc).isoformat()
        cursor = self.conn.execute(
            """
            UPDATE decoder_stats
            SET value = value + ?, updated_at = ?
            WHERE stat_key = ?
            """,
            (amount, now, stat_key),
        )

        if cursor.rowcount == 0:
            self.conn.execute(
                """
                INSERT INTO decoder_stats (stat_key, value, updated_at)
                VALUES (?, ?, ?)
                """,
                (stat_key, amount, now),
            )

        self.conn.commit()

    def close(self):
        if self.conn is not None:
            self.conn.close()
            self.conn = None

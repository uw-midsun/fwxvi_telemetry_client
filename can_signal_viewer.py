import sqlite3
import tkinter as tk
from tkinter import ttk
from pathlib import Path
from typing import Dict, Tuple

ROOT_DB = Path(__file__).resolve().parent / "data/decoded_data.sqlite"
REFRESH_MS = 250


class CanSignalViewer:
    def __init__(self, root: tk.Tk, db_path: Path):
        self.root = root
        self.db_path = db_path
        self.root.title("CAN Signal Viewer")
        self.root.geometry("900x550")

        self.status_var = tk.StringVar()
        self.status_var.set(f"Watching DB: {self.db_path}")
        self.stats_var = tk.StringVar()
        self.stats_var.set("Parsed: 0 | Matched: 0 | Unmatched: 0 | Malformed: 0")

        top = ttk.Frame(root, padding=10)
        top.pack(fill="both", expand=True)

        header = ttk.Frame(top)
        header.pack(fill="x", pady=(0, 8))

        ttk.Label(header, text="CAN Signal Viewer", font=("Segoe UI", 16, "bold")).pack(
            side="left"
        )
        ttk.Label(header, textvariable=self.status_var).pack(side="right")

        stats_row = ttk.Frame(top)
        stats_row.pack(fill="x", pady=(0, 8))
        ttk.Label(stats_row, textvariable=self.stats_var).pack(side="left")

        columns = ("message", "signal", "value", "timestamp", "id")
        self.tree = ttk.Treeview(top, columns=columns, show="headings")

        self.tree.heading("message", text="Message")
        self.tree.heading("signal", text="Signal")
        self.tree.heading("value", text="Value")
        self.tree.heading("timestamp", text="Timestamp")
        self.tree.heading("id", text="CAN ID")

        self.tree.column("message", width=240)
        self.tree.column("signal", width=220)
        self.tree.column("value", width=100, anchor="center")
        self.tree.column("timestamp", width=240)
        self.tree.column("id", width=80, anchor="center")

        scrollbar_y = ttk.Scrollbar(top, orient="vertical", command=self.tree.yview)
        self.tree.configure(yscrollcommand=scrollbar_y.set)

        self.tree.pack(side="left", fill="both", expand=True)
        scrollbar_y.pack(side="right", fill="y")

        self.row_ids: Dict[str, str] = {}
        self.last_seen_sample_id = 0
        self.conn: sqlite3.Connection | None = None

        self.root.protocol("WM_DELETE_WINDOW", self.on_close)

        self.poll_db()

    def connect(self) -> bool:
        if self.conn is not None:
            return True

        if not self.db_path.exists():
            return False

        try:
            self.conn = sqlite3.connect(self.db_path, timeout=1.0)
            return True
        except sqlite3.Error:
            self.conn = None
            return False

    def upsert_row(
        self, message: str, signal: str, value: int, timestamp: str, can_id: int
    ) -> None:
        key = f"{message}::{signal}"

        values = (
            message,
            signal,
            value,
            timestamp,
            can_id,
        )

        if key in self.row_ids:
            self.tree.item(self.row_ids[key], values=values)
        else:
            row_id = self.tree.insert("", "end", values=values)
            self.row_ids[key] = row_id

    def fetch_new_samples(self) -> list[Tuple[int, str, int, str, str, int]]:
        if self.conn is None:
            return []

        cursor = self.conn.execute(
            """
            SELECT id, timestamp, can_id, message_name, signal_name, value
            FROM signal_samples
            WHERE id > ?
            ORDER BY id ASC
            """,
            (self.last_seen_sample_id,),
        )
        return list(cursor.fetchall())

    def fetch_stats(self) -> Dict[str, int]:
        if self.conn is None:
            return {}

        cursor = self.conn.execute(
            """
            SELECT stat_key, value
            FROM decoder_stats
            """
        )
        return {str(k): int(v) for k, v in cursor.fetchall()}

    def poll_db(self) -> None:
        if not self.connect():
            self.status_var.set(f"Waiting for DB: {self.db_path}")
            self.root.after(REFRESH_MS, self.poll_db)
            return

        try:
            samples = self.fetch_new_samples()
        except sqlite3.OperationalError:
            self.status_var.set("Waiting for table: signal_samples")
            self.root.after(REFRESH_MS, self.poll_db)
            return
        except sqlite3.Error:
            self.status_var.set(f"DB read error: {self.db_path}")
            self.root.after(REFRESH_MS, self.poll_db)
            return

        for sample_id, timestamp, can_id, message, signal, value in samples:
            self.upsert_row(message, signal, value, timestamp, can_id)
            self.last_seen_sample_id = max(self.last_seen_sample_id, sample_id)

        try:
            stats = self.fetch_stats()
        except sqlite3.OperationalError:
            stats = {}
        parsed = stats.get("parsed_messages", 0)
        matched = stats.get("matched_messages", 0)
        unmatched = stats.get("unmatched_messages", 0)
        malformed = stats.get("malformed message", 0)
        self.stats_var.set(
            f"Parsed: {parsed} | Matched: {matched} | Unmatched: {unmatched} | Malformed: {malformed}"
        )

        self.status_var.set(
            f"Watching DB: {self.db_path} | {len(self.row_ids)} signals | last sample id {self.last_seen_sample_id}"
        )
        self.root.after(REFRESH_MS, self.poll_db)

    def on_close(self) -> None:
        if self.conn is not None:
            self.conn.close()
            self.conn = None
        self.root.destroy()


if __name__ == "__main__":
    root = tk.Tk()
    app = CanSignalViewer(root, ROOT_DB)
    root.mainloop()

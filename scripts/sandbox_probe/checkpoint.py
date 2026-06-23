"""Per-tool status store for the sandbox install-probe pipeline (resumable)."""

import sqlite3
from pathlib import Path

TERMINAL = {"merged", "perma-failed", "no-subcommands"}


class Checkpoint:
    def __init__(self, path: Path):
        self.conn = sqlite3.connect(str(path))
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS state ("
            "  tool TEXT PRIMARY KEY, status TEXT, reason TEXT,"
            "  attempts INTEGER DEFAULT 0, updated_at TEXT DEFAULT (datetime('now')))"
        )
        self.conn.commit()

    def set(self, tool: str, status: str, reason: str) -> None:
        self.conn.execute(
            "INSERT INTO state (tool, status, reason, attempts) VALUES (?,?,?,1) "
            "ON CONFLICT(tool) DO UPDATE SET status=excluded.status, reason=excluded.reason, "
            "  attempts=state.attempts+1, updated_at=datetime('now')",
            (tool, status, reason),
        )
        self.conn.commit()

    def get(self, tool: str):
        r = self.conn.execute(
            "SELECT status, reason, attempts FROM state WHERE tool=?", (tool,)
        ).fetchone()
        return r if r else None

    def is_done(self, tool: str) -> bool:
        r = self.get(tool)
        return bool(r and r[0] in TERMINAL)

    def counts(self) -> dict:
        return dict(
            self.conn.execute("SELECT status, count(*) FROM state GROUP BY status")
        )

#!/usr/bin/env python3
"""Apply LLM enrichment (enrich.jsonl) to cmdhub.db.

Adds a `topics` column to arguments if missing, then updates each command's
description / risk_level / example_template / topics from the JSONL produced by
llm_enrich.py. Descriptions and risk are overwritten (the LLM pass is authoritative,
English); example_template/topics are filled.

    python3 apply_enrich.py --enrich /tmp/enrich.jsonl --db ~/.local/share/cmdhub/cmdhub.db
"""
from __future__ import annotations

import argparse
import json
import sqlite3
import sys
from pathlib import Path


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--enrich", required=True)
    ap.add_argument("--db", default=str(Path.home() / ".local/share/cmdhub/cmdhub.db"))
    a = ap.parse_args()

    conn = sqlite3.connect(a.db)
    if not any(c[1] == "topics" for c in conn.execute("PRAGMA table_info(arguments)")):
        conn.execute("ALTER TABLE arguments ADD COLUMN topics TEXT")
        conn.commit()
        print("[apply] added arguments.topics column")

    n = bad = 0
    batch = []
    for line in open(a.enrich):
        line = line.strip()
        if not line:
            continue
        try:
            d = json.loads(line)
        except Exception:
            bad += 1
            continue
        desc = (d.get("description") or "").strip()
        if not desc:
            bad += 1
            continue
        batch.append((
            desc,
            d.get("risk_level") or "safe",
            (d.get("example_template") or None),
            (d.get("topics") or None),
            d["cmd_path"],
        ))
        if len(batch) >= 1000:
            conn.executemany(
                "UPDATE arguments SET description=?, risk_level=?, "
                "example_template=COALESCE(NULLIF(?,''), example_template), topics=? WHERE cmd_path=?",
                batch)
            conn.commit(); n += len(batch); batch = []
    if batch:
        conn.executemany(
            "UPDATE arguments SET description=?, risk_level=?, "
            "example_template=COALESCE(NULLIF(?,''), example_template), topics=? WHERE cmd_path=?",
            batch)
        n += len(batch)
    conn.commit()
    conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    conn.close()
    print(f"[apply] updated {n} commands ({bad} skipped)")


if __name__ == "__main__":
    main()

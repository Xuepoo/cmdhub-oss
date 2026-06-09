#!/usr/bin/env python3
"""Import normalized registry-adapter records (crates/aur/pypi/npm) into cmdhub.db.

Breadth layer: each record becomes one root command (name + description + install).
NEVER clobbers existing data — a record is skipped if its cmd_path already exists, or
if a same-named app already carries install_instructions (i.e. already well covered).

Usage:
    python3 import_registry.py --db ... --records /tmp/crates.json /tmp/aur.json
"""
from __future__ import annotations

import argparse
import json
import sqlite3
import sys
from pathlib import Path


def import_records(db_path: Path, record_files: list[Path]) -> None:
    conn = sqlite3.connect(str(db_path))
    conn.execute("PRAGMA journal_mode=WAL")

    existing_cmd = {r[0] for r in conn.execute("SELECT cmd_path FROM arguments")}
    covered_names = {
        r[0] for r in conn.execute(
            "SELECT name FROM apps WHERE install_instructions IS NOT NULL "
            "AND length(install_instructions) > 2"
        )
    }
    print(f"[import] {len(existing_cmd)} existing cmd_paths, {len(covered_names)} covered names", flush=True)

    added = skipped = 0
    seen_new: set[str] = set()
    for f in record_files:
        if not f.exists():
            print(f"[skip] missing {f}")
            continue
        recs = json.loads(f.read_text())
        f_added = 0
        for r in recs:
            cmd = r["cmd_path"]
            name = r["name"]
            if cmd in existing_cmd or cmd in seen_new or name in covered_names:
                skipped += 1
                continue
            app_id = r["app_id"]
            install = json.dumps(r["install_instructions"], ensure_ascii=False)
            desc = (r.get("description") or "").strip()
            if len(desc) < 5:  # some registry entries have terse/empty descriptions
                desc = f"{name} — command-line tool"
            r["description"] = desc
            conn.execute(
                "INSERT OR IGNORE INTO apps (app_id, name, install_instructions) VALUES (?,?,?)",
                (app_id, name, install),
            )
            conn.execute(
                "INSERT OR IGNORE INTO arguments "
                "(app_id, cmd_path, node_name, node_type, description, risk_level) "
                "VALUES (?,?,?,?,?, 'safe')",
                (app_id, cmd, name, "root", r["description"]),
            )
            seen_new.add(cmd)
            added += 1
            f_added += 1
        print(f"[import] {f.name}: +{f_added} (of {len(recs)})", flush=True)

    conn.commit()
    conn.close()
    print(f"\n[import] added {added} new tools, skipped {skipped} (existing/covered)")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", default=str(Path.home() / ".local/share/cmdhub/cmdhub.db"), type=Path)
    ap.add_argument("--records", nargs="+", required=True, type=Path)
    a = ap.parse_args()
    import_records(a.db, a.records)


if __name__ == "__main__":
    main()

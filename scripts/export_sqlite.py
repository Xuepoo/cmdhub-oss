#!/usr/bin/env python3
"""Export an existing cmdhub.db (apps + arguments) to the JSON format build_db.py consumes.

This lets us treat the local SQLite as the master dataset: mutate it (deep CLI
imports, install fixes, description cleanup), export, then rebuild a fresh DB with
real BGE-micro-v2 embeddings + FTS via build_db.py.

Usage:
    python3 export_sqlite.py --db ~/.local/share/cmdhub/cmdhub.db --out /tmp/cmdhub_export.json
"""
from __future__ import annotations

import argparse
import json
import sqlite3
from pathlib import Path


def export(db_path: Path, out_path: Path) -> None:
    conn = sqlite3.connect(str(db_path))
    conn.row_factory = sqlite3.Row

    apps = [
        {
            "app_id": r["app_id"],
            "name": r["name"],
            "os_aliases": r["os_aliases"],
            "install_instructions": r["install_instructions"],
        }
        for r in conn.execute("SELECT app_id, name, os_aliases, install_instructions FROM apps")
    ]

    arguments = [
        {
            "cmd_path": r["cmd_path"],
            "app_id": r["app_id"],
            "node_name": r["node_name"],
            "node_type": r["node_type"],
            "description": r["description"],
            "risk_level": r["risk_level"],
            "example_template": r["example_template"],
            "docker_image": r["docker_image"],
            "script_url": r["script_url"],
            "source_url": r["source_url"],
        }
        for r in conn.execute(
            "SELECT cmd_path, app_id, node_name, node_type, description, risk_level, "
            "example_template, docker_image, script_url, source_url FROM arguments"
        )
    ]
    conn.close()

    out_path.write_text(json.dumps({"apps": apps, "arguments": arguments}, ensure_ascii=False))
    print(f"Exported {len(apps)} apps, {len(arguments)} arguments → {out_path}")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", default=str(Path.home() / ".local/share/cmdhub/cmdhub.db"), type=Path)
    ap.add_argument("--out", default="/tmp/cmdhub_export.json", type=Path)
    args = ap.parse_args()
    export(args.db, args.out)


if __name__ == "__main__":
    main()

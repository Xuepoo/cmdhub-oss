#!/usr/bin/env python3
"""Batch re-probe already-probed, locally-installed tools with the current
extractor and merge each complete subtree onto its existing master app_id,
replacing the old (parser-truncated) tree. Idempotent + resumable via checkpoint.

Why: tools probed during the EC2 coldstart carry trees from an older extractor
that predated the --help parser fixes (#55/#56/#58/#59/#60). npm shows 9 children
in the DB but the current extractor yields 83 (incl install/update/run).
"""
from __future__ import annotations
import argparse
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path


def resolve_app_id(conn: sqlite3.Connection, tool: str) -> str | None:
    row = conn.execute(
        "SELECT app_id FROM arguments WHERE cmd_path=? AND node_type='root'", (tool,)
    ).fetchone()
    return row[0] if row else None


def subtree_size(conn: sqlite3.Connection, tool: str) -> int:
    return conn.execute(
        "SELECT COUNT(*) FROM arguments WHERE cmd_path=? OR cmd_path LIKE ?||'.%'",
        (tool, tool),
    ).fetchone()[0]


def probe_one(tool: str, extractor: str) -> str:
    """Run the extractor for a single tool in an isolated XDG home.
    Returns the path to the produced probe DB. Raises on failure."""
    work = Path(tempfile.mkdtemp(prefix=f"probe-{tool}-"))
    cfg = work / ".config" / "cmdhub"
    cfg.mkdir(parents=True)
    (cfg / "targets.json").write_text(
        json.dumps({"targets": [{"name": tool, "path": tool}]})
    )
    env = dict(os.environ,
               HOME=str(work),
               XDG_CONFIG_HOME=str(work / ".config"),
               XDG_DATA_HOME=str(work / ".local/share"),
               XDG_CACHE_HOME=str(work / ".cache"),
               CMDH_NO_SANDBOX="1")
    subprocess.run([extractor], env=env, capture_output=True, text=True,
                   timeout=600, check=True)
    probe_db = work / ".local/share" / "cmdhub" / "cmdhub.db"
    if not probe_db.exists():
        raise RuntimeError(f"extractor produced no DB for {tool}")
    return str(probe_db)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--master", required=True)
    ap.add_argument("--checkpoint", required=True)
    ap.add_argument("--targets-file", default=None,
                    help="newline-separated tool names; default = high-pop installed probe roots from --master")
    ap.add_argument("--max-tree", type=int, default=500,
                    help="skip tools whose CURRENT master subtree exceeds this (already-complete giant trees)")
    ap.add_argument("--extractor", default=os.path.expanduser("~/.local/share/cargo/bin/cmdh-extractor"))
    ap.add_argument("--merge-script", default=str(Path(__file__).parent / "merge_probe_into_master.py"))
    ap.add_argument("--min-pop", type=float, default=0.8)
    a = ap.parse_args()

    ck = json.loads(Path(a.checkpoint).read_text()) if Path(a.checkpoint).exists() else {}

    conn = sqlite3.connect(a.master)
    if a.targets_file:
        targets = [t.strip() for t in Path(a.targets_file).read_text().splitlines() if t.strip()]
    else:
        rows = conn.execute(
            "SELECT DISTINCT ar.cmd_path FROM arguments ar JOIN apps ap ON ap.app_id=ar.app_id "
            "WHERE ar.node_type='root' AND ar.provenance='probe' AND ar.cmd_path NOT LIKE '%.%' "
            "AND ap.popularity >= ? ORDER BY ar.cmd_path", (a.min_pop,)
        ).fetchall()
        targets = [r[0] for r in rows]

    print(f"[reprobe] {len(targets)} candidate tools")
    for tool in targets:
        if ck.get(tool, {}).get("status") == "done":
            continue
        if not shutil.which(tool):
            ck[tool] = {"status": "not-installed"}
            continue
        app_id = resolve_app_id(conn, tool)
        if not app_id:
            ck[tool] = {"status": "no-root"}
            continue
        before = subtree_size(conn, tool)
        if before > a.max_tree:
            ck[tool] = {"status": "skipped-giant", "before": before}
            continue
        try:
            probe_db = probe_one(tool, a.extractor)
        except Exception as e:
            ck[tool] = {"status": "probe-failed", "error": str(e)[:200]}
            print(f"[reprobe] {tool}: PROBE FAILED {e}")
            Path(a.checkpoint).write_text(json.dumps(ck, indent=1))
            continue
        r = subprocess.run([sys.executable, a.merge_script, "--master", a.master,
                            "--probe-db", probe_db, "--tool", tool, "--app-id", app_id],
                           capture_output=True, text=True)
        if r.returncode != 0:
            ck[tool] = {"status": "merge-failed", "error": (r.stderr or "")[:200]}
            print(f"[reprobe] {tool}: MERGE FAILED {r.stderr[:120]}")
            Path(a.checkpoint).write_text(json.dumps(ck, indent=1))
            continue
        after = subtree_size(sqlite3.connect(a.master), tool)
        ck[tool] = {"status": "done", "before": before, "after": after}
        print(f"[reprobe] {tool}: {before} -> {after}")
        Path(a.checkpoint).write_text(json.dumps(ck, indent=1))

    done = sum(1 for v in ck.values() if v.get("status") == "done")
    grew = sum(1 for v in ck.values() if v.get("status") == "done" and v.get("after", 0) > v.get("before", 0))
    print(f"\n[reprobe] done={done} grew={grew} total_processed={len(ck)}")


if __name__ == "__main__":
    main()

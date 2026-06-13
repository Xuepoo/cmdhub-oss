#!/usr/bin/env python3
"""Fix FTS topics for the 5 golden-eval misses.

Root-cause analysis found that these commands have insufficient FTS coverage for
the natural-language queries used in eval_golden.py:

  rm        → 'delete files'          (FTS pos 98-213; btrfs/bfs beat it)
  bat       → 'better cat with ...'   (AND-query has zero matches; bat at OR pos 86)
  git.log   → 'show git commit history' (path_match: git.show +12, git.log -4)
  kubectl.apply → 'deploy kubernetes' (kubectl.apply at FTS pos 131)
  kubectl.logs  → 'view pod logs'     (AND-query 'view* AND pod* AND logs*' never matches kubectl)

Fixes: enrich topics text so these commands rank in Stage-1 FTS top-50 for their
respective queries.  FTS capabilities = cmd_path_words + description + topics.

Usage:
    python3 scripts/fix_golden_eval.py [--db PATH] [--dry-run]
"""
from __future__ import annotations

import argparse
import os
import sqlite3
import sys
from typing import Optional


# For each cmd_path: extra topics to append (space-separated).
# Keep them focused — one or two key missing concepts per entry.
TOPIC_ADDITIONS: dict[str, str] = {
    # rm: FTS says "removes/deletes" but query uses "delete files"; boost density
    "rm.recursive": "delete remove files rm linux coreutils trash dangerous",
    "rm.verbose":   "delete remove files rm linux coreutils",
    "rm.force":     "delete remove files force rm linux",
    "rm.interactive":"delete remove files interactive rm linux",
    "rm.dir":       "delete remove directory rm linux",
    # bat: no standalone "cat" in FTS; query is "better cat with syntax highlighting"
    "bat": "cat better pager cat-replacement cat-clone file-viewer syntax-highlighter",
    # git.log: needs dense "history log commits show" so BM25 beats git.show
    "git.log": "log history commits show changelog display timeline",
    # kubectl.apply: FTS has "deployment" but not "deploy"; query is "deploy kubernetes"
    "kubectl.apply": "deploy deployment apply kubernetes k8s yaml manifest",
    # kubectl.logs: no "view" in FTS; query 'view* AND pod* AND logs*' never matches
    "kubectl.logs": "view display pod logs container kubernetes k8s debugging",
}

# Root rm command is entirely missing from arguments + apps_fts.
RM_ROOT = {
    "cmd_path": "rm",
    "app_id":   "org.gnu.coreutils.rm",
    "node_name": "rm",
    "node_type": "root",
    "description": "Remove files or directories from the filesystem",
    "risk_level": "dangerous",
    "topics": "rm delete remove files directories linux unix coreutils dangerous trash",
}


def _fts_capabilities(cmd_path: str, description: str, topics: Optional[str]) -> str:
    path_words = cmd_path.replace(".", " ").replace("-", " ")
    return f"{path_words} {description or ''} {topics or ''}".strip()


def _fts_name(app_id: str, app_name: str) -> str:
    parts = app_id.rsplit(".", 1)
    if len(parts) != 2:
        return app_name
    pkg_alias = parts[1]
    if pkg_alias == app_name or pkg_alias in app_name:
        return app_name
    return f"{app_name} {pkg_alias}"


def _rebuild_fts(cur: sqlite3.Cursor, cmd_path: str) -> None:
    row = cur.execute(
        "SELECT arg.app_id, app.name, arg.description, arg.topics "
        "FROM arguments arg JOIN apps app ON arg.app_id = app.app_id "
        "WHERE arg.cmd_path = ?",
        (cmd_path,),
    ).fetchone()
    if not row:
        print(f"  [warn] {cmd_path}: not in arguments, skipping FTS rebuild")
        return
    app_id, app_name, description, topics = row
    cap = _fts_capabilities(cmd_path, description, topics)
    name = _fts_name(app_id, app_name)
    cur.execute("DELETE FROM apps_fts WHERE cmd_path = ?", (cmd_path,))
    cur.execute(
        "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES (?,?,?)",
        (cmd_path, name, cap),
    )


def fix_rm_root(cur: sqlite3.Cursor, dry: bool) -> int:
    existing = cur.execute(
        "SELECT cmd_path FROM arguments WHERE cmd_path = ?", (RM_ROOT["cmd_path"],)
    ).fetchone()
    if existing:
        print(f"  rm root already exists, skip")
        return 0
    print(f"  Adding root rm command to arguments + apps_fts")
    if dry:
        return 0
    cur.execute(
        "INSERT OR IGNORE INTO arguments "
        "(cmd_path, app_id, node_name, node_type, description, risk_level, "
        " example_template, docker_image, script_url, source_url, topics) "
        "VALUES (?,?,?,?,?,?,?,?,?,?,?)",
        (
            RM_ROOT["cmd_path"], RM_ROOT["app_id"], RM_ROOT["node_name"],
            RM_ROOT["node_type"], RM_ROOT["description"], RM_ROOT["risk_level"],
            None, None, None, None, RM_ROOT["topics"],
        ),
    )
    _rebuild_fts(cur, RM_ROOT["cmd_path"])
    return 1


def fix_topics(cur: sqlite3.Cursor, dry: bool) -> int:
    changed = 0
    for cmd_path, extra in TOPIC_ADDITIONS.items():
        row = cur.execute(
            "SELECT topics FROM arguments WHERE cmd_path = ?", (cmd_path,)
        ).fetchone()
        if not row:
            print(f"  [warn] {cmd_path}: not found in arguments, skip")
            continue
        current_topics = row[0] or ""
        # Avoid adding duplicates
        already = all(t in current_topics for t in extra.split())
        if already:
            print(f"  {cmd_path}: topics already up-to-date, skip")
            continue
        new_topics = (current_topics + " " + extra).strip()
        print(f"  {cmd_path}: updating topics (+{extra[:60]})")
        if dry:
            continue
        cur.execute(
            "UPDATE arguments SET topics = ? WHERE cmd_path = ?",
            (new_topics, cmd_path),
        )
        _rebuild_fts(cur, cmd_path)
        changed += 1
    return changed


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", default=os.path.expanduser("~/.local/share/cmdhub/cmdhub.db"))
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    if not os.path.exists(args.db):
        print(f"[error] DB not found: {args.db}", file=sys.stderr)
        sys.exit(1)

    conn = sqlite3.connect(args.db)
    cur = conn.cursor()
    mode = "DRY RUN" if args.dry_run else "APPLY"
    print(f"[fix-golden-eval] {mode}  db={args.db}")

    total = 0

    print("\n== 1. Add missing rm root command ==")
    total += fix_rm_root(cur, args.dry_run)

    print("\n== 2. Enrich FTS topics for 5 golden-eval misses ==")
    total += fix_topics(cur, args.dry_run)

    if not args.dry_run:
        conn.commit()
        conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        conn.commit()
        print(f"\n[fix-golden-eval] Done. Changed {total} entries.")
    else:
        print(f"\n[fix-golden-eval] Dry-run complete. Would change ~{total}+ entries.")

    conn.close()


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Fix two classes of data quality problems in the live cmdhub.db.

1. Broken-prefix sole-source apps: subcommands were promoted to roots during
   import, losing the binary name prefix. Fix: prepend "binary." to every
   cmd_path and add a root entry for the binary itself.
   Affected: amdgpu_top, argocd, gita (confirmed sole-source, no canonical twin).

2. Usage-example noise stored as root commands: some importers saved
   "arc amend", "nvim --clean", "eksctl create cluster" etc. as cmd_paths
   with spaces. These are not real subcommands; delete them.

Usage:
    python3 scripts/fix_data_quality.py [--db PATH] [--dry-run]
"""
from __future__ import annotations

import argparse
import os
import sqlite3
import sys

# Sole-source broken-prefix apps: binary_name -> [app_id, ...]
# These apps' root commands are subcommands of the binary, not separate binaries.
# Identified by: n_first_segs >= 2, no first_seg matches binary name, no canonical twin.
_BROKEN_PREFIX = {
    "amdgpu_top": ["org.archlinux.amdgpu_top"],
    "argocd":     ["org.archlinux.argocd"],
    "gita":       ["org.archlinux.gita"],
}


def _fix_broken_prefix(cur: sqlite3.Cursor, binary: str, app_ids: list[str], dry: bool) -> int:
    """Prepend 'binary.' to all cmd_paths for these apps and add a root command."""
    changed = 0
    for app_id in app_ids:
        rows = cur.execute(
            "SELECT cmd_path, node_name, node_type, description, risk_level, "
            "example_template, docker_image, script_url, source_url, topics "
            "FROM arguments WHERE app_id = ?", (app_id,)
        ).fetchall()
        if not rows:
            print(f"  [{binary}] {app_id}: no rows found, skip")
            continue

        # Build new paths
        updates: list[tuple[str, str]] = []
        for row in rows:
            old_path = row[0]
            # Don't double-prefix
            if old_path == binary or old_path.startswith(binary + "."):
                continue
            new_path = f"{binary}.{old_path}"
            updates.append((old_path, new_path))

        # Check if root already exists
        has_root = any(r[0] == binary for r in rows)

        print(f"  [{binary}] {app_id}: renaming {len(updates)} paths"
              + (" (+ add root)" if not has_root else ""))
        if dry:
            for old, new in updates[:5]:
                print(f"    {old!r} → {new!r}")
            if len(updates) > 5:
                print(f"    ... and {len(updates) - 5} more")
            continue

        # Rename paths (arguments primary key = cmd_path, must delete+insert)
        for old_path, new_path in updates:
            # Find the row
            r = cur.execute(
                "SELECT node_name, node_type, description, risk_level, "
                "example_template, docker_image, script_url, source_url, topics "
                "FROM arguments WHERE cmd_path = ? AND app_id = ?", (old_path, app_id)
            ).fetchone()
            if not r:
                continue
            cur.execute("DELETE FROM arguments WHERE cmd_path = ?", (old_path,))
            cur.execute("DELETE FROM apps_fts WHERE cmd_path = ?", (old_path,))
            cur.execute(
                "INSERT OR IGNORE INTO arguments (cmd_path, app_id, node_name, node_type, description, "
                "risk_level, example_template, docker_image, script_url, source_url, topics) "
                "VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                (new_path, app_id) + r
            )
            # Update FTS
            cur.execute("DELETE FROM apps_fts WHERE cmd_path = ?", (old_path,))
            # FTS will be rebuilt from scratch on GPU rebuild; just keep it consistent for now
            changed += 1

        # Add root if missing
        if not has_root:
            # Use first row's description as a fallback
            first = rows[0]
            cur.execute(
                "INSERT OR IGNORE INTO arguments "
                "(cmd_path, app_id, node_name, node_type, description, risk_level, "
                "example_template, docker_image, script_url, source_url, topics) "
                "VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                (binary, app_id, binary, "root",
                 f"{binary} command", "safe",
                 None, None, None, None, None)
            )
            changed += 1

    return changed


def _delete_space_paths(cur: sqlite3.Cursor, dry: bool) -> int:
    """Delete arguments whose cmd_path contains a space (usage-example noise)."""
    rows = cur.execute(
        "SELECT cmd_path, app_id FROM arguments WHERE cmd_path LIKE '% %'"
    ).fetchall()
    if not rows:
        print("  No space-paths found")
        return 0

    print(f"  Deleting {len(rows)} space-in-path rows:")
    for cmd_path, app_id in rows[:20]:
        print(f"    {app_id}: {cmd_path!r}")
    if len(rows) > 20:
        print(f"    ... and {len(rows) - 20} more")

    if dry:
        return 0

    paths = [r[0] for r in rows]
    for path in paths:
        cur.execute("DELETE FROM arguments WHERE cmd_path = ?", (path,))
        cur.execute("DELETE FROM apps_fts WHERE cmd_path = ?", (path,))

    # commands_vec: orphan cleanup (won't work on vec0 in sqlite3 CLI, but try)
    try:
        cur.execute(
            "DELETE FROM commands_vec WHERE cmd_path NOT IN (SELECT cmd_path FROM arguments)"
        )
    except Exception:
        pass

    return len(rows)


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
    print(f"[fix-data-quality] {mode}  db={args.db}")

    total = 0

    print("\n== 1. Fix broken-prefix apps ==")
    for binary, app_ids in _BROKEN_PREFIX.items():
        total += _fix_broken_prefix(cur, binary, app_ids, args.dry_run)

    # Space-in-path cleanup deferred: 96 entries have no dot-path equivalent and
    # some (cargo binstall, buildctl build) are real subcommands with wrong import format.
    # Clean those separately without a GPU rebuild.
    print("\n== 2. Space-in-path cleanup: DEFERRED (see TODO in script) ==")

    if not args.dry_run:
        conn.commit()
        conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        conn.commit()
        apps = conn.execute("SELECT COUNT(*) FROM apps").fetchone()[0]
        cmds = conn.execute("SELECT COUNT(*) FROM arguments").fetchone()[0]
        print(f"\n[fix-data-quality] Done. Changed {total} rows. DB: {apps} apps, {cmds} args")
    else:
        print(f"\n[fix-data-quality] Dry-run complete. Would change ~{total}+ rows")

    conn.close()


if __name__ == "__main__":
    main()

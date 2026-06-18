#!/usr/bin/env python3
"""Fabrication sweep — identify and delete fake <tool>-<subcmd> apps from cmdhub.db.

A "fabricated" app is one whose commands use a hyphenated binary root that duplicates
a real sub-command group already covered by a probe-verified root tool.

Concretely: if app A has cmd_paths rooted at "podman-image.*" (all inferred) but
"org.cmdhub.podman" already has probe rows for "podman.image.*", then A is a
fabrication of that subcommand group and should be removed.

Heuristic per candidate app:
  1. Derive binary_root = first segment of cmd_path (e.g. "podman-image")
  2. binary_root must match ^<tool>-<sub>$ (exactly one hyphen segment)
  3. Root tool <tool> must have probe rows in the DB
  4. <tool>.<sub>.* must exist as probe cmd_paths
  5. Candidate has ZERO probe/manual rows (100% inferred)
  6. Candidate's app_id is NOT in a real package namespace (AUR/crates/npm/…)

Default: dry-run, emit TSV to stdout.  --apply deletes from the DB.

Usage:
    uv run python3 scripts/sweep_fabrications.py --db /path/to/cmdhub.db
    uv run python3 scripts/sweep_fabrications.py --db /path/to/cmdhub.db --apply
    uv run python3 scripts/sweep_fabrications.py --db /path/to/cmdhub.db --roots podman,docker
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

# ---------------------------------------------------------------------------
# Real-package namespace guards — apps under these prefixes are genuine
# upstream packages even if their names look like <tool>-<sub>.
# ---------------------------------------------------------------------------
REAL_PACKAGE_PREFIXES = (
    "org.archlinux.",
    "org.tldr.",
    "io.crates.",
    "io.pypi.",
    "io.npmjs.",
    "com.npmjs.",
    "io.rubygems.",
    "io.homebrew.",
    "com.github.",
    "org.git-extras.",
    "org.pypi.",
)


def _is_real_package(app_id: str) -> bool:
    return any(app_id.startswith(p) for p in REAL_PACKAGE_PREFIXES)


def _parse_hyphen_root(binary_root: str) -> tuple[str, str] | None:
    """If binary_root looks like '<tool>-<sub>', return (tool, sub), else None.

    We only split on the FIRST hyphen so 'podman-image' → ('podman', 'image').
    Multi-segment subs like 'docker-buildx-build' are not targeted here (they
    are real binaries; expand REAL_PACKAGE_PREFIXES instead).
    """
    m = re.fullmatch(r"([a-z][a-z0-9_]+)-([a-z][a-z0-9_]+)", binary_root)
    if m:
        return m.group(1), m.group(2)
    return None


def sweep(db_path: str, roots_filter: set[str] | None = None) -> list[dict]:
    """Return list of fabrication candidates from the SQLite DB.

    Each entry: {app_id, name, binary_root, probe_root, probe_sub_path,
                 cmd_count, sample_cmds}
    """
    import sqlite3

    conn = sqlite3.connect(db_path, check_same_thread=False)
    conn.row_factory = sqlite3.Row

    # 1. Find all binary roots that have probe rows.
    #    binary_root = first segment of cmd_path (before first dot).
    probe_roots_rows = conn.execute("""
        SELECT
            CASE
                WHEN instr(cmd_path, '.') > 0
                THEN substr(cmd_path, 1, instr(cmd_path, '.')-1)
                ELSE cmd_path
            END AS binary_root,
            COUNT(*) AS probe_cnt
        FROM arguments
        WHERE provenance = 'probe'
        GROUP BY binary_root
        HAVING probe_cnt > 0
    """).fetchall()
    probe_roots: dict[str, int] = {r["binary_root"]: r["probe_cnt"] for r in probe_roots_rows}

    # 2. Collect all probe cmd_paths for fast prefix lookup.
    probe_paths_rows = conn.execute("""
        SELECT cmd_path FROM arguments WHERE provenance = 'probe'
    """).fetchall()
    probe_paths: set[str] = {r["cmd_path"] for r in probe_paths_rows}

    # 3. For each app, get its binary_root (from the first cmd_path), provenance
    #    distribution, and cmd count.
    app_rows = conn.execute("""
        SELECT
            a.app_id,
            a.name,
            MIN(arg.cmd_path) AS first_cmd,
            COUNT(arg.cmd_path) AS cmd_count,
            SUM(CASE WHEN arg.provenance = 'probe' THEN 1 ELSE 0 END) AS probe_cmds,
            SUM(CASE WHEN arg.provenance = 'manual' THEN 1 ELSE 0 END) AS manual_cmds,
            GROUP_CONCAT(arg.cmd_path, '|||') AS all_cmds
        FROM apps a
        JOIN arguments arg ON arg.app_id = a.app_id
        GROUP BY a.app_id, a.name
    """).fetchall()

    candidates = []
    for row in app_rows:
        app_id: str = row["app_id"]

        # Skip real package namespaces
        if _is_real_package(app_id):
            continue

        # Guard: must be 100% inferred
        if row["probe_cmds"] > 0 or row["manual_cmds"] > 0:
            continue

        # Derive binary_root from the first cmd_path
        first_cmd: str = row["first_cmd"] or ""
        binary_root = first_cmd.split(".")[0] if "." in first_cmd else first_cmd
        if not binary_root:
            continue

        # Guard: binary_root must follow <tool>-<sub> pattern
        parsed = _parse_hyphen_root(binary_root)
        if not parsed:
            continue
        tool, sub = parsed

        # Filter to specific roots if requested
        if roots_filter and tool not in roots_filter:
            continue

        # Guard: the root tool must have probe rows
        if tool not in probe_roots:
            continue

        # Guard: <tool>.<sub>.* must exist as probe cmd_paths
        sub_prefix = f"{tool}.{sub}"
        matching = [p for p in probe_paths if p == sub_prefix or p.startswith(sub_prefix + ".")]
        if not matching:
            continue

        sample_cmds = (row["all_cmds"] or "").split("|||")[:3]
        candidates.append({
            "app_id": app_id,
            "name": row["name"],
            "binary_root": binary_root,
            "probe_root": tool,
            "probe_sub_path": sub_prefix,
            "cmd_count": row["cmd_count"],
            "probe_sub_count": len(matching),
            "sample_cmds": sample_cmds,
        })

    conn.close()
    return candidates


def apply_deletions(db_path: str, app_ids: set[str]) -> tuple[int, int]:
    """Delete fabricated apps and their commands from the DB.

    Returns (apps_deleted, args_deleted).
    """
    import sqlite3

    conn = sqlite3.connect(db_path)
    placeholders = ",".join("?" * len(app_ids))
    id_list = list(app_ids)

    args_deleted = conn.execute(
        f"DELETE FROM arguments WHERE app_id IN ({placeholders})", id_list
    ).rowcount
    apps_deleted = conn.execute(
        f"DELETE FROM apps WHERE app_id IN ({placeholders})", id_list
    ).rowcount
    conn.commit()
    conn.close()
    return apps_deleted, args_deleted


def main() -> None:
    ap = argparse.ArgumentParser(description="Fabrication sweep for cmdhub.db")
    ap.add_argument("--db", required=True, help="Path to cmdhub.db SQLite file")
    ap.add_argument("--apply", action="store_true", help="Delete fabrications from the DB")
    ap.add_argument(
        "--roots", "-r",
        help="Comma-separated root tool names to limit sweep (e.g. podman,docker). Default: all.",
        default=None,
    )
    args = ap.parse_args()

    db_path = args.db
    if not Path(db_path).exists():
        print(f"Error: {db_path} not found", file=sys.stderr)
        sys.exit(1)

    roots_filter: set[str] | None = None
    if args.roots:
        roots_filter = {r.strip() for r in args.roots.split(",") if r.strip()}

    candidates = sweep(db_path, roots_filter=roots_filter)

    # Print TSV for human review
    header = "\t".join([
        "app_id", "name", "binary_root", "probe_root", "probe_sub_path",
        "cmd_count", "probe_sub_count", "sample_cmds",
    ])
    print(header)
    for c in candidates:
        print("\t".join([
            c["app_id"],
            c["name"],
            c["binary_root"],
            c["probe_root"],
            c["probe_sub_path"],
            str(c["cmd_count"]),
            str(c["probe_sub_count"]),
            "; ".join(c["sample_cmds"]),
        ]))

    print(f"\n[sweep] Found {len(candidates)} fabrication candidates.", file=sys.stderr)

    if not candidates:
        print("[sweep] Nothing to delete.", file=sys.stderr)
        return

    if not args.apply:
        print(
            "[sweep] Dry-run — review TSV above, then re-run with --apply to delete.",
            file=sys.stderr,
        )
        return

    bad_ids = {c["app_id"] for c in candidates}
    apps_del, args_del = apply_deletions(db_path, bad_ids)
    print(
        f"[sweep] Deleted {apps_del} apps, {args_del} commands from {db_path}",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()

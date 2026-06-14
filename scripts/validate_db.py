#!/usr/bin/env python3
"""Validate cmdhub.db data quality.

Checks:
  1. Schema integrity (tables, FTS5, vec0 consistency)
  2. node_type invariant (root vs sub)
  3. Install instruction format (no nulls, no empty strings, valid JSON)
  4. Description quality (non-empty, meaningful length)
  5. Known tool coverage (expected tools + subcommands present)
  6. Subcommand completeness for high-value CLIs

Usage:
    uv run --with sqlite-vec python3 scripts/validate_db.py [--db PATH] [--json]
"""
from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any

DEFAULT_DB = os.path.expanduser("~/.local/share/cmdhub/cmdhub.db")


# ── Known tool registry ───────────────────────────────────────────────────────
# Format: "tool": [list of expected subcommand names]
KNOWN_TOOLS: dict[str, list[str]] = {
    "git": ["commit", "push", "pull", "clone", "branch", "checkout", "merge",
            "rebase", "log", "diff", "status", "add", "remote", "fetch", "stash",
            "tag", "reset", "revert", "cherry-pick"],
    "docker": ["run", "build", "push", "pull", "ps", "images", "exec", "logs",
               "stop", "rm", "rmi", "network", "volume", "compose", "login",
               "tag", "inspect", "cp", "start"],
    "kubectl": ["get", "apply", "delete", "describe", "create", "exec", "logs",
                "port-forward", "scale", "rollout", "config"],
    "gh": ["pr", "issue", "repo", "auth", "workflow", "release", "gist",
           "api", "codespace", "secret", "run"],
    "aws": ["s3", "ec2", "iam", "lambda", "rds", "ecs", "eks", "cloudformation",
            "ssm", "secretsmanager", "configure"],
    "gcloud": ["compute", "container", "storage", "iam", "sql", "functions",
               "run", "auth", "config", "projects"],
    "npm": ["install", "run", "test", "publish", "init", "update", "uninstall",
            "list", "audit", "ci"],
    "cargo": ["build", "test", "run", "check", "clippy", "fmt", "publish",
              "install", "update", "new", "init"],
    "uv": ["run", "add", "remove", "sync", "lock", "pip", "venv", "tool",
           "python", "init", "build", "publish"],
    "yay": [],   # yay uses flags (-S, -R, -Syu), not named subcommands
    "helm": ["install", "upgrade", "uninstall", "list", "repo", "search",
             "template", "lint", "package", "rollback"],
    "terraform": ["init", "plan", "apply", "destroy", "validate", "fmt",
                  "state", "workspace", "output", "import"],
    "systemctl": ["start", "stop", "restart", "status", "enable", "disable",
                  "reload", "list-units", "daemon-reload"],
    "nvim": [],   # no expected subcommands, just check it exists
    "vim": [],
    "tmux": ["new", "attach", "detach", "kill-session", "list-sessions"],
    "fzf": [],
    "rg": [],        # ripgrep binary is rg
}

# Expected install key patterns per tool source
VALID_INSTALL_KEYS = {
    "pacman", "yay", "paru", "brew", "apt", "apt-get", "dnf", "zypper",
    "snap", "flatpak", "scoop", "winget", "cargo", "npm", "pip", "go",
    "curl", "wget", "docker", "nix",
    # Modern alternatives
    "uv", "pipx", "nix-env", "nix_env", "choco", "chocolatey", "apk",
    "emerge", "pkg", "xbps", "guix", "conda", "mamba",
}

# Descriptions that are clearly bad (placeholder / too generic)
BAD_DESCRIPTION_PATTERNS = [
    "a tool", "a cli", "a command", "a utility", "todo", "placeholder",
    "unknown", "n/a", "none", "example", "test",
]


# ── DB helpers ────────────────────────────────────────────────────────────────

def open_db(path: str) -> sqlite3.Connection:
    try:
        import sqlite_vec
        conn = sqlite3.connect(path, check_same_thread=False)
        conn.enable_load_extension(True)
        sqlite_vec.load(conn)
        conn.enable_load_extension(False)
    except ImportError:
        conn = sqlite3.connect(path, check_same_thread=False)
    conn.row_factory = sqlite3.Row
    return conn


def query(conn: sqlite3.Connection, sql: str, params: tuple = ()) -> list[sqlite3.Row]:
    return conn.execute(sql, params).fetchall()


# ── Individual checks ─────────────────────────────────────────────────────────

class CheckResult:
    def __init__(self, name: str) -> None:
        self.name = name
        self.passed = 0
        self.failed = 0
        self.warnings: list[str] = []
        self.errors: list[str] = []

    @property
    def ok(self) -> bool:
        return len(self.errors) == 0

    def warn(self, msg: str) -> None:
        self.warnings.append(msg)

    def fail(self, msg: str) -> None:
        self.errors.append(msg)
        self.failed += 1

    def ok_n(self, n: int = 1) -> None:
        self.passed += n


def check_schema(conn: sqlite3.Connection) -> CheckResult:
    r = CheckResult("schema")
    expected_tables = {"apps", "arguments", "apps_fts", "commands_vec", "sync_meta"}
    actual = {row[0] for row in query(conn, "SELECT name FROM sqlite_master WHERE type='table'")}
    for t in expected_tables:
        if t in actual:
            r.ok_n()
        else:
            r.fail(f"Missing table: {t}")
    return r


def check_node_type_invariant(conn: sqlite3.Connection) -> CheckResult:
    r = CheckResult("node_type_invariant")

    # root commands with node_type='sub' (bug we fixed)
    rows = query(conn, """
        SELECT cmd_path, node_type FROM arguments
        WHERE node_type = 'sub' AND cmd_path NOT LIKE '%.%'
        LIMIT 20
    """)
    if rows:
        r.fail(f"{len(rows)} root cmd_paths with node_type='sub': {[row['cmd_path'] for row in rows[:5]]}")
    else:
        r.ok_n()

    # subcommands with node_type='root'
    rows = query(conn, """
        SELECT cmd_path, node_type FROM arguments
        WHERE node_type = 'root' AND cmd_path LIKE '%.%'
        LIMIT 20
    """)
    if rows:
        r.fail(f"{len(rows)} sub cmd_paths with node_type='root': {[row['cmd_path'] for row in rows[:5]]}")
    else:
        r.ok_n()

    # NULL node_types
    rows = query(conn, "SELECT COUNT(*) as n FROM arguments WHERE node_type IS NULL OR node_type NOT IN ('root','sub')")
    n = rows[0]["n"]
    if n:
        r.fail(f"{n} arguments with invalid node_type")
    else:
        r.ok_n()

    return r


def check_install_instructions(conn: sqlite3.Connection) -> CheckResult:
    r = CheckResult("install_instructions")

    # Apps with NULL install_instructions
    rows = query(conn, "SELECT COUNT(*) as n FROM apps WHERE install_instructions IS NULL")
    null_count = rows[0]["n"]
    total = query(conn, "SELECT COUNT(*) as n FROM apps")[0]["n"]
    r.warn(f"{null_count}/{total} apps have NULL install_instructions ({null_count*100//total}%)")

    # Apps with malformed JSON
    rows = query(conn, "SELECT app_id, name, install_instructions FROM apps WHERE install_instructions IS NOT NULL")
    bad_json, empty_val, unknown_key = [], [], []
    for row in rows:
        raw = row["install_instructions"]
        try:
            obj = json.loads(raw)
        except json.JSONDecodeError:
            bad_json.append(row["name"])
            continue
        for k, v in obj.items():
            if v == "" or v is None:
                empty_val.append(f"{row['name']}.{k}")
            if k not in VALID_INSTALL_KEYS:
                r.warn(f"Unknown install key '{k}' in {row['name']}")

    if bad_json:
        r.fail(f"{len(bad_json)} apps have malformed install_instructions JSON: {bad_json[:5]}")
    else:
        r.ok_n()

    if empty_val:
        r.fail(f"{len(empty_val)} apps have empty install values: {empty_val[:5]}")
    else:
        r.ok_n()

    return r


def check_description_quality(conn: sqlite3.Connection) -> CheckResult:
    r = CheckResult("description_quality")

    # Very short descriptions (< 4 chars — genuinely useless)
    rows = query(conn, """
        SELECT cmd_path, description FROM arguments
        WHERE length(description) < 4
        LIMIT 20
    """)
    if rows:
        r.fail(f"{len(rows)} arguments with description < 5 chars: {[row['cmd_path'] for row in rows[:5]]}")
    else:
        r.ok_n()

    # NULL descriptions
    rows = query(conn, "SELECT COUNT(*) as n FROM arguments WHERE description IS NULL OR description = ''")
    n = rows[0]["n"]
    if n:
        r.fail(f"{n} arguments with NULL/empty description")
    else:
        r.ok_n()

    # Placeholder descriptions
    bad_descs = []
    rows = query(conn, "SELECT cmd_path, description FROM arguments WHERE length(description) < 50 LIMIT 500")
    for row in rows:
        desc_lower = row["description"].lower().strip()
        if any(desc_lower == p or desc_lower.startswith(p + " ") for p in BAD_DESCRIPTION_PATTERNS):
            bad_descs.append(row["cmd_path"])

    if bad_descs:
        r.warn(f"{len(bad_descs)} potentially placeholder descriptions: {bad_descs[:5]}")
    else:
        r.ok_n()

    return r


def check_tool_coverage(conn: sqlite3.Connection) -> CheckResult:
    r = CheckResult("tool_coverage")
    missing_tools: list[str] = []
    incomplete: dict[str, list[str]] = {}

    for tool, expected_subs in KNOWN_TOOLS.items():
        # Check root command exists
        rows = query(conn, "SELECT cmd_path FROM arguments WHERE cmd_path = ? AND node_type = 'root'", (tool,))
        if not rows:
            missing_tools.append(tool)
            r.fail(f"Tool not found: {tool}")
            continue
        r.ok_n()

        if not expected_subs:
            continue

        # Check expected subcommands exist
        actual_subs = {
            row["cmd_path"].split(".", 1)[1]
            for row in query(conn, "SELECT cmd_path FROM arguments WHERE cmd_path LIKE ? AND node_type = 'sub'",
                             (f"{tool}.%",))
            if "." in row["cmd_path"]
        }
        # Only count direct children (depth 1)
        direct_subs = {s.split(".")[0] for s in actual_subs}
        missing_subs = [s for s in expected_subs if s not in direct_subs]
        if missing_subs:
            incomplete[tool] = missing_subs
            r.warn(f"{tool}: missing subcommands {missing_subs[:8]}"
                   + (f" (+{len(missing_subs)-8} more)" if len(missing_subs) > 8 else ""))
        else:
            r.ok_n()

    return r


def check_subcommand_counts(conn: sqlite3.Connection) -> CheckResult:
    r = CheckResult("subcommand_counts")

    # Apps with 0 subcommands (only root command)
    rows = query(conn, """
        SELECT a.name, COUNT(arg.cmd_path) as cmd_count
        FROM apps a
        JOIN arguments arg ON a.app_id = arg.app_id
        GROUP BY a.app_id, a.name
        HAVING cmd_count = 1
        ORDER BY a.name
        LIMIT 10
    """)
    r.warn(f"{len(rows)} apps have only 1 command (root only, no subcommands) — showing first 10: "
           f"{[row['name'] for row in rows]}")

    # Distribution
    rows = query(conn, """
        SELECT
            SUM(CASE WHEN cmd_count = 1 THEN 1 ELSE 0 END) as only_root,
            SUM(CASE WHEN cmd_count BETWEEN 2 AND 5 THEN 1 ELSE 0 END) as few,
            SUM(CASE WHEN cmd_count BETWEEN 6 AND 20 THEN 1 ELSE 0 END) as medium,
            SUM(CASE WHEN cmd_count > 20 THEN 1 ELSE 0 END) as rich
        FROM (
            SELECT a.app_id, COUNT(arg.cmd_path) as cmd_count
            FROM apps a JOIN arguments arg ON a.app_id = arg.app_id
            GROUP BY a.app_id
        ) t
    """)
    row = rows[0]
    r.warn(f"Subcommand distribution — root-only: {row['only_root']}, 2-5: {row['few']}, "
           f"6-20: {row['medium']}, >20: {row['rich']}")
    r.ok_n()
    return r


def check_fts_vec_consistency(conn: sqlite3.Connection) -> CheckResult:
    r = CheckResult("fts_vec_consistency")
    try:
        args_count = query(conn, "SELECT COUNT(*) as n FROM arguments")[0]["n"]
        fts_count = query(conn, "SELECT COUNT(*) as n FROM apps_fts")[0]["n"]
        vec_count = query(conn, "SELECT COUNT(*) as n FROM commands_vec")[0]["n"]

        if fts_count != args_count:
            r.fail(f"FTS5 row count {fts_count} != arguments count {args_count} (delta: {args_count-fts_count})")
        else:
            r.ok_n()

        if vec_count != args_count:
            r.fail(f"vec0 row count {vec_count} != arguments count {args_count} (delta: {args_count-vec_count})")
        else:
            r.ok_n()
    except Exception as e:
        r.warn(f"Could not check FTS/vec (sqlite-vec not loaded?): {e}")
    return r


def check_high_value_tool_depth(conn: sqlite3.Connection) -> CheckResult:
    """For high-value tools, check that subcommands go at least 2 levels deep."""
    r = CheckResult("subcommand_depth")
    high_value = ["git", "docker", "kubectl", "gh", "aws", "gcloud", "helm", "terraform"]

    for tool in high_value:
        # Check depth-2 subcommands exist (e.g. git.remote.add, docker.network.create)
        rows = query(conn, """
            SELECT cmd_path FROM arguments
            WHERE cmd_path LIKE ? AND cmd_path LIKE '%.%.%'
            LIMIT 1
        """, (f"{tool}.%",))
        direct_subs = query(conn, """
            SELECT COUNT(*) as n FROM arguments
            WHERE cmd_path LIKE ? AND node_type = 'sub'
        """, (f"{tool}.%",))
        n = direct_subs[0]["n"]

        if n == 0:
            r.fail(f"{tool}: no subcommands found at all")
        elif n < 5:
            r.warn(f"{tool}: only {n} subcommands — likely incomplete")
        elif not rows:
            r.warn(f"{tool}: {n} subcommands but none go 2 levels deep")
        else:
            r.ok_n()

    return r


def check_fabricated_examples(rows: list[dict]) -> list[str]:
    """Flag LLM-inferred rows whose example_template contradicts their own cmd_path —
    the signature of a fabricated contract (e.g. contract root `podman-images` but the
    example invokes plain `podman`, or the example uses a subcommand word that is not
    in the contract path). Probe rows are ground truth and never flagged. Warn-only:
    heuristics must not fail the build."""
    warns: list[str] = []
    for r in rows:
        if (r.get("provenance") or "inferred") != "inferred":
            continue
        ex = (r.get("example_template") or "").strip()
        if not ex:
            continue
        cmd_path = r["cmd_path"]
        tokens = ex.split()
        if tokens and tokens[0] == "sudo":
            tokens = tokens[1:]
        if not tokens:
            continue

        # 1. The example must invoke the contract's own binary (root segment).
        root = cmd_path.split(".", 1)[0].lower()
        first = tokens[0].lower()
        if first not in (root, f"./{root}"):
            warns.append(
                f"{cmd_path}: inferred example invokes '{tokens[0]}' but the contract "
                f"root is '{root}' (example: {ex})"
            )
            continue

        # 2. For subcommand contracts, the example's bare words after the binary must
        #    follow the contract's own path segments — a stray word there is usually a
        #    fabricated subcommand. Root contracts are exempt (demoing a sub is fine).
        if "." not in cmd_path:
            continue
        path_words = [seg.lower() for seg in cmd_path.split(".")[1:]]
        for i, t in enumerate(tokens[1:]):
            if t.startswith("-") or "{{" in t or "=" in t:
                break
            if t in ("|", "&&", "||", ";", ">", "<", ">>", "2>"):
                break
            if i >= len(path_words) or path_words[i] != t.lower():
                warns.append(
                    f"{cmd_path}: inferred example subcommand '{t}' not in the "
                    f"contract path (example: {ex})"
                )
                break
    return warns


def check_fabrication_heuristics(conn: sqlite3.Connection) -> CheckResult:
    """Warn-level scan for fabricated inferred examples (see check_fabricated_examples)."""
    r = CheckResult("fabricated_examples")
    cols = {row["name"] for row in query(conn, "PRAGMA table_info(arguments)")}
    prov_sel = "provenance" if "provenance" in cols else "'inferred' AS provenance"
    rows = [
        dict(row)
        for row in query(
            conn,
            f"SELECT cmd_path, node_name, example_template, {prov_sel} FROM arguments "
            "WHERE example_template IS NOT NULL AND example_template != ''",
        )
    ]
    warns = check_fabricated_examples(rows)
    # Warn-only by design; cap the printout so a large registry stays readable.
    for w in warns[:50]:
        r.warn(w)
    if len(warns) > 50:
        r.warn(f"... and {len(warns) - 50} more suspected fabricated examples")
    r.ok_n(len(rows) - len(warns))
    return r


# ── Runner ────────────────────────────────────────────────────────────────────

def run_all_checks(db_path: str) -> dict[str, Any]:
    conn = open_db(db_path)

    meta = query(conn, "SELECT key, value FROM sync_meta")
    meta_dict = {row["key"]: row["value"] for row in meta}

    stats = {
        "db_path": db_path,
        "db_size_mb": round(Path(db_path).stat().st_size / 1_048_576, 1),
        "last_sync": meta_dict.get("last_sync_time", "unknown"),
        "apps": query(conn, "SELECT COUNT(*) as n FROM apps")[0]["n"],
        "commands": query(conn, "SELECT COUNT(*) as n FROM arguments")[0]["n"],
    }

    checks = [
        check_schema(conn),
        check_node_type_invariant(conn),
        check_install_instructions(conn),
        check_description_quality(conn),
        check_tool_coverage(conn),
        check_subcommand_counts(conn),
        check_fts_vec_consistency(conn),
        check_high_value_tool_depth(conn),
        check_fabrication_heuristics(conn),
    ]

    conn.close()
    return {"stats": stats, "checks": checks}


def print_report(result: dict[str, Any]) -> None:
    stats = result["stats"]
    checks: list[CheckResult] = result["checks"]

    print(f"\n{'='*60}")
    print(f"  cmdhub.db Validation Report")
    print(f"{'='*60}")
    print(f"  DB: {stats['db_path']}  ({stats['db_size_mb']} MB)")
    print(f"  Apps: {stats['apps']:,}   Commands: {stats['commands']:,}")
    print(f"{'='*60}\n")

    total_pass = total_fail = 0
    for c in checks:
        status = "✓ PASS" if c.ok else "✗ FAIL"
        print(f"[{status}] {c.name}  (passed={c.passed}, failed={c.failed})")
        for e in c.errors:
            print(f"  ERROR: {e}")
        for w in c.warnings:
            print(f"  WARN:  {w}")
        total_pass += c.passed
        total_fail += c.failed

    print(f"\n{'='*60}")
    overall = "ALL PASSED" if total_fail == 0 else f"{total_fail} FAILURES"
    print(f"  Result: {overall}  ({total_pass} passed, {total_fail} failed)")
    print(f"{'='*60}\n")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", default=DEFAULT_DB, help="Path to cmdhub.db")
    ap.add_argument("--json", action="store_true", help="Output JSON instead of text")
    args = ap.parse_args()

    if not Path(args.db).exists():
        print(f"[error] DB not found: {args.db}", file=sys.stderr)
        sys.exit(1)

    result = run_all_checks(args.db)

    if args.json:
        out = {
            "stats": result["stats"],
            "checks": [
                {
                    "name": c.name,
                    "ok": c.ok,
                    "passed": c.passed,
                    "failed": c.failed,
                    "errors": c.errors,
                    "warnings": c.warnings,
                }
                for c in result["checks"]
            ],
        }
        print(json.dumps(out, indent=2))
    else:
        print_report(result)

    total_fail = sum(c.failed for c in result["checks"])
    sys.exit(0 if total_fail == 0 else 1)


if __name__ == "__main__":
    main()

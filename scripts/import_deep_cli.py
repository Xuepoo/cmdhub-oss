#!/usr/bin/env python3
"""Import deep recursive CLI probe results (cli_<tool>.json) into cmdhub.db.

Generic for ANY multi-subcommand tool (aws, gcloud, az, kubectl, gh, docker,
helm, terraform, wrangler, vercel, ...). Each file is produced by
docker_probe/probe_cli.py against an installed binary and is a list of help
pages: [{"cmd_path": "aws.ec2.create-vpc", "path": [...], "text": "..."}].

For every cli_<tool>.json found in the probe dir this script:
  * picks the canonical app for that tool name = the existing app that already
    owns the most commands (falls back to org.cmdhub.<tool> if the tool is new)
  * consolidates ALL of the tool's commands under that one app_id — cmd_path is
    a GLOBAL primary key, so stale fragmented rows from other sources are removed
    first, then the complete probe is inserted as the single source of truth
  * derives install_instructions generically: a curated override if we have one,
    otherwise inherits the richest existing entry for that tool name
  * extracts man-style DESCRIPTION sections (aws v2) as well as cobra/click help

Usage:
    python3 import_deep_cli.py --probe-dir <deep_probe_results> \
        --db ~/.local/share/cmdhub/cmdhub.db
"""
from __future__ import annotations

import argparse
import json
import re
import sqlite3
from pathlib import Path

# Curated install overrides — ONLY for tools whose canonical install is known and
# whose DB-inherited value is wrong or missing. Everything else inherits from the DB.
# Values are package names (normalize_install_cmd in the CLI expands them) or full commands.
CURATED_INSTALL: dict[str, dict[str, str]] = {
    "aws":       {"pacman": "aws-cli-v2", "brew": "awscli", "apt": "awscli", "pip": "awscli"},
    "gcloud":    {"pacman": "google-cloud-cli", "brew": "google-cloud-sdk", "choco": "gcloudsdk"},
    "az":        {"pacman": "azure-cli", "brew": "azure-cli", "pip": "azure-cli"},
    "kubectl":   {"pacman": "kubectl", "brew": "kubectl", "apt": "kubectl", "scoop": "kubectl", "choco": "kubernetes-cli"},
    "gh":        {"pacman": "github-cli", "brew": "gh", "apt": "gh", "scoop": "gh", "choco": "gh", "winget": "GitHub.cli"},
    "docker":    {"pacman": "docker", "brew": "docker", "apt": "docker.io"},
    "helm":      {"pacman": "helm", "brew": "helm", "scoop": "helm", "choco": "kubernetes-helm"},
    "terraform": {"pacman": "terraform", "brew": "terraform", "scoop": "terraform", "choco": "terraform", "apt": "terraform"},
    "wrangler":  {"npm": "wrangler"},
    "vercel":    {"npm": "vercel"},
    "netlify":   {"npm": "netlify-cli"},
    "firebase":  {"npm": "firebase-tools"},
    "doctl":     {"pacman": "doctl", "brew": "doctl", "snap": "doctl"},
    "flyctl":    {"brew": "flyctl"},
    "oci":       {"pip": "oci-cli", "brew": "oci-cli"},
    "tofu":      {"pacman": "opentofu", "brew": "opentofu", "scoop": "opentofu"},
    "aliyun":    {"brew": "aliyun-cli", "yay": "aliyun-cli-bin"},
    "tccli":     {"pip": "tccli"},
    "openstack": {"pip": "python-openstackclient", "pacman": "python-openstackclient"},
    # Xuepoo's own open-source CLIs — published to crates.io (cargo, all distros),
    # the AUR (yay/paru on Arch), a personal Homebrew tap, and a Scoop bucket. On
    # Debian/Fedora/macOS with no native package the resolver falls back to cargo.
    "vectomancy":          {"cargo": "vectomancy", "yay": "vectomancy", "paru": "vectomancy",
                            "brew": "xuepoo/tap/vectomancy", "scoop": "xuepoo/vectomancy"},
    "sonic-bridge":        {"cargo": "sonic-bridge", "yay": "sonic-bridge", "paru": "sonic-bridge",
                            "brew": "xuepoo/tap/sonic-bridge", "scoop": "xuepoo/sonic-bridge"},
    "waywarp":             {"cargo": "waywarp", "yay": "waywarp", "paru": "waywarp",
                            "brew": "xuepoo/tap/waywarp", "scoop": "xuepoo/waywarp"},
    "alx":                 {"cargo": "agent-lx-music", "yay": "agent-lx-music", "paru": "agent-lx-music",
                            "brew": "xuepoo/tap/agent-lx-music", "scoop": "xuepoo/agent-lx-music"},
    "agent-book-translate":{"cargo": "agent-book-translate", "yay": "agent-book-translate", "paru": "agent-book-translate",
                            "brew": "xuepoo/tap/agent-book-translate", "scoop": "xuepoo/agent-book-translate"},
    "cmdh":                {"brew": "xuepoo/tap/cmdhub", "scoop": "xuepoo/cmdhub"},
    "cmdhub-mcp":          {"brew": "xuepoo/tap/cmdhub", "scoop": "xuepoo/cmdhub"},
}

_ANSI_RE = re.compile(r"\x1b\[[0-9;]*[mGKHF]|\x1b\([AB]")
_OVERSTRIKE_RE = re.compile(r".\x08")
_WS_RE = re.compile(r"\s+")

# Section headers that end a description block (cobra / click / man styles).
_STOP_HEADERS = (
    "usage:", "options:", "flags:", "commands:", "arguments:", "subcommands:",
    "global flags:", "available commands:", "examples:", "synopsis", "positional arguments:",
)

_ERROR_SIGS = ("[error]", "invalid choice", "the following arguments are required",
               "unknown command", "unrecognized arguments")

# Lines that are runtime noise, not description content (e.g. docker -h deprecation warning).
_NOISE_LINE_RE = re.compile(
    r"^(flag shorthand .* deprecated|warning:|warn:|deprecated:|note:)", re.IGNORECASE
)


def _clean(text: str) -> str:
    text = _OVERSTRIKE_RE.sub("", text)
    return _ANSI_RE.sub("", text).strip()


def _is_error(text: str) -> bool:
    low = text.lower()
    return any(s in low for s in _ERROR_SIGS) or len(text) < 30


def _extract_man_description(text: str, max_chars: int = 300) -> str:
    """Extract the DESCRIPTION paragraph from man-style help (aws v2)."""
    lines = text.splitlines()
    for i, line in enumerate(lines):
        if line.strip().upper() == "DESCRIPTION":
            buf: list[str] = []
            for nxt in lines[i + 1:]:
                s = nxt.strip()
                if s and s.upper() == s and re.match(r"^[A-Z][A-Z ]{2,}$", s):
                    break
                if not s:
                    if buf:
                        break
                    continue
                buf.append(s)
                if len(" ".join(buf)) >= max_chars:
                    break
            desc = _WS_RE.sub(" ", " ".join(buf)).strip()
            if desc:
                return desc[:max_chars]
    return ""


def _extract_description(text: str, max_chars: int = 300) -> str:
    """Description for man / cobra / click / argparse help."""
    man = _extract_man_description(text, max_chars)
    if man:
        return man

    lines = text.splitlines()
    buf: list[str] = []
    for line in lines:
        s = line.strip()
        low = s.lower()
        if low.startswith("usage:"):
            continue
        if any(low.startswith(h) for h in _STOP_HEADERS):
            if buf:
                break
            continue
        if not s:
            if buf:
                break
            continue
        if _NOISE_LINE_RE.match(s):
            continue
        buf.append(s)
        if len(" ".join(buf)) >= max_chars:
            break
    desc = _WS_RE.sub(" ", " ".join(buf)).strip()
    return desc[:max_chars] if desc else ""


def _canonical_app_id(conn: sqlite3.Connection, tool: str) -> str:
    """The existing app for this tool that owns the most commands, else org.cmdhub.<tool>."""
    row = conn.execute(
        "SELECT app_id FROM apps WHERE name = ? "
        "ORDER BY (SELECT COUNT(*) FROM arguments WHERE arguments.app_id = apps.app_id) DESC "
        "LIMIT 1",
        (tool,),
    ).fetchone()
    return row[0] if row else f"org.cmdhub.{tool}"


def _inherited_install(conn: sqlite3.Connection, tool: str) -> str | None:
    """Richest existing install_instructions for this tool name (arch/cmdhub preferred)."""
    row = conn.execute(
        "SELECT install_instructions FROM apps "
        "WHERE name = ? AND install_instructions IS NOT NULL AND length(install_instructions) > 2 "
        "ORDER BY CASE "
        "  WHEN app_id LIKE 'org.archlinux.%' THEN 1 "
        "  WHEN app_id LIKE 'org.cmdhub.%' THEN 2 "
        "  WHEN app_id LIKE 'com.%' THEN 3 ELSE 4 END ASC, "
        "  length(install_instructions) DESC LIMIT 1",
        (tool,),
    ).fetchone()
    return row[0] if row else None


def import_deep(probe_dir: Path, db_path: Path) -> None:
    conn = sqlite3.connect(str(db_path))
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA foreign_keys=ON")

    files = sorted(probe_dir.glob("cli_*.json"))
    if not files:
        print(f"[error] no cli_*.json files in {probe_dir}")
        return

    total_tools = total_args = 0

    for f in files:
        tool = f.stem[len("cli_"):]
        try:
            pages = json.loads(f.read_text())
        except json.JSONDecodeError as e:
            print(f"[skip] {f.name}: {e}")
            continue
        if not pages:
            print(f"[skip] {f.name}: empty")
            continue

        # Resolve canonical target and install BEFORE deleting fragments.
        app_id = _canonical_app_id(conn, tool)
        if tool in CURATED_INSTALL:
            install = json.dumps(CURATED_INSTALL[tool])
        else:
            install = _inherited_install(conn, tool)

        # Consolidate: free the global cmd_path namespace for this tool, then drop
        # now-empty same-name fragment apps so search returns one clean source.
        n_del = conn.execute(
            "DELETE FROM arguments WHERE cmd_path = ? OR cmd_path LIKE ?",
            (tool, f"{tool}.%"),
        ).rowcount
        conn.execute(
            "DELETE FROM apps WHERE name = ? AND app_id != ? "
            "AND NOT EXISTS (SELECT 1 FROM arguments WHERE arguments.app_id = apps.app_id)",
            (tool, app_id),
        )

        # Upsert canonical app.
        conn.execute(
            "INSERT INTO apps (app_id, name, install_instructions) VALUES (?, ?, ?) "
            "ON CONFLICT(app_id) DO UPDATE SET "
            "install_instructions = COALESCE(excluded.install_instructions, apps.install_instructions)",
            (app_id, tool, install),
        )

        n_ins = 0
        for page in pages:
            cmd_path = page.get("cmd_path", "")
            path = page.get("path", [])
            text = _clean(page.get("text", ""))
            if not cmd_path or _is_error(text):
                continue
            node_type = "root" if len(path) <= 1 else "sub"
            node_name = path[-1] if path else cmd_path.split(".")[-1]
            desc = _extract_description(text) or f"{' '.join(path)} command"
            conn.execute(
                "INSERT OR REPLACE INTO arguments "
                "(app_id, cmd_path, node_name, node_type, description, risk_level) "
                "VALUES (?, ?, ?, ?, ?, 'safe')",
                (app_id, cmd_path, node_name, node_type, desc),
            )
            n_ins += 1

        total_tools += 1
        total_args += n_ins
        print(f"[ok] {tool} → {app_id}: deleted {n_del} old, inserted {n_ins} commands"
              f"{' (no install)' if not install else ''}")

    conn.commit()
    conn.close()
    print(f"\nDeep import done: {total_tools} tools, {total_args} commands")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--probe-dir", required=True, type=Path)
    ap.add_argument("--db", default=str(Path.home() / ".local/share/cmdhub/cmdhub.db"), type=Path)
    args = ap.parse_args()
    if not args.probe_dir.exists():
        print(f"[error] probe-dir not found: {args.probe_dir}")
        return
    import_deep(args.probe_dir, args.db)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Import VPS probe JSON results directly into cmdhub.db.

Usage:
    python3 import_probe_results.py --probe-dir path/to/probe_results \
        --db ~/.local/share/cmdhub/cmdhub.db

Each probe JSON file is a list of dicts:
  [{"cmd_path": "wandb", "path": ["wandb"], "text": "...", ...}, ...]

The first entry with node_type implied root (path len == 1) becomes the app.
Subsequent entries become argument subcommand records.
"""
from __future__ import annotations

import argparse
import json
import re
import sqlite3
import hashlib
from pathlib import Path

_SOURCE_MAP = {
    "pip": "org.pypi",
    "npm": "com.npmjs",
    "aur": "org.archlinux",
    "cargo": "io.crates",
    "go": "io.pkg.go",
}

_ANSI_RE = re.compile(r"\x1b\[[0-9;]*[mGKHF]|\x1b\([AB]")


def _clean(text: str) -> str:
    return _ANSI_RE.sub("", text).strip()


_SECTION_HEADERS = ("options:", "commands:", "flags:", "arguments:", "subcommands:", "global flags:")

def _extract_description(text: str, max_chars: int = 300) -> str:
    """Extract a short description from --help output.

    Handles two common formats:
    1. Description before Usage: (man-style, argparse)
    2. Indented description after Usage: (Click/Typer style)
    """
    lines = text.splitlines()
    desc_lines: list[str] = []
    past_usage = False

    for line in lines:
        stripped = line.strip()
        low = stripped.lower()

        if low.startswith("usage:"):
            past_usage = True
            continue

        if any(low.startswith(h) for h in _SECTION_HEADERS):
            break

        if not stripped:
            if desc_lines:
                break
            continue

        if past_usage:
            # Click indents description with 2 spaces; accept any non-header content
            desc_lines.append(stripped)
        else:
            desc_lines.append(stripped)

        if len(" ".join(desc_lines)) >= max_chars:
            break

    desc = " ".join(desc_lines)
    return desc[:max_chars] if desc else ""


def _build_install_instructions(source: str, pkg: str) -> str | None:
    """Build a basic install_instructions JSON for known sources."""
    match source:
        case "pip":
            return json.dumps({"pip": f"pip install {pkg}", "uv": f"uv tool install {pkg}"})
        case "npm":
            return json.dumps({"npm": f"npm install -g {pkg}"})
        case "cargo":
            return json.dumps({"cargo": f"cargo install {pkg}"})
        case "go":
            return json.dumps({"cargo": f"go install {pkg}@latest"})
        case "aur":
            return json.dumps({"yay": f"yay -S {pkg}", "paru": f"paru -S {pkg}"})
        case _:
            return None


def _app_id(source: str, pkg: str, binary: str) -> str:
    """Generate a stable app_id.

    For scoped npm packages like @scope/name, use 'scope-name' to avoid collisions
    when multiple packages share the same leaf name (e.g. @angular/cli and @railway/cli).
    """
    prefix = _SOURCE_MAP.get(source, f"unknown.{source}")
    # Normalize: strip leading @, replace / and other non-safe chars with -
    normalized = pkg.lstrip("@").split("[")[0]          # drop extras/versions
    safe = re.sub(r"[^a-zA-Z0-9._-]", "-", normalized)  # @ already stripped, / → -
    safe = re.sub(r"-+", "-", safe).strip("-")           # collapse duplicate dashes
    return f"{prefix}.{safe}"


def _is_error_result(text: str) -> bool:
    """True if the help text is just an error (no real help content)."""
    low = text.lower()
    return ("no such file" in low or "cannot open shared object" in low or
            "error while loading" in low or len(text) < 80)


def import_probe_dir(probe_dir: Path, db_path: Path, packages_toml: Path | None = None) -> None:
    # Build pkg → source mapping from packages.toml if available
    pkg_map: dict[str, tuple[str, str]] = {}  # binary → (source, pkg)
    if packages_toml and packages_toml.exists():
        import tomllib
        with open(packages_toml, "rb") as f:
            config = tomllib.load(f)
        for p in config.get("packages", []):
            binary = p.get("binary", p["pkg"].split("/")[-1].lstrip("@"))
            pkg_map[binary] = (p["source"], p["pkg"])

    conn = sqlite3.connect(str(db_path))
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA synchronous=NORMAL")

    imported_apps = 0
    imported_args = 0

    for json_file in sorted(probe_dir.glob("*.json")):
        fname = json_file.stem  # e.g. "pip_wandb" or "aur_kick-bin"
        parts = fname.split("_", 1)
        if len(parts) != 2:
            print(f"[skip] unexpected filename: {json_file.name}")
            continue
        file_source, file_pkg = parts[0], parts[1].replace("_", "-")

        try:
            pages: list[dict] = json.loads(json_file.read_text())
        except json.JSONDecodeError as e:
            print(f"[skip] JSON error in {json_file.name}: {e}")
            continue

        if not pages:
            print(f"[skip] empty: {json_file.name}")
            continue

        # Find root page (shortest path)
        root_pages = [p for p in pages if len(p.get("path", [])) == 1]
        if not root_pages:
            root_pages = pages[:1]
        root = root_pages[0]
        binary = root.get("path", [file_pkg])[0] if root.get("path") else file_pkg

        # Skip error-only results
        if _is_error_result(root.get("text", "")):
            print(f"[skip] error result: {json_file.name}")
            continue

        # Determine source/pkg
        if binary in pkg_map:
            source, pkg = pkg_map[binary]
        else:
            source, pkg = file_source, file_pkg

        app_id = _app_id(source, pkg, binary)
        install_instructions = _build_install_instructions(source, pkg)
        description = _extract_description(_clean(root.get("text", "")))

        # Insert or update app
        conn.execute(
            "INSERT OR IGNORE INTO apps (app_id, name, install_instructions) VALUES (?, ?, ?)",
            (app_id, binary, install_instructions),
        )
        conn.execute(
            "UPDATE apps SET install_instructions = ? WHERE app_id = ? AND (install_instructions IS NULL OR length(install_instructions) <= 2)",
            (install_instructions, app_id),
        )

        # Insert or update arguments
        for page in pages:
            cmd_path = page.get("cmd_path", "")
            path = page.get("path", [])
            text = _clean(page.get("text", ""))
            node_type = "root" if len(path) <= 1 else "sub"
            node_name = path[-1] if path else cmd_path.split(".")[-1]
            page_desc = _extract_description(text) if node_type == "root" else text[:200]

            if _is_error_result(text):
                continue

            conn.execute("""
                INSERT OR IGNORE INTO arguments
                    (app_id, cmd_path, node_name, node_type, description, risk_level)
                VALUES (?, ?, ?, ?, ?, 'safe')
            """, (app_id, cmd_path, node_name, node_type, page_desc or description))
            if conn.execute("SELECT changes()").fetchone()[0] > 0:
                imported_args += 1

        # Insert/update FTS
        fts_exists = conn.execute(
            "SELECT 1 FROM apps_fts WHERE cmd_path = ?", (binary,)
        ).fetchone()
        if not fts_exists:
            capabilities = _clean(root.get("text", ""))[:2000]
            conn.execute(
                "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES (?, ?, ?)",
                (binary, binary, capabilities),
            )
        imported_apps += 1
        print(f"[ok] {app_id}: {len(pages)} pages")

    conn.commit()
    conn.close()
    print(f"\nImport done: {imported_apps} apps, {imported_args} arguments")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--probe-dir", required=True, type=Path)
    ap.add_argument("--db", default=str(Path.home() / ".local/share/cmdhub/cmdhub.db"), type=Path)
    ap.add_argument("--packages-toml", type=Path,
                    default=Path(__file__).parent.parent.parent / "cmdhub-claw/docker_probe/packages.toml")
    args = ap.parse_args()

    if not args.probe_dir.exists():
        print(f"[error] probe-dir not found: {args.probe_dir}")
        return

    import_probe_dir(args.probe_dir, args.db, args.packages_toml)


if __name__ == "__main__":
    main()

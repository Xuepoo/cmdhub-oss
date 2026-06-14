#!/usr/bin/env python3
"""Enrich cmdhub.db with npm install methods + fix known name-collision installs.

Repology already covers cargo (crates.io) and pip (pypi), but NOT npm. Many modern
CLIs ship only via npm (lighthouse, wrangler, ...). This script:

  1. For tool names lacking an `npm` entry, queries the npm registry and adds
     `npm install -g <name>` ONLY if the package exposes a `bin` (i.e. it's a real
     CLI, not a same-named library — e.g. npm "bat" has no bin, so it's skipped).
  2. Applies CORRECTIONS: overwrite install_instructions for tools whose existing
     data is a known same-name collision (e.g. Google "lighthouse" was tagged with
     the unrelated pacman/brew "lighthouse"; the real install is npm).

Add-only for npm (never overwrites); CORRECTIONS intentionally overwrite.

Usage:
    uv run --with requests python3 scripts/enrich_lang_pm.py \
        [--db ...] [--rate 0.3] [--dry-run]
"""
from __future__ import annotations

import argparse
import json
import re
import sqlite3
import sys
import time
from pathlib import Path

import requests

# Tools whose stored install is a known same-name collision → replace outright.
# Keys are the tool `name`; values are correct install_instructions.
CORRECTIONS: dict[str, dict[str, str]] = {
    "lighthouse": {"npm": "npm install -g lighthouse"},  # Google web-perf tool (npm only)
}

_NAME_RE = re.compile(r"^@?[a-z0-9][a-z0-9._-]*(/[a-z0-9._-]+)?$")  # valid npm package name


def npm_has_bin(session: requests.Session, name: str, proxies: dict) -> bool:
    """True if the npm package exists and its latest version exposes a CLI bin."""
    for attempt in range(3):
        try:
            r = session.get(f"https://registry.npmjs.org/{name}", proxies=proxies, timeout=10)
            if r.status_code == 404:
                return False
            if r.status_code == 429:
                time.sleep(2 * (attempt + 1))
                continue
            r.raise_for_status()
            d = r.json()
            latest = d.get("dist-tags", {}).get("latest")
            ver = d.get("versions", {}).get(latest, {}) if latest else {}
            return bool(ver.get("bin"))
        except Exception:
            if attempt == 2:
                return False
            time.sleep(1)
    return False


def enrich(db_path: str, rate: float, dry_run: bool, proxy: str) -> None:
    proxies = {"https": proxy, "http": proxy} if proxy else {}
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    session = requests.Session()
    session.headers["User-Agent"] = "cmdhub-langpm/0.1 (txx15132@gmail.com)"

    # 1. CORRECTIONS (overwrite known collisions)
    corrected = 0
    for name, install in CORRECTIONS.items():
        j = json.dumps(install, ensure_ascii=False)
        if not dry_run:
            n = conn.execute("UPDATE apps SET install_instructions = ? WHERE name = ?", (j, name)).rowcount
        else:
            n = conn.execute("SELECT COUNT(*) FROM apps WHERE name = ?", (name,)).fetchone()[0]
        if n:
            corrected += 1
            print(f"[correct] {name} → {install}")
    if not dry_run:
        conn.commit()

    # 2. npm add for names lacking an npm entry (skip multi-word/invalid npm names)
    rows = conn.execute("""
        SELECT name, MAX(install_instructions) AS install_instructions
        FROM apps
        WHERE json_extract(install_instructions, '$.npm') IS NULL
        GROUP BY name
        ORDER BY name
    """).fetchall()
    candidates = [r for r in rows if _NAME_RE.match(r["name"] or "")]
    print(f"[langpm] {len(candidates)} candidate names to check on npm "
          f"(of {len(rows)} lacking npm)", flush=True)

    added = checked = 0
    t0 = time.time()
    for i, r in enumerate(candidates):
        name = r["name"]
        if name in CORRECTIONS:
            continue
        if i > 0:
            time.sleep(rate)
        checked += 1
        if not npm_has_bin(session, name, proxies):
            continue
        try:
            existing = json.loads(r["install_instructions"] or "{}")
        except Exception:
            existing = {}
        if existing.get("npm"):
            continue
        existing["npm"] = f"npm install -g {name}"
        j = json.dumps(existing, ensure_ascii=False)
        if not dry_run:
            conn.execute(
                "UPDATE apps SET install_instructions = ? WHERE name = ? "
                "AND (json_extract(install_instructions,'$.npm') IS NULL)",
                (j, name),
            )
        added += 1
        if added % 20 == 0:
            conn.commit() if not dry_run else None
        if added % 25 == 0 or (i + 1) % 500 == 0:
            el = time.time() - t0
            eta = el / (i + 1) * (len(candidates) - i - 1)
            print(f"[langpm] {i+1}/{len(candidates)} checked — {added} npm added — ETA {eta:.0f}s", flush=True)

    if not dry_run:
        conn.commit()
    conn.close()
    print(f"\n[langpm] Done — {corrected} corrected, {added} npm installs added "
          f"(checked {checked}) in {time.time()-t0:.0f}s")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", default=str(Path.home() / ".local/share/cmdhub/cmdhub.db"))
    ap.add_argument("--rate", type=float, default=0.3, help="Seconds between npm requests")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--proxy", default="http://127.0.0.1:1080")
    args = ap.parse_args()
    if not Path(args.db).exists():
        print(f"[error] DB not found: {args.db}", file=sys.stderr)
        sys.exit(1)
    enrich(args.db, args.rate, args.dry_run, args.proxy)


if __name__ == "__main__":
    main()

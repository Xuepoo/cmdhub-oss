#!/usr/bin/env python3
"""Enrich cmdhub.db install_instructions from Repology API.

Queries Repology for each app name and fills in missing package manager entries
(dnf, apk, zypper, emerge, nix-env, scoop, choco, pkg, cargo, pip, etc.).

Only touches apps with brew OR apt already present (cross-platform tools).
Never overwrites existing non-null values.

Usage:
    uv run --with requests python3 scripts/enrich_repology.py \\
        [--db ~/.local/share/cmdhub/cmdhub.db] \\
        [--rate 1.0] \\
        [--dry-run]
"""

from __future__ import annotations

import argparse
import json
import sqlite3
import sys
import time
from pathlib import Path
from typing import Any

import requests

# ── Repo family → PM key ──────────────────────────────────────────────────────
# Each tuple: (repo_prefix_or_exact, pm_key, use_srcname)
# Order matters: more-specific prefixes first.
_REPO_MAP: list[tuple[str, str, bool]] = [
    # Debian/Ubuntu family → apt
    ("debian",      "apt",     False),
    ("ubuntu",      "apt",     False),
    ("kali",        "apt",     False),
    ("raspbian",    "apt",     False),
    ("linuxmint",   "apt",     False),
    ("parrot",      "apt",     False),
    ("pureos",      "apt",     False),
    ("devuan",      "apt",     False),
    ("trisquel",    "apt",     False),
    ("pardus",      "apt",     False),
    # Fedora/RHEL family → dnf
    ("fedora",      "dnf",     False),
    ("epel",        "dnf",     False),
    ("openeuler",   "dnf",     False),
    ("rosa",        "dnf",     False),
    ("centos",      "dnf",     False),
    # Alpine → apk
    ("alpine",      "apk",     False),
    # Gentoo → emerge (use srcname: "sys-apps/ripgrep")
    ("gentoo",      "emerge",  True),
    ("liguros",     "emerge",  True),
    # NixOS → nix-env
    ("nix_",        "nix-env", False),
    ("nixpkgs",     "nix-env", False),
    # OpenSUSE → zypper
    ("opensuse",    "zypper",  False),
    # FreeBSD/BSD → pkg
    ("freebsd",     "pkg",     False),
    ("openbsd",     "pkg",     False),
    ("pkgsrc",      "pkg",     False),
    # Windows → scoop, choco
    ("scoop",       "scoop",   False),
    ("chocolatey",  "choco",   False),
    ("winget",      "winget",  False),
    # macOS
    ("homebrew",    "brew",    False),
    ("macports",    "brew",    False),
    # Arch family → pacman
    ("arch",        "pacman",  False),
    ("artix",       "pacman",  False),
    ("manjaro",     "pacman",  False),
    ("parabola",    "pacman",  False),
    ("archpower",   "pacman",  False),
    ("aur",         "yay",     False),
    # Language PMs
    ("crates_io",   "cargo",   False),
    ("pypi",        "pip",     False),
]


def _repo_to_pm(repo: str) -> tuple[str, bool] | None:
    """Return (pm_key, use_srcname) for a Repology repo name, or None."""
    repo_lower = repo.lower()
    for prefix, pm_key, use_src in _REPO_MAP:
        if repo_lower == prefix or repo_lower.startswith(prefix + "_") or repo_lower.startswith(prefix):
            return pm_key, use_src
    return None


def _best_pkg_name(entry: dict, project_name: str, use_srcname: bool) -> str:
    """Extract the best package name for an install command from a Repology entry."""
    binname = entry.get("binname", "") or ""
    srcname = entry.get("srcname", "") or ""

    # Gentoo/emerge: always use full category/name from srcname
    if use_srcname and srcname:
        # Strip category prefix that looks like "sys-apps/ripgrep" → keep as-is for emerge
        return srcname

    # Prefer binname if it looks like a clean package name (no path separators)
    if binname and "/" not in binname and len(binname) < 60:
        return binname

    # Fallback to project name (most reliable for user-facing install commands)
    return project_name


def _build_install_cmd(pm: str, pkg: str) -> str:
    match pm:
        case "apt":     return f"apt install {pkg}"
        case "dnf":     return f"dnf install {pkg}"
        case "apk":     return f"apk add {pkg}"
        case "emerge":  return f"emerge {pkg}"
        case "nix-env": return f"nix-env -iA nixpkgs.{pkg}"
        case "zypper":  return f"zypper install {pkg}"
        case "pkg":     return f"pkg install {pkg}"
        case "scoop":   return f"scoop install {pkg}"
        case "choco":   return f"choco install {pkg}"
        case "winget":  return f"winget install {pkg}"
        case "brew":    return f"brew install {pkg}"
        case "pacman":  return f"pacman -S {pkg}"
        case "yay":     return f"yay -S {pkg}"
        case "paru":    return f"paru -S {pkg}"
        case "cargo":   return f"cargo install {pkg}"
        case "pip":     return f"pip install {pkg}"
        case _:         return f"{pm} install {pkg}"


def _name_score(pkg_name: str, project_name: str) -> int:
    """Score a candidate package name: higher = better match to project_name."""
    p = pkg_name.lower()
    q = project_name.lower()
    if p == q:
        return 100
    if p.startswith(q) and not p[len(q):].lstrip("-_"):
        return 80
    if q in p and not any(bad in p for bad in ["lib32", "lib64", "debug", "doc", "dev", "git"]):
        return 60
    if any(bad in p for bad in ["lib32", "lib64", "debug", "doc", "dev", "-git", "-nox"]):
        return 10
    return 30


def query_repology(session: requests.Session, project_name: str, proxies: dict) -> dict[str, str]:
    """Query Repology for a project and return {pm_key: install_cmd}."""
    url = f"https://repology.org/api/v1/projects/?search={project_name}"
    try:
        r = session.get(url, proxies=proxies, timeout=15)
        r.raise_for_status()
    except Exception as e:
        print(f"  [warn] Repology error for {project_name!r}: {e}", file=sys.stderr)
        return {}

    data: dict[str, list] = r.json()

    # Find exact project match (case-insensitive fallback)
    packages: list[dict] = data.get(project_name, [])
    if not packages:
        for key, pkgs in data.items():
            if key.lower() == project_name.lower():
                packages = pkgs
                break

    if not packages:
        return {}

    # Collect all candidates per PM, then pick the best-scored one
    pm_candidates: dict[str, list[tuple[int, str]]] = {}  # pm_key → [(score, cmd)]
    for entry in packages:
        repo = entry.get("repo", "")
        result = _repo_to_pm(repo)
        if result is None:
            continue
        pm_key, use_src = result
        pkg = _best_pkg_name(entry, project_name, use_src)
        score = _name_score(pkg, project_name)
        pm_candidates.setdefault(pm_key, []).append((score, _build_install_cmd(pm_key, pkg)))

    # Pick highest-scored candidate per PM
    pm_cmds: dict[str, str] = {}
    for pm_key, candidates in pm_candidates.items():
        best_score, best_cmd = max(candidates, key=lambda x: x[0])
        pm_cmds[pm_key] = best_cmd

    return pm_cmds


def enrich_db(db_path: str, rate: float, dry_run: bool, proxy: str) -> None:
    proxies = {"https": proxy, "http": proxy} if proxy else {}

    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row

    # Target: apps with brew OR apt already set (cross-platform tools)
    rows = conn.execute("""
        SELECT DISTINCT name, GROUP_CONCAT(app_id) as app_ids,
               MAX(install_instructions) as install_instructions
        FROM apps
        WHERE json_extract(install_instructions, '$.brew') IS NOT NULL
           OR json_extract(install_instructions, '$.apt') IS NOT NULL
        GROUP BY name
        ORDER BY name
    """).fetchall()

    total = len(rows)
    print(f"[repology] {total} distinct tool names to enrich", flush=True)

    session = requests.Session()
    session.headers["User-Agent"] = "cmdhub-enricher/0.1 (txx15132@gmail.com)"

    updated = 0
    skipped = 0
    t0 = time.time()

    for i, row in enumerate(rows):
        name = row["name"]
        existing_raw = row["install_instructions"] or "{}"
        try:
            existing: dict = json.loads(existing_raw)
        except Exception:
            existing = {}

        # Rate limit
        if i > 0:
            time.sleep(rate)

        pm_cmds = query_repology(session, name, proxies)
        if not pm_cmds:
            skipped += 1
            if (i + 1) % 50 == 0:
                elapsed = time.time() - t0
                print(f"[repology] {i+1}/{total} — {updated} updated, {skipped} not found — {elapsed:.0f}s", flush=True)
            continue

        # Merge: only add keys that don't already exist
        merged = dict(existing)
        new_keys = []
        for pm, cmd in pm_cmds.items():
            if pm not in merged or merged[pm] is None:
                merged[pm] = cmd
                new_keys.append(pm)

        if not new_keys:
            skipped += 1
            continue

        updated += 1
        new_json = json.dumps(merged, ensure_ascii=False)

        if not dry_run:
            # Update all apps with this name
            conn.execute(
                "UPDATE apps SET install_instructions = ? WHERE name = ?",
                (new_json, name),
            )
            if updated % 20 == 0:
                conn.commit()

        if (i + 1) % 50 == 0 or new_keys:
            elapsed = time.time() - t0
            eta = elapsed / (i + 1) * (total - i - 1)
            print(
                f"[repology] {i+1}/{total} {name!r:30} +{new_keys} — "
                f"{updated} updated — ETA {eta:.0f}s",
                flush=True,
            )

    if not dry_run:
        conn.commit()
    conn.close()

    elapsed = time.time() - t0
    print(f"\n[repology] Done in {elapsed:.0f}s — {updated} apps enriched, {skipped} skipped/not found")


def main() -> None:
    ap = argparse.ArgumentParser(description="Enrich cmdhub.db install_instructions via Repology")
    ap.add_argument("--db", default=str(Path.home() / ".local/share/cmdhub/cmdhub.db"))
    ap.add_argument("--rate", type=float, default=1.1, help="Seconds between requests (default 1.1)")
    ap.add_argument("--dry-run", action="store_true", help="Don't write to DB")
    ap.add_argument("--proxy", default="http://127.0.0.1:1080")
    args = ap.parse_args()

    if not Path(args.db).exists():
        print(f"[error] DB not found: {args.db}", file=sys.stderr)
        sys.exit(1)

    enrich_db(args.db, args.rate, args.dry_run, args.proxy)


if __name__ == "__main__":
    main()

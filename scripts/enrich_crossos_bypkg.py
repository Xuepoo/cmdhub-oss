#!/usr/bin/env python3
"""Cross-OS enrichment keyed on the PACKAGE name, not the binary name.

The first Repology pass looked up tools by app.name (the binary, e.g. "rg"), so tools
whose package name differs (rg→ripgrep, fd→fd-find) were missed and never got dnf/
zypper/etc. Here we extract the real package name from an existing pacman/apt/brew/
cargo value and look Repology up by THAT, filling missing distro managers. Only apps
where pkg-name != binary-name and that still lack some distro are processed.

    uv run --with requests python3 scripts/enrich_crossos_bypkg.py [--db ...] [--proxy ...]
"""
from __future__ import annotations

import argparse
import json
import sqlite3
import sys
import time
from pathlib import Path

import requests

sys.path.insert(0, str(Path(__file__).parent))
from enrich_repology import query_repology  # reuse exact-endpoint + score gate


def _pkg_from(val: str) -> str:
    v = (val or "").strip()
    return v.split()[-1] if " " in v else v


def enrich(db_path: str, proxy: str, rate: float) -> None:
    proxies = {"https": proxy, "http": proxy} if proxy else {}
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    rows = conn.execute("""
        SELECT app_id, name, install_instructions FROM apps
        WHERE install_instructions IS NOT NULL AND length(install_instructions) > 2
          AND (json_extract(install_instructions,'$.pacman') IS NOT NULL
               OR json_extract(install_instructions,'$.apt') IS NOT NULL
               OR json_extract(install_instructions,'$.brew') IS NOT NULL
               OR json_extract(install_instructions,'$.cargo') IS NOT NULL)
          AND (json_extract(install_instructions,'$.dnf') IS NULL
               OR json_extract(install_instructions,'$.zypper') IS NULL
               OR json_extract(install_instructions,'$.nix-env') IS NULL)
    """).fetchall()

    # Build app→pkgname, keep only where pkg != binary name (the missed set).
    targets: list[tuple[str, dict, str]] = []  # (app_id, install_dict, pkg)
    pkgset: set[str] = set()
    for r in rows:
        try:
            inst = json.loads(r["install_instructions"])
        except Exception:
            continue
        pkg = ""
        for k in ("pacman", "apt", "brew", "cargo"):
            if inst.get(k):
                pkg = _pkg_from(inst[k]); break
        if pkg and pkg.lower() != (r["name"] or "").lower():
            targets.append((r["app_id"], inst, pkg))
            pkgset.add(pkg)
    print(f"[bypkg] {len(targets)} apps with pkg!=binary missing distros; {len(pkgset)} distinct pkgs", flush=True)

    session = requests.Session()
    session.headers["User-Agent"] = "cmdhub-crossos/0.1 (txx15132@gmail.com)"

    # Query Repology once per distinct package name.
    pm_cache: dict[str, dict[str, str]] = {}
    for i, pkg in enumerate(sorted(pkgset)):
        if i:
            time.sleep(rate)
        pm_cache[pkg] = query_repology(session, pkg, proxies)
        if (i + 1) % 200 == 0:
            print(f"[bypkg] queried {i+1}/{len(pkgset)}", flush=True)

    updated = 0
    for app_id, inst, pkg in targets:
        found = pm_cache.get(pkg) or {}
        added = False
        for pm, cmd in found.items():
            if pm not in inst or inst[pm] is None:
                inst[pm] = cmd
                added = True
        if added:
            conn.execute("UPDATE apps SET install_instructions=? WHERE app_id=?",
                         (json.dumps(inst, ensure_ascii=False), app_id))
            updated += 1
            if updated % 50 == 0:
                conn.commit()
    conn.commit()
    # checkpoint WAL so a later copy (e.g. scp) sees the writes
    conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    conn.close()
    print(f"[bypkg] done — {updated} apps enriched via package name")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", default=str(Path.home() / ".local/share/cmdhub/cmdhub.db"))
    ap.add_argument("--proxy", default="http://127.0.0.1:1080")
    ap.add_argument("--rate", type=float, default=0.7)
    a = ap.parse_args()
    enrich(a.db, a.proxy, a.rate)


if __name__ == "__main__":
    main()

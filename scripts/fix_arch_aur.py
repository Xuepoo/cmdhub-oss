#!/usr/bin/env python3
"""Correct pacman vs AUR install methods in cmdhub.db.

The original Arch crawl labelled many packages as `pacman -S X` even when X only
exists in the AUR (e.g. `s`, `scc`), so the command fails on a stock Arch box.
This pass reclassifies each pacman entry:

  * package in an OFFICIAL repo (checked locally via `pacman -Sql`, no network)
        → keep `pacman -S X`
  * not official but present in the AUR (batch AUR RPC)
        → replace with `yay -S X` + `paru -S X`, drop the bogus `pacman`
  * in neither
        → drop the bogus `pacman` (other managers, if any, are kept)

Usage:
    uv run --with requests python3 scripts/fix_arch_aur.py [--db ...] [--dry-run]
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

import requests


def _official_set() -> set[str]:
    """All package names available in the local sync repos (core/extra/multilib...)."""
    try:
        out = subprocess.run(["pacman", "-Sql"], capture_output=True, text=True, timeout=30)
        return {l.strip() for l in out.stdout.splitlines() if l.strip()}
    except Exception as e:
        print(f"[warn] pacman -Sql failed ({e}); cannot verify official packages", file=sys.stderr)
        return set()


def _pkg_from_pacman(val: str) -> str:
    """Extract the package name from a stored pacman value (full cmd or bare name)."""
    v = val.strip()
    if v.lower().startswith("pacman"):
        return v.split()[-1]
    if v.lower().startswith("sudo pacman"):
        return v.split()[-1]
    return v


def _aur_exists_batch(session: requests.Session, names: list[str], proxies: dict) -> set[str]:
    """Return the subset of names that exist in the AUR (batched RPC v5 info)."""
    found: set[str] = set()
    CHUNK = 150
    for i in range(0, len(names), CHUNK):
        chunk = names[i:i + CHUNK]
        params = [("arg[]", n) for n in chunk]
        for attempt in range(3):
            try:
                r = session.get("https://aur.archlinux.org/rpc/v5/info", params=params,
                                proxies=proxies, timeout=20)
                if r.status_code == 429:
                    time.sleep(3 * (attempt + 1)); continue
                r.raise_for_status()
                for res in r.json().get("results", []):
                    nm = res.get("Name")
                    if nm:
                        found.add(nm)
                break
            except Exception as e:
                if attempt == 2:
                    print(f"[warn] AUR batch {i} failed: {e}", file=sys.stderr)
                else:
                    time.sleep(2)
        time.sleep(0.5)
        if (i // CHUNK) % 10 == 0:
            print(f"[aur] {min(i+CHUNK,len(names))}/{len(names)} checked, {len(found)} in AUR", flush=True)
    return found


def fix(db_path: str, dry_run: bool, proxy: str) -> None:
    import sqlite3
    proxies = {"https": proxy, "http": proxy} if proxy else {}

    official = _official_set()
    print(f"[arch] {len(official)} official packages from local sync db", flush=True)
    if not official:
        print("[error] no official set; aborting to avoid mislabelling everything", file=sys.stderr)
        return

    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    rows = conn.execute(
        "SELECT app_id, install_instructions FROM apps "
        "WHERE json_extract(install_instructions,'$.pacman') IS NOT NULL"
    ).fetchall()
    print(f"[arch] {len(rows)} apps carry a pacman entry", flush=True)

    # Collect distinct non-official pkg names → AUR check
    pkg_of: dict[str, str] = {}
    for r in rows:
        try:
            inst = json.loads(r["install_instructions"])
        except Exception:
            continue
        pacman_val = inst.get("pacman")
        if pacman_val:
            pkg_of[r["app_id"]] = _pkg_from_pacman(pacman_val)
    distinct_nonofficial = sorted({p for p in pkg_of.values() if p not in official})
    print(f"[arch] {len(distinct_nonofficial)} distinct non-official pacman names → AUR lookup", flush=True)

    session = requests.Session()
    session.headers["User-Agent"] = "cmdhub-archfix/0.1 (txx15132@gmail.com)"
    aur = _aur_exists_batch(session, distinct_nonofficial, proxies)
    print(f"[arch] {len(aur)} of those are in the AUR", flush=True)

    kept = to_aur = dropped = 0
    for r in rows:
        app_id = r["app_id"]
        try:
            inst = json.loads(r["install_instructions"])
        except Exception:
            continue
        pkg = pkg_of.get(app_id)
        if pkg is None:
            continue
        if pkg in official:
            inst["pacman"] = f"pacman -S {pkg}"  # normalize to full cmd
            kept += 1
            changed = inst.get("pacman") != json.loads(r["install_instructions"]).get("pacman")
        elif pkg in aur:
            inst.pop("pacman", None)
            inst["yay"] = f"yay -S {pkg}"
            inst["paru"] = f"paru -S {pkg}"
            to_aur += 1
            changed = True
        else:
            inst.pop("pacman", None)
            dropped += 1
            changed = True
        if changed and not dry_run:
            conn.execute("UPDATE apps SET install_instructions = ? WHERE app_id = ?",
                         (json.dumps(inst, ensure_ascii=False) if inst else None, app_id))

    if not dry_run:
        conn.commit()
    conn.close()
    print(f"\n[arch] done — kept official: {kept}, → AUR(yay/paru): {to_aur}, dropped bogus: {dropped}")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", default=str(Path.home() / ".local/share/cmdhub/cmdhub.db"))
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--proxy", default="http://127.0.0.1:1080")
    args = ap.parse_args()
    if not Path(args.db).exists():
        print(f"[error] DB not found: {args.db}", file=sys.stderr)
        sys.exit(1)
    fix(args.db, args.dry_run, args.proxy)


if __name__ == "__main__":
    main()

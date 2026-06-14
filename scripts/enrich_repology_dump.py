#!/usr/bin/env python3
"""Cross-OS install enrichment from the Repology DATABASE DUMP (no API, no rate limits).

Streams `zstd -dc repology-*.sql.zst`, parses the `packages` COPY block, and for every
project (effname) that matches one of our tools' package names, collects the package
name per repo → maps to a package manager → fills missing install methods (apt/dnf/
zypper/apk/nix-env/brew/scoop/choco/cargo/pip). Matching is by PACKAGE name (extracted
from existing install values), so binary≠package tools (rg→ripgrep, fd→fd-find) work.

    python3 enrich_repology_dump.py --dump /tmp/repology.sql.zst --db ~/.local/share/cmdhub/cmdhub.db
"""
from __future__ import annotations

import argparse
import json
import re
import sqlite3
import subprocess
import sys
from pathlib import Path

# Repology repo name (or its prefix) → (pm_key, use_srcname)
_REPO_MAP: list[tuple[str, str, bool]] = [
    ("debian", "apt", False), ("ubuntu", "apt", False), ("kali", "apt", False),
    ("linuxmint", "apt", False), ("raspbian", "apt", False),
    ("fedora", "dnf", False), ("epel", "dnf", False), ("centos", "dnf", False),
    ("alpine", "apk", False),
    ("gentoo", "emerge", True),
    ("nix", "nix-env", False),
    ("opensuse", "zypper", False),
    ("freebsd", "pkg", False), ("pkgsrc", "pkg", False),
    ("scoop", "scoop", False), ("chocolatey", "choco", False), ("winget", "winget", False),
    ("homebrew", "brew", False), ("macports", "brew", False),
    ("arch", "pacman", False), ("artix", "pacman", False), ("manjaro", "pacman", False),
    ("aur", "yay", False),
    ("crates_io", "cargo", False), ("pypi", "pip", False),
]


def _repo_to_pm(repo: str):
    r = repo.lower()
    for prefix, pm, use_src in _REPO_MAP:
        if r == prefix or r.startswith(prefix):
            return pm, use_src
    return None


def _build_cmd(pm: str, pkg: str) -> str:
    return {
        "apt": f"apt install {pkg}", "dnf": f"dnf install {pkg}", "apk": f"apk add {pkg}",
        "emerge": f"emerge {pkg}", "nix-env": f"nix-env -iA nixpkgs.{pkg}",
        "zypper": f"zypper install {pkg}", "pkg": f"pkg install {pkg}",
        "scoop": f"scoop install {pkg}", "choco": f"choco install {pkg}",
        "winget": f"winget install {pkg}", "brew": f"brew install {pkg}",
        "pacman": f"pacman -S {pkg}", "yay": f"yay -S {pkg}",
        "cargo": f"cargo install {pkg}", "pip": f"pip install {pkg}",
    }.get(pm, f"{pm} install {pkg}")


def _pkgkeys(inst: dict, name: str) -> set[str]:
    """All candidate lookup keys: the package name from every install value + the binary.
    Repology's effname may match any of them (e.g. fd's project is 'fd-find', matching the
    apt value, not the pacman 'fd')."""
    keys: set[str] = set()
    for k in ("pacman", "apt", "brew", "cargo", "dnf", "zypper", "apk", "yay", "pip", "npm"):
        v = inst.get(k)
        if v:
            keys.add((v.split()[-1] if " " in v else v).lower())
    if not keys:  # only fall back to the binary name when there's no package name at all
        keys.add(name.lower())
    return {k for k in keys if k}


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dump", required=True)
    ap.add_argument("--db", default=str(Path.home() / ".local/share/cmdhub/cmdhub.db"))
    a = ap.parse_args()

    conn = sqlite3.connect(a.db)
    conn.row_factory = sqlite3.Row
    rows = conn.execute("SELECT app_id, name, install_instructions FROM apps "
                        "WHERE install_instructions IS NOT NULL AND length(install_instructions) > 2").fetchall()
    key_to_apps: dict[str, list[tuple[str, dict]]] = {}
    for r in rows:
        try:
            inst = json.loads(r["install_instructions"])
        except Exception:
            continue
        for k in _pkgkeys(inst, r["name"]):
            key_to_apps.setdefault(k, []).append((r["app_id"], inst))
    targets = set(key_to_apps)
    print(f"[dump] {len(targets)} distinct package keys to look up", flush=True)

    # Stream the dump and parse the packages COPY block.
    proc = subprocess.Popen(["zstd", "-dc", a.dump], stdout=subprocess.PIPE, text=True, bufsize=1 << 20)
    found: dict[str, dict[str, str]] = {}  # effname → {pm: pkgname}
    cols: list[str] | None = None
    in_copy = False
    i_repo = i_eff = i_bin = i_src = -1
    seen_rows = 0
    assert proc.stdout
    for line in proc.stdout:
        if not in_copy:
            if line.startswith("COPY ") and ".packages " in line and "FROM stdin" in line:
                m = re.search(r"\(([^)]*)\)", line)
                cols = [c.strip().strip('"') for c in m.group(1).split(",")] if m else []
                def idx(n): return cols.index(n) if n in cols else -1
                i_repo, i_eff = idx("repo"), idx("effname")
                i_bin, i_src = idx("binname"), idx("srcname")
                in_copy = True
            continue
        if line.startswith("\\."):
            break
        seen_rows += 1
        f = line.rstrip("\n").split("\t")
        if i_eff < 0 or i_eff >= len(f):
            continue
        eff = f[i_eff].lower()
        if eff not in targets:
            continue
        pm_use = _repo_to_pm(f[i_repo]) if 0 <= i_repo < len(f) else None
        if not pm_use:
            continue
        pm, use_src = pm_use
        pkg = ""
        if use_src and 0 <= i_src < len(f) and f[i_src] not in ("\\N", ""):
            pkg = f[i_src]
        elif 0 <= i_bin < len(f) and f[i_bin] not in ("\\N", ""):
            pkg = f[i_bin]
        elif 0 <= i_src < len(f) and f[i_src] not in ("\\N", ""):
            pkg = f[i_src]
        if not pkg:
            continue
        found.setdefault(eff, {}).setdefault(pm, _build_cmd(pm, pkg))
        if seen_rows % 5_000_000 == 0:
            print(f"[dump] scanned {seen_rows//1_000_000}M rows, matched {len(found)} projects", flush=True)
    proc.stdout.close(); proc.wait()
    print(f"[dump] scan done: {seen_rows} pkg rows, matched {len(found)} of {len(targets)} keys", flush=True)

    updated = 0
    for key, pms in found.items():
        for app_id, inst in key_to_apps.get(key, []):
            added = False
            for pm, cmd in pms.items():
                if pm not in inst or inst[pm] is None:
                    inst[pm] = cmd; added = True
            if added:
                conn.execute("UPDATE apps SET install_instructions=? WHERE app_id=?",
                             (json.dumps(inst, ensure_ascii=False), app_id))
                updated += 1
    conn.commit()
    conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    conn.close()
    print(f"[dump] enriched {updated} apps from the Repology dump")


if __name__ == "__main__":
    main()

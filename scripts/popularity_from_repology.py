#!/usr/bin/env python3
"""Compute a data-driven popularity prior for every app from the Repology dump.

A project's popularity is approximated by HOW MANY distinct distro/ecosystem repos
package it: ubiquitous tools (git, coreutils, kubectl) appear in dozens of repos,
niche ones (delfast, prowler) in one or two. This cross-ecosystem repo-count is a
free, already-local signal — no API, no stars/downloads scraping needed — and it is
exactly the authority prior search ranking needs so relevant-but-obscure tools stop
outranking the canonical CLI for brand/concept words (azure->az, not prowler.azure).

Matching reuses the install-value package keys (so binary != package works: rg ->
ripgrep). The raw repo count is log-normalised to popularity in [0, 1] and written to
`apps.popularity` (column added if missing). Re-runnable.

    python3 popularity_from_repology.py --dump /tmp/repology.sql.zst --db ~/.local/share/cmdhub/cmdhub.db
"""
from __future__ import annotations

import argparse
import json
import math
import re
import sqlite3
import subprocess
from pathlib import Path

# Above this many packaging repos, a project is "as canonical as it gets" -> popularity 1.0.
# Set near the p98 of the matched repo-count distribution (curl=196, git=178, ripgrep=125,
# docker=101, azure-cli=50, kubectl=48, prowler=10): a low cap (e.g. 30) saturates every
# moderately-packaged tool to 1.0 and destroys the gradient brand-word ranking needs.
_REPO_CAP = 100


def _pkgkeys(inst: dict, name: str) -> set[str]:
    """Candidate lookup keys: package name from every install value + the binary name."""
    keys: set[str] = set()
    for k in ("pacman", "apt", "brew", "cargo", "dnf", "zypper", "apk", "yay", "pip", "npm"):
        v = inst.get(k)
        if v:
            keys.add((v.split()[-1] if " " in v else v).lower())
    keys.add(name.lower())
    return {k for k in keys if k}


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dump", required=True)
    ap.add_argument("--db", default=str(Path.home() / ".local/share/cmdhub/cmdhub.db"))
    ap.add_argument("--cap", type=int, default=_REPO_CAP)
    a = ap.parse_args()

    conn = sqlite3.connect(a.db)
    conn.row_factory = sqlite3.Row
    cols = {c[1] for c in conn.execute("PRAGMA table_info(apps)")}
    if "popularity" not in cols:
        conn.execute("ALTER TABLE apps ADD COLUMN popularity REAL DEFAULT 0.0")

    rows = conn.execute("SELECT app_id, name, install_instructions FROM apps").fetchall()
    key_to_apps: dict[str, list[str]] = {}
    for r in rows:
        inst = {}
        if r["install_instructions"]:
            try:
                inst = json.loads(r["install_instructions"])
            except Exception:
                inst = {}
        for k in _pkgkeys(inst if isinstance(inst, dict) else {}, r["name"]):
            key_to_apps.setdefault(k, []).append(r["app_id"])
    targets = set(key_to_apps)
    print(f"[pop] {len(rows)} apps, {len(targets)} distinct package keys", flush=True)

    # Stream the dump; count distinct repos per project (effname) for our targets only.
    proc = subprocess.Popen(["zstd", "-dc", a.dump], stdout=subprocess.PIPE, text=True, bufsize=1 << 20)
    repos_of: dict[str, set[str]] = {}
    cols_c: list[str] | None = None
    in_copy = False
    i_repo = i_eff = i_bin = i_src = i_bins = -1
    seen = 0
    assert proc.stdout

    def names_of(f: list[str]) -> set[str]:
        # Repology's effname is the project; a tool's actual package/binary often differs
        # (kubectl's binname is "kubectl" under project "kubernetes"). Match any of them.
        out: set[str] = set()
        for i in (i_eff, i_bin, i_src):
            if 0 <= i < len(f) and f[i] not in ("\\N", ""):
                out.add(f[i].lower())
        if 0 <= i_bins < len(f) and f[i_bins] not in ("\\N", ""):
            for b in f[i_bins].strip("{}").split(","):
                b = b.strip().strip('"').lower()
                if b:
                    out.add(b)
        return out

    for line in proc.stdout:
        if not in_copy:
            if line.startswith("COPY ") and ".packages " in line and "FROM stdin" in line:
                m = re.search(r"\(([^)]*)\)", line)
                cols_c = [c.strip().strip('"') for c in m.group(1).split(",")] if m else []
                def idx(n): return cols_c.index(n) if n in cols_c else -1
                i_repo, i_eff = idx("repo"), idx("effname")
                i_bin, i_src, i_bins = idx("binname"), idx("srcname"), idx("binnames")
                in_copy = True
            continue
        if line.startswith("\\."):
            break
        seen += 1
        f = line.rstrip("\n").split("\t")
        if i_repo < 0 or i_repo >= len(f):
            continue
        repo = f[i_repo]
        for nm in names_of(f):
            if nm in targets:
                repos_of.setdefault(nm, set()).add(repo)
        if seen % 5_000_000 == 0:
            print(f"[pop] scanned {seen//1_000_000}M rows, matched {len(repos_of)} projects", flush=True)
    proc.stdout.close(); proc.wait()
    print(f"[pop] scan done: {seen} rows, matched {len(repos_of)} of {len(targets)} keys", flush=True)

    # popularity per app = log-normalised max repo-count across its candidate keys.
    denom = math.log(1 + a.cap)
    app_pop: dict[str, float] = {}
    for key, repos in repos_of.items():
        pop = min(1.0, math.log(1 + len(repos)) / denom)
        for app_id in key_to_apps.get(key, []):
            if pop > app_pop.get(app_id, 0.0):
                app_pop[app_id] = pop

    conn.executemany("UPDATE apps SET popularity=? WHERE app_id=?",
                     [(p, aid) for aid, p in app_pop.items()])
    conn.commit()
    conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    n_nonzero = conn.execute("SELECT COUNT(*) FROM apps WHERE popularity > 0").fetchone()[0]
    top = conn.execute("SELECT name, round(popularity,3) p FROM apps ORDER BY popularity DESC LIMIT 8").fetchall()
    conn.close()
    print(f"[pop] set popularity for {len(app_pop)} apps ({n_nonzero} nonzero)")
    print("[pop] most popular:", ", ".join(f"{r['name']}={r['p']}" for r in top))


if __name__ == "__main__":
    main()

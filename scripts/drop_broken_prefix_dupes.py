#!/usr/bin/env python3
"""Drop structurally-broken duplicate apps (missing the binary prefix in their paths).

A tldr/arch import bug shifted some apps up one level: their first-level subcommands became
roots and the binary prefix was lost (org.archlinux.docker has cmd_paths `container`,
`container.start`, `exec`, `images` instead of `docker.container` …). These bare paths
pollute results and break path-match scoring. Almost all are low-value duplicates of a
correctly-structured canonical app (org.cmdhub.docker has the proper `docker.*` tree), so
the safe fix is to delete the broken copy *only when a canonical version of the same binary
exists* — no data loss, no re-embed/rebuild needed (FTS/vec orphans are filtered by the JOIN).

    python3 drop_broken_prefix_dupes.py --db ~/.local/share/cmdhub/cmdhub.db [--dry-run]
"""
from __future__ import annotations

import argparse
import os
import sqlite3
import sys
from collections import defaultdict


def binary_of(name: str) -> str:
    return name.strip().split()[0].lower() if name and name.strip() else ""


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", default=os.path.expanduser("~/.local/share/cmdhub/cmdhub.db"))
    ap.add_argument("--dry-run", action="store_true")
    a = ap.parse_args()
    if not os.path.exists(a.db):
        print(f"[error] no db at {a.db}", file=sys.stderr); sys.exit(1)

    c = sqlite3.connect(a.db)
    names = {aid: nm for aid, nm in c.execute("SELECT app_id, name FROM apps")}
    firstseg = defaultdict(set)
    for aid, cp in c.execute("SELECT app_id, cmd_path FROM arguments"):
        firstseg[aid].add(cp.split(".")[0].lower())

    # Per app, the set of cmd_path segments; per binary, the subcommands a canonical
    # (properly-rooted) twin exposes under "<binary>." — i.e. its real second segments.
    paths_of = defaultdict(list)
    for aid, cp in c.execute("SELECT app_id, cmd_path FROM arguments"):
        paths_of[aid].append(cp)
    canonical_subcmds = defaultdict(set)  # binary -> {subcommand names a good twin has}
    for aid, nm in names.items():
        b = binary_of(nm)
        if b and b in firstseg[aid]:
            for cp in paths_of[aid]:
                parts = cp.split(".")
                if parts[0].lower() == b and len(parts) > 1:
                    canonical_subcmds[b].add(parts[1].lower())

    # "Broken import" signature: the app exposes MULTIPLE distinct first segments (its real
    # subcommands got promoted to roots, e.g. docker -> container/exec/images/ps) yet none
    # is the binary. A legit subcommand-plugin (cargo binstall -> binstall.*, docker compose
    # -> docker_compose.*) has exactly ONE coherent first segment, so it is spared.
    drop = []
    for aid, nm in names.items():
        b = binary_of(nm)
        if not b or len(firstseg[aid]) < 2 or b in firstseg[aid]:
            continue
        twin = canonical_subcmds.get(b)
        # Drop only when EVERY one of the broken app's roots is already a real subcommand of
        # the canonical twin — i.e. the broken copy is fully redundant (docker's bare
        # container/exec/images all live under org.cmdhub.docker). A legit plugin whose
        # subcommand isn't in the twin (cargo "auditable", git "cola") is therefore kept.
        if twin and firstseg[aid] <= twin:
            drop.append((aid, b))

    print(f"[fix] broken-prefix apps with a canonical twin to drop: {len(drop)}")
    for aid, b in drop[:25]:
        print(f"   drop {aid}  (binary={b})")

    if a.dry_run:
        print("[fix] dry-run, no changes"); c.close(); return
    if drop:
        ids = [aid for aid, _ in drop]
        qs = ",".join("?" * len(ids))
        c.execute(f"DELETE FROM arguments WHERE app_id IN ({qs})", ids)
        c.execute(f"DELETE FROM apps WHERE app_id IN ({qs})", ids)
        # Keep standalone FTS5 / vector tables in sync (orphans skew bm25 and waste candidates).
        for tbl in ("apps_fts", "commands_vec"):
            try:
                c.execute(f"DELETE FROM {tbl} WHERE cmd_path NOT IN (SELECT cmd_path FROM arguments)")
            except Exception:
                pass
        c.commit()
        c.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        c.commit()
    apps = c.execute("SELECT COUNT(*) FROM apps").fetchone()[0]
    c.close()
    print(f"[fix] dropped {len(drop)} apps. remaining: {apps} apps")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Remove library noise and malformed paths from the command index.

The mass AUR ingest pulled in thousands of *library* packages (python-*, lib*,
language bindings, -sdk/-dev/-doc) that are not CLI tools and pollute search
(e.g. python-azure-mgmt-* outranking `az` for "azure"). It also recorded a few
malformed self-recursive command paths (az.az.az.*). This script deletes both
from a copy of the DB so the rebuilt FTS/vector index is clean. Re-runnable.

    python3 clean_index.py --db /tmp/work.db [--dry-run]
"""
from __future__ import annotations

import argparse
import os
import re
import sqlite3
import sys

# Packages whose name/app_id matches a library pattern but are genuinely CLI tools.
ALLOWLIST = {"libtree", "python-llm", "libcaca", "libxml2", "libreoffice"}

# Language libraries / bindings / dev artifacts — never an interactive CLI, removed from
# ANY source. Matched against both the display name and the package name in the app_id
# (the display name is often cleaned, e.g. app_id org.archlinux.python-kubernetes shows as
# "kubernetes" but is the Python client library, not kubectl).
LANG_LIB = re.compile(
    r"(^python-|^python2-|^perl-|^ruby-|^haskell-|^ghc-|^rust-|^nodejs-|^golang-|^php-"
    r"|-sdk$|-headers$|-dev$|-docs?$|-debug$|-bindings$|-typelib$|-stubs$)",
    re.IGNORECASE,
)
# Broader lib*/-git patterns — only safe to strip from the AUR long tail, where they are
# overwhelmingly mass-ingested noise (official repos have real apps like libreoffice).
AUR_LIB = re.compile(r"(^lib|-git$)", re.IGNORECASE)

# Malformed self-recursive command paths from a probe bug: the binary name reappears as a
# deeper path segment (az.az.az.find, az.find.az.patterns). No real subcommand is named "az".
MALFORMED = ["az.az.%", "%.az.az.%", "az.%.az", "az.%.az.%"]


def _pkg_from_app_id(app_id: str) -> str:
    """The package-name component of an app_id, e.g. org.archlinux.python-foo -> python-foo."""
    return app_id.rsplit(".", 1)[-1]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", required=True)
    ap.add_argument("--dry-run", action="store_true")
    a = ap.parse_args()
    if not os.path.exists(a.db):
        print(f"[error] no db at {a.db}", file=sys.stderr)
        sys.exit(1)

    c = sqlite3.connect(a.db)
    cur = c.cursor()

    lib_ids = []
    for app_id, name in cur.execute("SELECT app_id, name FROM apps").fetchall():
        pkg = _pkg_from_app_id(app_id)
        if name in ALLOWLIST or pkg in ALLOWLIST:
            continue
        is_aur = app_id.startswith("org.archlinux.aur.")
        hit = LANG_LIB.search(name) or LANG_LIB.search(pkg)
        if not hit and is_aur:
            hit = AUR_LIB.search(name) or AUR_LIB.search(pkg)
        if hit:
            lib_ids.append(app_id)

    mal_clause = " OR ".join(f"cmd_path LIKE '{p}'" for p in MALFORMED)
    mal_paths = [r[0] for r in cur.execute(
        f"SELECT cmd_path FROM arguments WHERE {mal_clause}").fetchall()]

    print(f"[clean] library-noise apps to remove : {len(lib_ids)}")
    print(f"[clean] malformed paths to remove    : {len(mal_paths)}")
    for p in mal_paths[:10]:
        print(f"          {p}")

    if a.dry_run:
        print("[clean] dry-run, no changes written")
        c.close()
        return

    if lib_ids:
        qs = ",".join("?" * len(lib_ids))
        cur.execute(f"DELETE FROM arguments WHERE app_id IN ({qs})", lib_ids)
        cur.execute(f"DELETE FROM apps WHERE app_id IN ({qs})", lib_ids)
    if mal_paths:
        qs = ",".join("?" * len(mal_paths))
        cur.execute(f"DELETE FROM arguments WHERE cmd_path IN ({qs})", mal_paths)
    c.commit()

    apps = cur.execute("SELECT COUNT(*) FROM apps").fetchone()[0]
    cmds = cur.execute("SELECT COUNT(*) FROM arguments").fetchone()[0]
    cur.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    c.commit()
    c.close()
    print(f"[clean] done. remaining: {apps} apps / {cmds} commands")


if __name__ == "__main__":
    main()

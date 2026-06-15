#!/usr/bin/env python3
"""Select the probe batch: beta-feedback-reported tools UNION top-N popularity
inferred tools. Prints app_ids to stdout; writes a packages.toml batch_probe.py
can consume.

Usage:
  python3 select_probe_targets.py --offline-db <cmdhub.db> --top 50 \
    [--feedback-tsv feedback.tsv] --out batch_packages.toml
"""
import argparse
import pathlib
import sqlite3
import sys


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--offline-db", required=True)
    ap.add_argument("--top", type=int, default=50)
    ap.add_argument("--feedback-tsv",
                    help="TSV of app_id<TAB>cmd_path from prod feedback (optional)")
    ap.add_argument("--out", default="batch_packages.toml")
    a = ap.parse_args()

    con = sqlite3.connect(a.offline_db)

    # Top-N popularity apps that have no probe-verified rows yet.
    rows = con.execute(
        """
        SELECT a.app_id, a.name FROM apps a
        WHERE a.app_id NOT IN (
            SELECT DISTINCT app_id FROM arguments WHERE provenance = 'probe'
        )
        ORDER BY a.popularity DESC
        LIMIT ?
        """,
        (a.top,),
    ).fetchall()
    targets = {app_id: name for app_id, name in rows}

    # Union in any tool a beta user reported via the feedback channel.
    if a.feedback_tsv and pathlib.Path(a.feedback_tsv).exists():
        for line in open(a.feedback_tsv):
            parts = line.strip().split("\t")
            if parts and parts[0]:
                app_id = parts[0]
                r = con.execute(
                    "SELECT name FROM apps WHERE app_id = ?", (app_id,)
                ).fetchone()
                targets[app_id] = r[0] if r else app_id.split(".")[-1]

    # batch_probe.py consumes [[packages]] stanzas; binary = the tool's leaf name.
    with open(a.out, "w") as f:
        f.write("# generated probe batch (feedback ∪ top popularity)\n")
        for app_id, name in sorted(targets.items()):
            binary = name.split()[0] if name else app_id.split(".")[-1]
            f.write(f'[[packages]]\nbinary = "{binary}"\napp_id = "{app_id}"\n\n')

    print(f"selected {len(targets)} tools → {a.out}", file=sys.stderr)
    for app_id in sorted(targets):
        print(app_id)


if __name__ == "__main__":
    main()

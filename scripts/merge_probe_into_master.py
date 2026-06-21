#!/usr/bin/env python3
"""Merge cmdh-extractor probe rows for a single tool into the master cmdhub.db.

Remaps the extractor's org.local.<tool> app_id onto the tool's EXISTING app in the
master (preserving its popularity + install_instructions), replaces the tool's
prior arguments (the lone root row) with the freshly-probed subtree, and tags
provenance='probe'. build_db re-embeds from `arguments`, so vectors are not copied.

    uv run --with sqlite-vec python3 merge_probe_into_master.py \
        --master tmp/rebuild-v4/cmdhub.db \
        --probe-db /tmp/cmdh-probe-wr/data/cmdhub/cmdhub.db --tool wrangler \
        --app-id org.archlinux.wrangler
"""
import argparse, sqlite3, sys

ARG_COLS = ["cmd_path", "app_id", "node_name", "node_type", "description",
            "risk_level", "example_template", "topics"]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--master", required=True)
    ap.add_argument("--probe-db", required=True)
    ap.add_argument("--tool", required=True, help="binary name, e.g. wrangler")
    ap.add_argument("--app-id", required=True, help="existing master app_id to remap onto")
    ap.add_argument("--topics", default=None,
                    help="brand/topic words to set on every merged row (e.g. 'cloudflare workers') "
                         "so brand-named queries reach these subcommands via the embedding")
    a = ap.parse_args()

    # The probe DB may not have a topics column (extractor doesn't set it); read what
    # exists and default topics to NULL, then override below.
    pc = sqlite3.connect(a.probe_db)
    probe_cols = [r[1] for r in pc.execute("PRAGMA table_info(arguments)").fetchall()]
    sel_cols = [c for c in ARG_COLS if c in probe_cols]
    rows = pc.execute(
        f"SELECT {','.join(sel_cols)} FROM arguments "
        "WHERE cmd_path = ? OR cmd_path LIKE ?||'.%'",
        (a.tool, a.tool),
    ).fetchall()
    rows = [dict(zip(sel_cols, r)) for r in rows]
    pc.close()
    if not rows:
        sys.exit(f"[merge] no probe rows for {a.tool} in {a.probe_db}")

    mc = sqlite3.connect(a.master)
    # The target app must already exist (we preserve its popularity/install).
    app = mc.execute("SELECT app_id, name FROM apps WHERE app_id = ?", (a.app_id,)).fetchone()
    if not app:
        sys.exit(f"[merge] target app_id {a.app_id} not found in master")

    before = mc.execute(
        "SELECT count(*) FROM arguments WHERE cmd_path = ? OR cmd_path LIKE ?||'.%'",
        (a.tool, a.tool),
    ).fetchone()[0]

    # Drop the tool's prior rows (the lone inferred/probed root), then insert the subtree.
    mc.execute("DELETE FROM arguments WHERE cmd_path = ? OR cmd_path LIKE ?||'.%'", (a.tool, a.tool))
    inserted = 0
    for d in rows:
        d["app_id"] = a.app_id                       # remap org.local.* -> existing app
        topics = a.topics if a.topics else d.get("topics")
        mc.execute(
            "INSERT OR REPLACE INTO arguments "
            "(cmd_path, app_id, node_name, node_type, description, risk_level, example_template, topics, provenance) "
            "VALUES (?,?,?,?,?,?,?,?, 'probe')",
            (d["cmd_path"], d["app_id"], d["node_name"], d["node_type"],
             d["description"], d.get("risk_level"), d.get("example_template"), topics),
        )
        inserted += 1
    mc.commit()
    after = mc.execute(
        "SELECT count(*) FROM arguments WHERE cmd_path = ? OR cmd_path LIKE ?||'.%'",
        (a.tool, a.tool),
    ).fetchone()[0]
    mc.close()
    print(f"[merge] {a.tool}: {before} -> {after} rows (inserted {inserted}, app_id={a.app_id})")


if __name__ == "__main__":
    main()

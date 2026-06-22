#!/usr/bin/env python3
"""Diff two cmdhub.db snapshots into an IncrementalSyncPayload (client-compatible),
compress, and sign. See docs/superpowers/specs/2026-06-21-incremental-update-pipeline-design.md

    uv run --with sqlite-vec --with zstandard --with cryptography python3 gen_delta.py \\
        --prev OLD.db --new NEW.db --version 2026.06.22 \\
        --prev-sync-time 1781000000 --new-sync-time 1781600000 --out-dir /tmp/cmdhub_release
"""
from __future__ import annotations
import argparse, json, os, sqlite3, struct, sys


def _open(p):
    c = sqlite3.connect(p)
    c.enable_load_extension(True)
    import sqlite_vec
    sqlite_vec.load(c)
    return c


# Columns must cover every required field of cmdhub_shared::DbApp (aci.rs), or the
# Rust client's serde deserialization of IncrementalSyncPayload fails. popularity is
# a non-Option f64 -> it MUST be present (a missing-field error otherwise).
_APP_COLS = ("app_id", "name", "os_aliases", "install_instructions", "popularity")


def _apps(c):
    return {r[0]: dict(zip(_APP_COLS, r))
            for r in c.execute(f"SELECT {','.join(_APP_COLS)} FROM apps")}


_ARG_COLS = ("cmd_path,app_id,node_name,node_type,description,risk_level,"
             "example_template,docker_image,script_url,source_url")


def _args(c):
    return {r[0]: dict(zip(_ARG_COLS.split(","), r))
            for r in c.execute(f"SELECT {_ARG_COLS} FROM arguments")}


def _vecs(c):
    return {cp: struct.unpack(f"{len(b) // 4}f", b)
            for cp, b in c.execute("SELECT cmd_path, embedding FROM commands_vec")}


def _by_app(args: dict) -> dict[str, dict]:
    """Group {cmd_path: arg_row} by app_id."""
    out: dict[str, dict] = {}
    for cp, g in args.items():
        out.setdefault(g["app_id"], {})[cp] = g
    return out


def diff(prev_db: str, new_db: str) -> dict:
    """Produce the client's IncrementalSyncPayload between two cmdhub.db snapshots.

    APP-SCOPED, because the client wipes ALL of an app's commands when that app
    appears in `payload.apps` and re-inserts only the commands present in
    `payload.arguments`. So an app is "dirty" if its row changed OR any command
    under it was added/changed/removed; for every dirty app we emit its row plus
    ALL its current commands + float32[384] vectors. This is the only way to
    propagate within-app command deletions correctly.

    deleted_apps: apps in prev but not new (client wipes them entirely)."""
    pc, nc = _open(prev_db), _open(new_db)
    pa, na = _apps(pc), _apps(nc)
    pg, ng = _args(pc), _args(nc)
    pv, nv = _vecs(pc), _vecs(nc)
    pc.close()
    nc.close()

    deleted_apps = [aid for aid in pa if aid not in na]

    p_by_app, n_by_app = _by_app(pg), _by_app(ng)

    def app_is_dirty(aid: str) -> bool:
        if na[aid] != pa.get(aid):           # app row changed or app is new
            return True
        if n_by_app.get(aid, {}) != p_by_app.get(aid, {}):  # any command add/change/remove
            return True
        # vector-only change (same arg row, different embedding) — rare but possible
        for cp in n_by_app.get(aid, {}):
            if nv.get(cp) != pv.get(cp):
                return True
        return False

    dirty = [aid for aid in na if app_is_dirty(aid)]
    dirty_set = set(dirty)

    # Emit the full DbApp shape (cmdhub_shared::aci.rs) — popularity is required.
    apps = [{"app_id": na[a]["app_id"], "name": na[a]["name"],
             "os_aliases": na[a]["os_aliases"],
             "install_instructions": na[a]["install_instructions"],
             "popularity": na[a]["popularity"]}
            for a in dirty]
    arguments = [g for cp, g in ng.items() if g["app_id"] in dirty_set]
    command_vecs = [{"cmd_path": cp, "embedding": list(nv[cp])}
                    for cp, g in ng.items()
                    if g["app_id"] in dirty_set and cp in nv]
    return {"deleted_apps": deleted_apps, "apps": apps,
            "arguments": arguments, "command_vecs": command_vecs}


def main() -> None:
    import importlib.util
    ap = argparse.ArgumentParser(description="Generate a signed incremental delta between two cmdhub.db")
    ap.add_argument("--prev", required=True, help="previous (already-published) cmdhub.db")
    ap.add_argument("--new", required=True, help="new cmdhub.db (post golden-gate)")
    ap.add_argument("--version", required=True, help="new release version, e.g. 2026.06.22")
    ap.add_argument("--prev-sync-time", type=int, required=True,
                    help="last_sync_time of the prev release (the delta's base)")
    ap.add_argument("--new-sync-time", type=int, required=True,
                    help="last_sync_time of the new release (must match the full manifest)")
    ap.add_argument("--priv", default=os.path.expanduser("~/.config/cmdhub/keys/ed25519_private.bin"))
    ap.add_argument("--cdn", default="https://cdn.cmdhub.org")
    ap.add_argument("--out-dir", default="/tmp/cmdhub_release")
    a = ap.parse_args()

    payload = diff(a.prev, a.new)
    raw = json.dumps(payload, ensure_ascii=False).encode()
    import zstandard
    zst = zstandard.ZstdCompressor(level=19).compress(raw)
    os.makedirs(a.out_dir, exist_ok=True)

    s_spec = importlib.util.spec_from_file_location(
        "sd", os.path.join(os.path.dirname(os.path.abspath(__file__)), "sign_db.py"))
    sd = importlib.util.module_from_spec(s_spec)
    s_spec.loader.exec_module(sd)

    import hashlib
    sha = hashlib.sha256(zst).hexdigest()
    key = f"db/delta-{sha[:16]}.json.zst"
    sig_key = f"db/delta-{sha[:16]}.json.sig"
    out_zst = os.path.join(a.out_dir, os.path.basename(key))
    open(out_zst, "wb").write(zst)
    _sha, sig = sd.sign_file(out_zst, a.priv)
    open(os.path.join(a.out_dir, os.path.basename(sig_key)), "wb").write(sig)

    entry = {"version": a.version, "sync_time": a.new_sync_time,
             "prev_sync_time": a.prev_sync_time,
             "delta": {"url": f"{a.cdn}/{key}", "sig_url": f"{a.cdn}/{sig_key}", "sha256": sha},
             "counts": {k: len(v) for k, v in payload.items()}}
    json.dump(entry, open(os.path.join(a.out_dir, "delta-entry.json"), "w"), indent=2)
    print(f"[delta] {entry['counts']} -> {out_zst} ({len(zst) / 1e6:.2f} MB)")
    print(f"[delta] entry -> {a.out_dir}/delta-entry.json")


if __name__ == "__main__":
    main()

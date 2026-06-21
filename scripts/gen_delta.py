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


def _apps(c):
    return {r[0]: r for r in c.execute(
        "SELECT app_id, name, install_instructions FROM apps")}


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

    apps = [{"app_id": na[a][0], "name": na[a][1], "install_instructions": na[a][2]}
            for a in dirty]
    arguments = [g for cp, g in ng.items() if g["app_id"] in dirty_set]
    command_vecs = [{"cmd_path": cp, "embedding": list(nv[cp])}
                    for cp, g in ng.items()
                    if g["app_id"] in dirty_set and cp in nv]
    return {"deleted_apps": deleted_apps, "apps": apps,
            "arguments": arguments, "command_vecs": command_vecs}

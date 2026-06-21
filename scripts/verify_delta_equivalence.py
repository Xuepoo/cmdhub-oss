#!/usr/bin/env python3
"""Prove incremental == full: apply a gen_delta payload to a copy of the previous
cmdhub.db exactly as the Rust client (updater.rs) does, then assert the result's
apps / arguments / commands_vec rows equal a full rebuild of the new DB.

    uv run --with sqlite-vec python3 verify_delta_equivalence.py --prev OLD.db --new NEW.db

Exit 0 = equivalent. The Rust client mirrors apply_payload(); the byte-packing is
covered by the updater.rs unit test (float32[384] little-endian = 1536 bytes)."""
from __future__ import annotations
import argparse, importlib.util, os, shutil, sqlite3, struct, sys, tempfile


def _load_gd():
    spec = importlib.util.spec_from_file_location(
        "gd", os.path.join(os.path.dirname(os.path.abspath(__file__)), "gen_delta.py"))
    gd = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(gd)
    return gd


def _open(p):
    c = sqlite3.connect(p)
    c.enable_load_extension(True)
    import sqlite_vec
    sqlite_vec.load(c)
    return c


def apply_payload(db_path: str, payload: dict) -> None:
    """Mirror updater.rs apply: app-scoped wipe+reinsert + float32[384] vec apply."""
    con = _open(db_path)

    def delete_app_commands(app_id: str) -> None:
        for (cp,) in con.execute("SELECT cmd_path FROM arguments WHERE app_id=?", (app_id,)).fetchall():
            con.execute("DELETE FROM commands_vec WHERE cmd_path=?", (cp,))
        con.execute("DELETE FROM arguments WHERE app_id=?", (app_id,))

    for app_id in payload["deleted_apps"]:
        delete_app_commands(app_id)
        con.execute("DELETE FROM apps WHERE app_id=?", (app_id,))

    for app in payload["apps"]:
        delete_app_commands(app["app_id"])
        con.execute(
            "INSERT OR REPLACE INTO apps (app_id, name, install_instructions) VALUES (?,?,?)",
            (app["app_id"], app["name"], app["install_instructions"]),
        )

    for g in payload["arguments"]:
        con.execute(
            "INSERT OR REPLACE INTO arguments "
            "(cmd_path,app_id,node_name,node_type,description,risk_level,"
            " example_template,docker_image,script_url,source_url) VALUES (?,?,?,?,?,?,?,?,?,?)",
            (g["cmd_path"], g["app_id"], g["node_name"], g["node_type"], g["description"],
             g["risk_level"], g["example_template"], g["docker_image"], g["script_url"], g["source_url"]),
        )

    for vec in payload["command_vecs"]:
        emb = vec["embedding"]
        if len(emb) != 384:
            continue
        vec_bytes = struct.pack(f"<{len(emb)}f", *emb)  # little-endian float32, matches build_db
        con.execute("DELETE FROM commands_vec WHERE cmd_path=?", (vec["cmd_path"],))
        con.execute("INSERT INTO commands_vec (cmd_path, embedding) VALUES (?,?)",
                    (vec["cmd_path"], vec_bytes))

    con.commit()
    con.close()


def _snapshot(db_path: str) -> dict:
    con = _open(db_path)
    apps = sorted(con.execute("SELECT app_id, name, install_instructions FROM apps"))
    cols = ("cmd_path,app_id,node_name,node_type,description,risk_level,"
            "example_template,docker_image,script_url,source_url")
    args = sorted(con.execute(f"SELECT {cols} FROM arguments"))
    vecs = sorted(con.execute("SELECT cmd_path, embedding FROM commands_vec"))
    con.close()
    return {"apps": apps, "arguments": args, "commands_vec": vecs}


def verify(prev_db: str, new_db: str) -> list[str]:
    """Return a list of mismatch descriptions (empty = equivalent)."""
    gd = _load_gd()
    payload = gd.diff(prev_db, new_db)
    tmp = tempfile.mkdtemp(prefix="delta_eq_")
    applied = os.path.join(tmp, "applied.db")
    shutil.copyfile(prev_db, applied)
    try:
        apply_payload(applied, payload)
        got, want = _snapshot(applied), _snapshot(new_db)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

    problems: list[str] = []
    for table in ("apps", "arguments", "commands_vec"):
        if got[table] != want[table]:
            g, w = set(map(repr, got[table])), set(map(repr, want[table]))
            extra = list(g - w)[:3]
            missing = list(w - g)[:3]
            problems.append(f"{table}: {len(got[table])} applied vs {len(want[table])} full; "
                            f"extra={extra} missing={missing}")
    return problems


def main() -> None:
    ap = argparse.ArgumentParser(description="Verify incremental delta == full rebuild")
    ap.add_argument("--prev", required=True)
    ap.add_argument("--new", required=True)
    a = ap.parse_args()
    problems = verify(a.prev, a.new)
    if problems:
        print("[verify] MISMATCH (incremental != full):", file=sys.stderr)
        for p in problems:
            print("  -", p, file=sys.stderr)
        sys.exit(1)
    print("[verify] OK: incremental == full (apps/arguments/commands_vec all match)")


if __name__ == "__main__":
    main()

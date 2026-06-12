"""Pure, deterministic transforms from the build export into SEO R2 artifacts.

No I/O here — every function takes plain data and returns plain data so it can be
golden-tested. The export shape is {apps:[...], arguments:[...]} (see export_sqlite.py):
each app has one root argument (node_type == 'root', cmd_path without a dot) plus subs.
"""
from __future__ import annotations


def _topics(raw: str | None) -> list[str]:
    return (raw or "").split()


def _root_node(args: list[dict]) -> dict | None:
    for a in args:
        if a.get("node_type") == "root" or "." not in a.get("cmd_path", "."):
            return a
    return args[0] if args else None


def build_shard(app: dict, args: list[dict], intents: list[str], generated_at: str) -> dict:
    """One app's shard: app meta + root-derived description/topics + the command tree."""
    root = _root_node(args) or {}
    tree = [
        {
            "cmd_path": a["cmd_path"],
            "node_name": a["node_name"],
            "node_type": a["node_type"],
            "description": a.get("description") or "",
            "risk_level": a.get("risk_level") or "safe",
            "example_template": a.get("example_template"),
        }
        for a in sorted(args, key=lambda r: r["cmd_path"])
    ]
    return {
        "app_id": app["app_id"],
        "name": app["name"],
        "description": root.get("description") or "",
        "topics": _topics(root.get("topics")),
        "popularity": app.get("popularity", 0.0),
        "install": {
            "os_aliases": app.get("os_aliases"),
            "instructions": app.get("install_instructions"),
        },
        "source_url": root.get("source_url"),
        "intents": list(intents),
        "tree": tree,
        "generated_at": generated_at,
    }

def build_index(apps: list[dict]) -> list[dict]:
    """Compact index for sitemaps + /commands aggregation, popularity desc, stable tiebreak."""
    rows = [
        {"app_id": a["app_id"], "name": a["name"], "popularity": a.get("popularity", 0.0)}
        for a in apps
    ]
    rows.sort(key=lambda r: (-r["popularity"], r["app_id"]))
    return rows


def build_manifest(build_id: str, count: int, generated_at: str) -> dict:
    return {"build_id": build_id, "count": count, "generated_at": generated_at}
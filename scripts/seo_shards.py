"""Pure, deterministic transforms from the build export into SEO R2 artifacts.

No I/O here — every function takes plain data and returns plain data so it can be
golden-tested. The export shape is {apps:[...], arguments:[...]} (see export_sqlite.py):
each app has one root argument (node_type == 'root', cmd_path without a dot) plus subs.
"""
from __future__ import annotations


import gzip
from xml.sax.saxutils import escape


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


def _url_entry(app_id: str, popularity: float, base_url: str, lastmod: str) -> str:
    loc = escape(f"{base_url}/c/{app_id}")
    # popularity (0..1) -> sitemap priority (0.0..1.0), one decimal, deterministic
    priority = f"{min(max(popularity, 0.0), 1.0):.1f}"
    return (
        f"  <url><loc>{loc}</loc>"
        f"<lastmod>{lastmod}</lastmod>"
        f"<priority>{priority}</priority></url>"
    )


def build_sitemaps(index: list[dict], base_url: str, lastmod: str, per_file: int = 50000) -> dict:
    """Return {filename: bytes|str}. Parts are gzipped bytes; the index is a str."""
    out: dict = {}
    parts = [index[i : i + per_file] for i in range(0, len(index), per_file)] or [[]]
    for n, chunk in enumerate(parts, start=1):
        body = "\n".join(_url_entry(e["app_id"], e["popularity"], base_url, lastmod) for e in chunk)
        xml = (
            '<?xml version="1.0" encoding="UTF-8"?>\n'
            '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n'
            f"{body}\n</urlset>\n"
        )
        out[f"sitemap-{n}.xml.gz"] = gzip.compress(xml.encode("utf-8"), mtime=0)
    refs = "\n".join(
        f"  <sitemap><loc>{escape(base_url)}/sitemaps/sitemap-{n}.xml.gz</loc>"
        f"<lastmod>{lastmod}</lastmod></sitemap>"
        for n in range(1, len(parts) + 1)
    )
    out["sitemap-index.xml"] = (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n'
        f"{refs}\n</sitemapindex>\n"
    )
    return out


def build_robots(base_url: str) -> str:
    return (
        "User-agent: *\n"
        "Allow: /c/\n"
        "Allow: /commands/\n"
        f"Sitemap: {base_url}/sitemaps/sitemap-index.xml\n"
    )
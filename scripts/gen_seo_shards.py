#!/usr/bin/env python3
"""Generate SEO R2 artifacts (shards, index, sitemaps, manifest) from cmdhub_export.json.

Pure transforms live in seo_shards.py; this file is the I/O shell.

    python3 gen_seo_shards.py --export cmdhub_export.json --intents head_intents.jsonl \\
        --out ./seo_out --base-url https://cmdhub.org [--build-id 20260612]
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
from pathlib import Path

import seo_shards


def _load_intents(path: Path | None) -> dict[str, list[str]]:
    if not path or not path.exists():
        return {}
    out: dict[str, list[str]] = {}
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        rec = json.loads(line)
        out[rec["app_id"]] = list(rec.get("intents", []))
    return out


def _write_json(path: Path, obj) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(obj, ensure_ascii=False, sort_keys=True, separators=(",", ":")))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--export", required=True, type=Path)
    ap.add_argument("--intents", type=Path, default=None)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--base-url", default="https://cmdhub.org")
    ap.add_argument("--build-id", default=None)
    a = ap.parse_args()

    build_id = a.build_id or dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d%H%M%S")
    generated_at = dt.datetime.now(dt.timezone.utc).isoformat()

    data = json.loads(a.export.read_text())
    apps = {x["app_id"]: x for x in data["apps"]}
    args_by_app: dict[str, list] = {}
    for arg in data["arguments"]:
        args_by_app.setdefault(arg["app_id"], []).append(arg)
    intents = _load_intents(a.intents)

    reg = a.out / "registry"
    for app_id, app in apps.items():
        shard = seo_shards.build_shard(
            app, args_by_app.get(app_id, []), intents.get(app_id, []), generated_at
        )
        _write_json(reg / f"{app_id}.json", shard)

    index = seo_shards.build_index(list(apps.values()))
    _write_json(reg / "index.json", index)
    _write_json(reg / "manifest.json", seo_shards.build_manifest(build_id, len(apps), generated_at))

    lastmod = generated_at[:10]
    sm = a.out / "sitemaps"
    sm.mkdir(parents=True, exist_ok=True)
    for name, body in seo_shards.build_sitemaps(index, a.base_url, lastmod).items():
        target = sm / name
        if isinstance(body, bytes):
            target.write_bytes(body)
        else:
            target.write_text(body)
    (a.out / "robots.txt").write_text(seo_shards.build_robots(a.base_url))

    print(f"[seo] wrote {len(apps)} shards + index + sitemaps to {a.out} (build {build_id})")


if __name__ == "__main__":
    main()
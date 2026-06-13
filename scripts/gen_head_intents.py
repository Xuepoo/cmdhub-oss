#!/usr/bin/env python3
"""Pre-generate up to 5 natural-language intents for the top-N apps by popularity.

Reuses the OpenRouter + deepseek-v4-flash pattern from llm_enrich.py (cheap/fast, no
reasoning). Output is a sidecar JSONL consumed by gen_seo_shards.py:
    {"app_id": "...", "intents": ["how to ...", ...]}
Idempotent: skips app_ids already present in the output file.

    OPENROUTER_API_KEY=... python3 gen_head_intents.py --export cmdhub_export.json \\
        --out head_intents.jsonl [--n 5000] [--workers 60] [--model deepseek/deepseek-v4-flash]
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import requests

PROMPT = (
    "You are an SEO assistant for a CLI registry. For the command below, output a JSON "
    "array of up to 5 short natural-language tasks a user would search for (e.g. "
    '"how to compress a folder"). Output ONLY the JSON array.\n\nCommand: {name}\nDescription: {desc}'
)


def select_head(apps: list[dict], n: int) -> list[dict]:
    return sorted(apps, key=lambda a: (-a.get("popularity", 0.0), a["app_id"]))[:n]


def parse_intents(content: str, limit: int) -> list[str]:
    m = re.search(r"\[.*\]", content, re.DOTALL)
    if not m:
        return []
    try:
        arr = json.loads(m.group(0))
    except json.JSONDecodeError:
        return []
    return [str(x) for x in arr][:limit] if isinstance(arr, list) else []


def fetch_intents(session, key: str, model: str, app: dict) -> list[str]:
    body = {
        "model": model,
        "temperature": 0.2,
        "messages": [{"role": "user", "content": PROMPT.format(name=app["name"], desc=app.get("description", ""))}],
    }
    r = session.post(
        "https://openrouter.ai/api/v1/chat/completions",
        headers={"Authorization": f"Bearer {key}"},
        json=body,
        timeout=60,
    )
    r.raise_for_status()
    return parse_intents(r.json()["choices"][0]["message"]["content"], limit=5)


def _already_done(out_path: Path) -> set[str]:
    if not out_path.exists():
        return set()
    return {json.loads(l)["app_id"] for l in out_path.read_text().splitlines() if l.strip()}


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--export", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--n", type=int, default=5000)
    ap.add_argument("--workers", type=int, default=60)
    ap.add_argument("--model", default="deepseek/deepseek-v4-flash")
    a = ap.parse_args()

    key = os.environ.get("OPENROUTER_API_KEY", "")
    if not key:
        print("[error] set OPENROUTER_API_KEY", file=sys.stderr)
        sys.exit(1)

    data = json.loads(a.export.read_text())
    # app description comes from the root argument node
    desc_by_app = {
        arg["app_id"]: arg.get("description", "")
        for arg in data["arguments"]
        if arg.get("node_type") == "root"
    }
    for app in data["apps"]:
        app["description"] = desc_by_app.get(app["app_id"], "")

    done = _already_done(a.out)
    head = [app for app in select_head(data["apps"], a.n) if app["app_id"] not in done]
    print(f"[seo] generating intents for {len(head)} apps (skipping {len(done)} done)")

    session = requests.Session()
    with a.out.open("a") as fh, ThreadPoolExecutor(max_workers=a.workers) as ex:
        def work(app):
            try:
                return app["app_id"], fetch_intents(session, key, a.model, app)
            except Exception as e:  # noqa: BLE001 — one bad app must not kill the batch
                print(f"[warn] {app['app_id']}: {e}", file=sys.stderr)
                return app["app_id"], []

        for app_id, intents in ex.map(work, head):
            if intents:
                fh.write(json.dumps({"app_id": app_id, "intents": intents}, ensure_ascii=False) + "\n")
                fh.flush()


if __name__ == "__main__":
    main()

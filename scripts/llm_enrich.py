#!/usr/bin/env python3
"""LLM enrichment of every command: English description, topics, risk_level, example.

Uses OpenRouter (deepseek-v4-flash) with high concurrency. For each command it returns
JSON {description, topics[], risk_level, example_template}. Output is a resumable JSONL
(one {cmd_path, ...} per line); re-running skips already-done cmd_paths. Network-bound,
meant to run on the VPS (OpenRouter unrestricted, deepseek cheap).

    OPENROUTER_API_KEY=... python3 llm_enrich.py --in commands.json --out enrich.jsonl \
        [--workers 60] [--model deepseek/deepseek-v4-flash] [--proxy ""]
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor

import requests

SYS = (
    "You enrich a CLI command registry for an AI coding agent. Given a tool and one of its "
    "(sub)commands with its current description, return ONLY compact JSON with keys:\n"
    '  "description": one clear English sentence (<=160 chars) stating what THIS command does, '
    "naming the tool; translate/rewrite non-English or cryptic text; keep accurate facts.\n"
    '  "topics": 5-12 lowercase keyword tags a user might search — domain, ecosystem, brand '
    "names, synonyms, the tool name (e.g. for `az`: azure, microsoft, cloud, cli).\n"
    '  "risk_level": one of safe|medium|dangerous (dangerous = deletes/destroys/overwrites; '
    "medium = creates/modifies/deploys state; safe = read-only/local).\n"
    '  "example_template": a realistic one-line usage example, {{placeholders}} for args.'
)


def enrich_one(s, key, model, proxies, item):
    cmd, name, desc = item["cmd_path"], item.get("name", ""), item.get("description", "")
    user = f"tool: {name}\ncommand path: {cmd.replace('.', ' ')}\ncurrent description: {desc[:200]}"
    body = {"model": model, "temperature": 0.2,
            "messages": [{"role": "system", "content": SYS}, {"role": "user", "content": user}]}
    for attempt in range(4):
        try:
            r = s.post("https://openrouter.ai/api/v1/chat/completions",
                       headers={"Authorization": f"Bearer {key}"}, json=body, proxies=proxies, timeout=60)
            if r.status_code == 429:
                time.sleep(2 * (attempt + 1)); continue
            r.raise_for_status()
            t = r.json()["choices"][0]["message"]["content"]
            d = json.loads(t[t.find("{"): t.rfind("}") + 1])
            rl = str(d.get("risk_level", "safe")).lower()
            return {
                "cmd_path": cmd,
                "description": (d.get("description") or desc)[:300],
                "topics": " ".join(str(x).lower().strip() for x in (d.get("topics") or [])[:12] if x)[:300],
                "risk_level": rl if rl in ("safe", "medium", "dangerous") else "safe",
                "example_template": (d.get("example_template") or "")[:200],
            }
        except Exception:
            if attempt == 3:
                return None
            time.sleep(1.5)
    return None


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--in", dest="inp", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--workers", type=int, default=60)
    ap.add_argument("--model", default="deepseek/deepseek-v4-flash")
    ap.add_argument("--proxy", default="")
    a = ap.parse_args()
    key = os.environ.get("OPENROUTER_API_KEY", "")
    if not key:
        print("[error] set OPENROUTER_API_KEY", file=sys.stderr); sys.exit(1)
    proxies = {"https": a.proxy, "http": a.proxy} if a.proxy else {}

    items = json.load(open(a.inp))
    done: set[str] = set()
    if os.path.exists(a.out):
        for line in open(a.out):
            try:
                done.add(json.loads(line)["cmd_path"])
            except Exception:
                pass
    todo = [it for it in items if it["cmd_path"] not in done]
    print(f"[llm] {len(items)} total, {len(done)} done, {len(todo)} to enrich", flush=True)

    s = requests.Session()
    out_f = open(a.out, "a")
    lock = threading.Lock()
    cnt = [0]
    t0 = time.time()

    def work(item):
        res = enrich_one(s, key, a.model, proxies, item)
        if res:
            with lock:
                out_f.write(json.dumps(res, ensure_ascii=False) + "\n")
                cnt[0] += 1
                if cnt[0] % 500 == 0:
                    out_f.flush()
                    el = time.time() - t0
                    rate = cnt[0] / el if el else 0
                    eta = (len(todo) - cnt[0]) / rate if rate else 0
                    print(f"[llm] {cnt[0]}/{len(todo)} — {rate:.0f}/s — ETA {eta/60:.0f}min", flush=True)

    with ThreadPoolExecutor(max_workers=a.workers) as pool:
        list(pool.map(work, todo))
    out_f.flush(); out_f.close()
    print(f"[llm] DONE — enriched {cnt[0]} in {(time.time()-t0)/60:.0f}min")


if __name__ == "__main__":
    main()

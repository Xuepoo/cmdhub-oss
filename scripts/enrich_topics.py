#!/usr/bin/env python3
"""LLM topics enrichment for probe subcommands with empty topics.

Mainstream tools probed from --help have terse descriptions and no topics, so
colloquial queries (with synonyms users actually type) don't recall them. This
fills the topics column with 8-15 search keywords per command via deepseek-flash,
so they recall through the existing FTS-injection path (no ranking change).

Resumable: LLM results are appended to a JSONL cache keyed by cmd_path; re-runs
skip cached rows. Only fills EMPTY topics — never overwrites curated ones.

    OPENROUTER_API_KEY=... uv run --with requests python3 scripts/enrich_topics.py \
        --master tmp/reprobe/master.db --cache tmp/reprobe/topics_cache.jsonl \
        [--tools npm,cargo,docker] [--min-pop 0.8] [--limit N] \
        [--workers 30] [--model deepseek/deepseek-v4-flash] [--proxy http://127.0.0.1:1080]
"""
from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
import threading
from concurrent.futures import ThreadPoolExecutor

import requests

SYS = (
    "You enrich a CLI command registry for a search engine. Given one (sub)command and "
    "its description, output 8-15 lowercase search keywords a user might type to find it: "
    "the tool/brand name, close synonyms, and task words. Stay faithful to the description "
    "— do not invent capabilities the command does not have. Output ONLY the keywords, "
    "comma-separated, no explanation."
)


def build_prompt(cmd_path: str, description: str, parent: str) -> tuple[str, str]:
    words = cmd_path.replace(".", " ").replace("-", " ")
    user = f"Command: `{words}` (parent tool: {parent}). Description: {description or '(none)'}."
    return SYS, user


def parse_topics(raw: str) -> str:
    seen: list[str] = []
    for tok in raw.replace("\n", ",").split(","):
        t = tok.strip().lower()
        if t and t not in seen:
            seen.append(t)
    return ", ".join(seen[:15])


def select_targets(conn, tools, min_pop, limit):
    # Both roots (single-command tools like find/du/pkill) and subcommands need
    # topics — terse probe text + empty topics is what blocks colloquial recall.
    sql = (
        "SELECT ar.cmd_path, ar.description, ar.app_id FROM arguments ar "
        "JOIN apps a ON a.app_id = ar.app_id "
        "WHERE ar.provenance='probe' AND ar.node_type IN ('root','sub') "
        "AND (ar.topics IS NULL OR ar.topics='') AND COALESCE(a.popularity,0.0) >= ?"
    )
    rows = conn.execute(sql, [min_pop]).fetchall()
    out = []
    for cmd_path, desc, _ in rows:
        parent = cmd_path.split(".")[0]
        if tools and parent not in tools:
            continue
        out.append((cmd_path, desc or "", parent))
    return out[:limit] if limit else out


def call_llm(session, key, model, proxies, cmd_path, desc, parent):
    sysmsg, user = build_prompt(cmd_path, desc, parent)
    body = {"model": model, "temperature": 0.2,
            "messages": [{"role": "system", "content": sysmsg},
                         {"role": "user", "content": user}]}
    for _ in range(3):
        try:
            r = session.post("https://openrouter.ai/api/v1/chat/completions",
                             headers={"Authorization": f"Bearer {key}"},
                             json=body, proxies=proxies, timeout=60)
            if r.status_code == 200:
                return parse_topics(r.json()["choices"][0]["message"]["content"])
        except Exception:
            pass
    return None


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--master", required=True)
    ap.add_argument("--cache", required=True)
    ap.add_argument("--tools", default=None)
    ap.add_argument("--min-pop", type=float, default=0.0)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--workers", type=int, default=30)
    ap.add_argument("--model", default="deepseek/deepseek-v4-flash")
    ap.add_argument("--proxy", default="")
    a = ap.parse_args()

    key = os.environ.get("OPENROUTER_API_KEY", "")
    if not key:
        print("[error] set OPENROUTER_API_KEY", file=sys.stderr)
        sys.exit(1)
    proxies = {"http": a.proxy, "https": a.proxy} if a.proxy else None
    tools = a.tools.split(",") if a.tools else None

    done: dict[str, str] = {}
    if os.path.exists(a.cache):
        for line in open(a.cache):
            d = json.loads(line)
            done[d["cmd_path"]] = d["topics"]

    conn = sqlite3.connect(a.master)
    targets = select_targets(conn, tools, a.min_pop, a.limit)
    todo = [t for t in targets if t[0] not in done]
    print(f"[enrich] {len(targets)} targets, {len(todo)} to query ({len(done)} cached)")

    lock = threading.Lock()
    session = requests.Session()
    cache_f = open(a.cache, "a")

    def work(t):
        cmd_path, desc, parent = t
        topics = call_llm(session, key, a.model, proxies, cmd_path, desc, parent)
        if topics:
            with lock:
                cache_f.write(json.dumps({"cmd_path": cmd_path, "topics": topics}) + "\n")
                cache_f.flush()
                done[cmd_path] = topics

    with ThreadPoolExecutor(max_workers=a.workers) as ex:
        list(ex.map(work, todo))
    cache_f.close()

    applied = 0
    for cmd_path, topics in done.items():
        cur = conn.execute(
            "UPDATE arguments SET topics=? WHERE cmd_path=? AND (topics IS NULL OR topics='')",
            (topics, cmd_path))
        applied += cur.rowcount
    conn.commit()
    print(f"[enrich] applied topics to {applied} rows")


if __name__ == "__main__":
    main()

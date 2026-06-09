#!/usr/bin/env python3
"""Agent-style evaluation of cmdh search quality.

For each natural-language task intent, run `cmdh search` and let an LLM (OpenRouter
deepseek) judge — as a real agent would — whether the returned commands actually
satisfy the intent, and at what rank. Reports found-rate, mean reciprocal rank, and
the failing intents so ranking can be tuned with evidence instead of guesswork.

    OPENROUTER_API_KEY=... uv run --with requests python3 scripts/eval_search.py \
        [--cmdh ~/.local/share/cargo/bin/cmdh] [--limit 5] [--model deepseek/deepseek-chat]
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time

import requests

# (intent, hint of what a good answer looks like — for the human-readable report only)
INTENTS: list[tuple[str, str]] = [
    ("delete files", "rm / shred / trash"),
    ("list files in a directory with details", "ls / eza"),
    ("search text inside files recursively", "grep / rg"),
    ("find files by name", "find / fd"),
    ("show disk usage of a directory", "du / dust"),
    ("monitor system processes live", "top / htop / btm"),
    ("kill a process by name", "pkill / killall"),
    ("download a file from a url", "curl / wget"),
    ("extract a tar.gz archive", "tar / ouch"),
    ("change file permissions", "chmod"),
    ("show git commit history", "git log"),
    ("create a new git branch", "git branch / git switch"),
    ("create a vpc on aws", "aws ec2 create-vpc"),
    ("list ec2 instances on aws", "aws ec2 describe-instances"),
    ("create a storage bucket on aws", "aws s3 mb / s3api create-bucket"),
    ("invoke a lambda function on aws", "aws lambda invoke"),
    ("deploy a service to kubernetes", "kubectl apply / create deployment"),
    ("view kubernetes pod logs", "kubectl logs"),
    ("build a docker image", "docker build"),
    ("authenticate with github cli", "gh auth login"),
    ("fuzzy find and jump to a directory", "zoxide / z"),
    ("a better cat with syntax highlighting", "bat"),
    ("convert an image to equations", "vectomancy"),
    ("manage music with an ai agent", "alx / agent-lx-music"),
    ("translate an epub book with an llm", "agent-book-translate"),
    ("offline search for cli commands", "cmdh"),
]

JUDGE_SYS = (
    "You are evaluating a command-line search engine. Given a user's intent and the "
    "ranked commands it returned, decide if any returned command genuinely accomplishes "
    "the intent. Reply ONLY compact JSON: "
    '{"found": true|false, "rank": <1-based index of the best correct result or null>, '
    '"note": "<short reason>"}.'
)


def run_cmdh(cmdh: str, query: str, limit: int) -> list[dict]:
    try:
        out = subprocess.run([cmdh, "search", query, "--limit", str(limit)],
                             capture_output=True, text=True, timeout=20)
        return json.loads(out.stdout or "[]")
    except Exception:
        return []


def judge(session: requests.Session, model: str, key: str, intent: str, results: list[dict]) -> dict:
    listing = "\n".join(
        f"{i+1}. {r.get('cmd_path')} — {r.get('description','')[:80]}"
        for i, r in enumerate(results)
    ) or "(no results)"
    body = {
        "model": model,
        "messages": [
            {"role": "system", "content": JUDGE_SYS},
            {"role": "user", "content": f"Intent: {intent}\nReturned:\n{listing}"},
        ],
        "temperature": 0,
    }
    for attempt in range(3):
        try:
            r = session.post("https://openrouter.ai/api/v1/chat/completions",
                             headers={"Authorization": f"Bearer {key}"}, json=body, timeout=60)
            r.raise_for_status()
            txt = r.json()["choices"][0]["message"]["content"]
            s = txt[txt.find("{"): txt.rfind("}") + 1]
            return json.loads(s)
        except Exception as e:
            if attempt == 2:
                return {"found": False, "rank": None, "note": f"judge error: {e}"}
            time.sleep(2)
    return {"found": False, "rank": None, "note": "judge failed"}


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cmdh", default=os.path.expanduser("~/.local/share/cargo/bin/cmdh"))
    ap.add_argument("--limit", type=int, default=5)
    ap.add_argument("--model", default="deepseek/deepseek-chat")
    ap.add_argument("--proxy", default="http://127.0.0.1:1080")
    args = ap.parse_args()
    key = os.environ.get("OPENROUTER_API_KEY", "")
    if not key:
        print("[error] set OPENROUTER_API_KEY", file=sys.stderr); sys.exit(1)

    session = requests.Session()
    if args.proxy:
        session.proxies = {"https": args.proxy, "http": args.proxy}

    found = 0
    rr_sum = 0.0
    fails: list[str] = []
    for intent, hint in INTENTS:
        results = run_cmdh(args.cmdh, intent, args.limit)
        verdict = judge(session, args.model, key, intent, results)
        ok = bool(verdict.get("found"))
        rank = verdict.get("rank")
        if ok and rank:
            found += 1
            rr_sum += 1.0 / rank
            mark = f"✓ @{rank}"
        else:
            mark = "✗"
            top = results[0]["cmd_path"] if results else "(none)"
            fails.append(f"  {intent!r} → got '{top}' (want {hint}); {verdict.get('note','')[:60]}")
        print(f"[{mark:6}] {intent}", flush=True)
        time.sleep(0.3)

    n = len(INTENTS)
    print(f"\n=== found {found}/{n} ({found*100//n}%)  MRR={rr_sum/n:.3f} ===")
    if fails:
        print("FAILURES:")
        print("\n".join(fails))


if __name__ == "__main__":
    main()

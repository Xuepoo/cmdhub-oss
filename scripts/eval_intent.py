#!/usr/bin/env python3
"""Colloquial-intent golden eval. Reuses eval_golden's search+match logic but
loads queries from data/intent_golden.json and reports recall@1/@5. Use it to
prove the re-probe lift and gate releases against everyday phrasing (the 26-set
in eval_golden uses tool-flavored wording and never caught the npm/docker gaps).
"""
from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from eval_golden import run_cmdh, matches  # noqa: E402


def load_intents(path: str) -> list[tuple[str, list[str]]]:
    data = json.loads(Path(path).read_text())
    return [(d["query"], d["accepts"]) for d in data]


def recall_at_k(rank: int | None, k: int) -> bool:
    return rank is not None and rank <= k


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cmdh", default=os.path.expanduser("~/.local/share/cargo/bin/cmdh"))
    ap.add_argument("--data", default=str(Path(__file__).parent.parent / "data/intent_golden.json"))
    ap.add_argument("--limit", type=int, default=5)
    ap.add_argument("--min-recall1", type=float, default=0.0)
    ap.add_argument("-v", "--verbose", action="store_true")
    a = ap.parse_args()

    intents = load_intents(a.data)
    hit1 = hit5 = 0
    fails = []
    for query, accepts in intents:
        paths = run_cmdh(a.cmdh, query, a.limit)
        rank = next((i + 1 for i, p in enumerate(paths) if matches(p, accepts)), None)
        if recall_at_k(rank, 1):
            hit1 += 1
        if recall_at_k(rank, 5):
            hit5 += 1
        else:
            fails.append(f"  {query!r} -> {paths[:3]} (want {accepts[:3]})")
        if a.verbose:
            print(f"[{('@' + str(rank)) if rank else 'MISS':5}] {query}")
    n = len(intents)
    r1, r5 = hit1 / n, hit5 / n
    print(f"\n=== intent recall@1={r1:.3f} ({hit1}/{n})  recall@5={r5:.3f} ({hit5}/{n}) ===")
    if fails:
        print("OUT OF TOP-5:")
        print("\n".join(fails))
    if r1 < a.min_recall1:
        print(f"FAIL: recall@1 {r1:.3f} < min {a.min_recall1}")
        sys.exit(1)


if __name__ == "__main__":
    main()

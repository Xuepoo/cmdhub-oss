#!/usr/bin/env python3
"""Deterministic golden-set evaluation of cmdh search quality.

The LLM-judge eval (eval_search.py) is great for open-ended correctness but its MRR is
NOISY: the judge's "rank" output varies run to run, so it can't reliably distinguish
ranking configs (search itself is deterministic). This harness instead checks each query's
results against a hardcoded set of acceptable tools/paths and computes found-rate + MRR
deterministically — no LLM, no network, instant, repeatable. Use it to tune ranking.

A result matches if any accept token equals the command's binary (first path segment),
the leaf segment, or the dotted path itself (so "git.log", "aws.ec2.describe-instances",
and "bat" all work).

    python3 eval_golden.py [--cmdh ~/.local/share/cargo/bin/cmdh] [--limit 5] [-v]
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys

# intent -> acceptable answers (matched against binary / leaf / full dotted cmd_path)
GOLDEN: list[tuple[str, list[str]]] = [
    ("delete files", ["rm", "shred", "trash", "rmdir", "unlink", "srm", "trash-put"]),
    ("list files in a directory with details", ["ls", "eza", "exa", "lsd", "ll"]),
    ("search text inside files recursively", ["grep", "rg", "ripgrep", "ag", "ack", "ugrep"]),
    ("find files by name", ["find", "fd", "fdfind"]),
    ("show disk usage of a directory", ["du", "dust", "ncdu", "dua", "gdu", "dust"]),
    ("monitor system processes live", ["top", "htop", "btop", "btm", "glances", "gtop"]),
    ("kill a process by name", ["pkill", "killall", "kill"]),
    ("download a file from a url", ["curl", "wget", "aria2", "aria2c", "httpie", "http", "xh"]),
    ("extract a tar.gz archive", ["tar", "ouch", "atool", "bsdtar", "unp", "dtrx"]),
    ("change file permissions", ["chmod"]),
    ("show git commit history", ["log"]),  # git.log
    ("create a new git branch", ["branch", "switch", "checkout"]),  # git.*
    ("create a vpc on aws", ["create-vpc"]),  # aws.ec2.create-vpc
    ("list ec2 instances on aws", ["describe-instances"]),
    ("create a storage bucket on aws", ["mb", "create-bucket"]),
    ("invoke a lambda function on aws", ["invoke"]),
    ("deploy a service to kubernetes", ["apply", "create", "helm", "kubectl"]),
    ("view kubernetes pod logs", ["logs"]),  # kubectl.logs
    ("build a docker image", ["build", "buildx", "buildkit"]),  # docker.*build*
    ("authenticate with github cli", ["auth", "login"]),  # gh.auth*
    ("fuzzy find and jump to a directory", ["zoxide", "z", "autojump", "fasd", "jump"]),
    ("a better cat with syntax highlighting", ["bat", "batcat"]),
    ("convert an image to equations", ["vectomancy", "pix2tex", "mathpix", "texify"]),
    ("manage music with an ai agent", ["alx", "agent-lx-music"]),
    ("translate an epub book with an llm", ["agent-book-translate"]),
    ("offline search for cli commands", ["cmdh", "cmdhub"]),
]


def segs(cmd_path: str) -> set[str]:
    parts = [p.lower() for p in cmd_path.split(".")]
    out = set(parts)
    out.add(parts[0])   # binary
    out.add(parts[-1])  # leaf
    out.add(cmd_path.lower())
    return out


def matches(cmd_path: str, accepts: list[str]) -> bool:
    s = segs(cmd_path)
    return any(a.lower() in s for a in accepts)


def run_cmdh(cmdh: str, query: str, limit: int) -> list[str]:
    try:
        out = subprocess.run([cmdh, "search", query, "--limit", str(limit)],
                             capture_output=True, text=True, timeout=20)
        return [d.get("cmd_path", "") for d in json.loads(out.stdout or "[]")]
    except Exception:
        return []


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cmdh", default=os.path.expanduser("~/.local/share/cargo/bin/cmdh"))
    ap.add_argument("--limit", type=int, default=5)
    ap.add_argument("-v", "--verbose", action="store_true")
    a = ap.parse_args()

    found = 0
    rr = 0.0
    fails = []
    for intent, accepts in GOLDEN:
        paths = run_cmdh(a.cmdh, intent, a.limit)
        rank = next((i + 1 for i, p in enumerate(paths) if matches(p, accepts)), None)
        if rank:
            found += 1
            rr += 1.0 / rank
            mark = f"@{rank}"
        else:
            mark = "MISS"
            fails.append(f"  {intent!r} → {paths[:3]} (want {accepts[:4]})")
        if a.verbose:
            print(f"[{mark:5}] {intent}")
    n = len(GOLDEN)
    print(f"\n=== found {found}/{n} ({found*100//n}%)  MRR={rr/n:.3f} ===")
    if fails:
        print("MISSES:")
        print("\n".join(fails))


if __name__ == "__main__":
    main()

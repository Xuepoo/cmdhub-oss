#!/usr/bin/env python3
"""Reverse-coverage search tester: for each probe-verified command, generate
name-free task queries, run `cmdh search`, report unfindable tools categorized
with fix suggestions. Discovery tool, not a release gate. See
docs/superpowers/specs/2026-06-20-coverage-search-tester-design.md
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import sqlite3
import subprocess
import sys
import time
from collections import Counter
from dataclasses import dataclass, field


@dataclass
class Command:
    cmd_path: str
    description: str


@dataclass
class Verdict:
    cmd_path: str
    query: str
    status: str                       # "pass" | "near_miss" | "fail"
    rank: int | None                  # 1-based rank of cmd_path, else None
    category: str | None = None       # set for fails/near-misses
    blockers: list[str] = field(default_factory=list)  # cmd_paths above source
    suggestion: str | None = None


def load_commands(db_path: str) -> list[Command]:
    """Probe-verified commands (cmd_path + description) from the master registry."""
    con = sqlite3.connect(db_path)
    rows = con.execute(
        "SELECT cmd_path, COALESCE(description,'') FROM arguments "
        "WHERE provenance='probe' AND cmd_path IS NOT NULL"
    ).fetchall()
    con.close()
    return [Command(cmd_path=p, description=d) for p, d in rows if p]


NEAR_MISS_MAX = 20

_STOPWORDS = {
    "a", "an", "the", "to", "into", "from", "with", "of", "for", "on", "in",
    "and", "or", "by", "as", "at", "how", "do", "i", "my", "this", "that",
    "using", "use", "via", "your", "their", "it", "its",
}

_SEVERITY = {"not_found": 0, "canonical_burial": 1, "inferred_attractor": 1,
             "sibling_misorder": 2, "genuine_ambiguity": 3}

GEN_SYS = (
    "You generate test queries for a command-line search engine. For each tool "
    "given (name + description), output 3 short natural-language queries a user "
    "would type to find a tool that does that job. CRITICAL: describe the TASK; "
    "never mention the tool's name or binary. Reply ONLY compact JSON mapping "
    'each cmd_path to a list of 3 strings: {"<cmd_path>": ["q1","q2","q3"], ...}.'
)


def name_echo_filter(cmd_path: str, query: str) -> bool:
    """True if the query is clean (keep it); False if it leaks a path token."""
    q = f" {query.lower()} "
    for seg in cmd_path.lower().split("."):
        if len(seg) < 2:
            continue
        if f" {seg} " in q or f" {seg}." in q or f" {seg}," in q:
            return False
    return True


def evaluate(cmd_path: str, results: list[dict], k: int = 5) -> Verdict:
    rank = None
    for i, r in enumerate(results):
        if r.get("cmd_path") == cmd_path:
            rank = i + 1
            break
    if rank is not None and rank <= k:
        status = "pass"
    elif rank is not None and rank <= NEAR_MISS_MAX:
        status = "near_miss"
    else:
        status = "fail"
        rank = None
    blockers = [r.get("cmd_path", "") for r in results[:k]]
    return Verdict(cmd_path=cmd_path, query="", status=status, rank=rank,
                   blockers=blockers)


def categorize(cmd_path: str, results: list[dict], k: int = 5) -> str:
    """Classify why cmd_path failed, by inspecting the top-k blockers.
    Precedence: not_found > sibling_misorder > canonical_burial > genuine_ambiguity."""
    top = results[:k]
    if not top:
        return "not_found"
    root = cmd_path.split(".")[0]
    blocker_paths = [r.get("cmd_path", "") for r in top]
    if any(bp != cmd_path and bp.split(".")[0] == root for bp in blocker_paths):
        return "sibling_misorder"
    if any(r.get("verified") is False for r in top):
        return "canonical_burial"
    return "genuine_ambiguity"


def flag_attractors(fails: list[Verdict], min_hits: int = 3) -> set[str]:
    """cmd_paths appearing as the rank-1 blocker across >= min_hits distinct fails."""
    top1 = Counter(v.blockers[0] for v in fails if v.blockers)
    return {path for path, n in top1.items() if n >= min_hits}


def apply_attractor_category(fails: list[Verdict], attractors: set[str]) -> None:
    for v in fails:
        if v.blockers and v.blockers[0] in attractors:
            v.category = "inferred_attractor"


def suggest_override(v: Verdict) -> str | None:
    """Candidate topics_append for burial-class fails: the query's content words."""
    if v.category not in ("canonical_burial", "inferred_attractor"):
        return None
    words = [w.strip(".,()[]<>") for w in v.query.lower().split()]
    content = [w for w in words if w and w not in _STOPWORDS and len(w) > 1]
    seen: list[str] = []
    for w in content:
        if w not in seen:
            seen.append(w)
    return " ".join(seen) if seen else None


def _extract_json(text: str) -> dict:
    """Tolerant JSON extraction: strips ```json fences / preamble, then parses
    the outermost {...}. Returns {} on failure (deepseek sometimes wraps output)."""
    if not text:
        return {}
    t = text.strip()
    if "```" in t:                       # drop code fences
        t = t.replace("```json", "```")
        parts = t.split("```")
        t = max(parts, key=len) if len(parts) > 1 else t
    start, end = t.find("{"), t.rfind("}")
    if start == -1 or end == -1 or end <= start:
        return {}
    try:
        out = json.loads(t[start:end + 1])
        return out if isinstance(out, dict) else {}
    except Exception:
        return {}


def run_searches_parallel(cmdh: str, pairs: list[tuple[str, str]], limit: int,
                          workers: int) -> dict[tuple[str, str], list[dict]]:
    """Run cmdh search for many (cmd_path, query) pairs concurrently.
    subprocess-bound work overlaps well; returns results keyed by the pair."""
    from concurrent.futures import ThreadPoolExecutor
    out: dict[tuple[str, str], list[dict]] = {}

    def one(pair):
        cmd_path, q = pair
        return pair, run_cmdh(cmdh, q, limit)

    with ThreadPoolExecutor(max_workers=workers) as pool:
        for pair, res in pool.map(one, pairs):
            out[pair] = res
    return out


def _cmd_hash(c: Command) -> str:
    return hashlib.sha1(f"{c.cmd_path}\x00{c.description}".encode()).hexdigest()


def _llm_generate_batch(batch: list[Command], session, model: str, key: str) -> dict:
    listing = "\n".join(f"{c.cmd_path}: {c.description[:160]}" for c in batch)
    body = {"model": model, "temperature": 0.3,
            "messages": [{"role": "system", "content": GEN_SYS},
                         {"role": "user", "content": listing}]}
    for attempt in range(3):
        try:
            r = session.post("https://openrouter.ai/api/v1/chat/completions",
                             headers={"Authorization": f"Bearer {key}"},
                             json=body, timeout=90)
            r.raise_for_status()
            txt = r.json()["choices"][0]["message"]["content"]
            parsed = _extract_json(txt)
            if parsed:
                return parsed
            # empty parse: retry (transient bad formatting) unless last attempt
            if attempt == 2:
                return {}
        except Exception:
            if attempt == 2:
                return {}
        time.sleep(2)
    return {}


def generate_queries(commands: list[Command], cache_path: str, per_tool: int,
                     batch_size: int, session, model: str, key: str,
                     regen: bool = False) -> dict[str, list[str]]:
    cache: dict[str, dict] = {}
    if os.path.exists(cache_path) and not regen:
        cache = json.load(open(cache_path))
    todo = [c for c in commands if _cmd_hash(c) not in cache]
    for i in range(0, len(todo), batch_size):
        batch = todo[i:i + batch_size]
        raw = _llm_generate_batch(batch, session, model, key)
        for c in batch:
            qs = raw.get(c.cmd_path, []) if isinstance(raw, dict) else []
            kept = [q for q in qs if isinstance(q, str)
                    and name_echo_filter(c.cmd_path, q)][:per_tool]
            cache[_cmd_hash(c)] = {"cmd_path": c.cmd_path, "queries": kept}
        json.dump(cache, open(cache_path, "w"))
        print(f"  [gen] {min(i + batch_size, len(todo))}/{len(todo)}",
              file=sys.stderr, flush=True)
    return {cache[_cmd_hash(c)]["cmd_path"]: cache[_cmd_hash(c)]["queries"]
            for c in commands}


def passes_count(verdicts: list[Verdict]) -> int:
    return sum(1 for v in verdicts if v.status == "pass")


def render_report(verdicts: list[Verdict], total_queries: int) -> tuple[str, list[dict]]:
    passes = sum(1 for v in verdicts if v.status == "pass")
    fails = [v for v in verdicts if v.status != "pass"]
    fails.sort(key=lambda v: (_SEVERITY.get(v.category, 9), v.cmd_path))
    cat_counts = Counter(v.category for v in fails)
    rate = (passes / total_queries * 100) if total_queries else 0.0

    lines = ["# Coverage sweep report", "",
             f"- queries: {total_queries}  pass: {passes}/{total_queries} ({rate:.1f}%)",
             f"- near-miss: {sum(1 for v in fails if v.status=='near_miss')}",
             "- categories: " + ", ".join(f"{c}={n}" for c, n in cat_counts.most_common()),
             "", "| category | tool | query | top blockers | suggested topics |",
             "|---|---|---|---|---|"]
    for v in fails:
        lines.append(f"| {v.category} | `{v.cmd_path}` | {v.query} | "
                     f"{', '.join(v.blockers[:3])} | {v.suggestion or ''} |")
    data = [{"cmd_path": v.cmd_path, "query": v.query, "status": v.status,
             "rank": v.rank, "category": v.category, "blockers": v.blockers,
             "suggestion": v.suggestion} for v in fails]
    return "\n".join(lines), data


JUDGE_SYS = (
    "You evaluate a command-line search engine. Given a user's task intent and the "
    "top commands it returned, decide if ANY returned command genuinely accomplishes "
    "the intent (an equivalent tool counts — the user just needs a working answer). "
    'Reply ONLY compact JSON: {"satisfied": true|false, "note": "<short reason>"}.'
)


def judge_results(session, model: str, key: str, query: str,
                  results: list[dict]) -> bool:
    """LLM judge: did the search return ANY command that satisfies the query?
    Used to correct the coverage pass-rate (a non-source but equivalent tool in
    the top results is a real success from the user's perspective)."""
    listing = "\n".join(
        f"{i+1}. {r.get('cmd_path')} — {(r.get('description') or '')[:80]}"
        for i, r in enumerate(results[:5])) or "(no results)"
    body = {"model": model, "temperature": 0,
            "messages": [{"role": "system", "content": JUDGE_SYS},
                         {"role": "user", "content": f"Intent: {query}\nReturned:\n{listing}"}]}
    for attempt in range(3):
        try:
            r = session.post("https://openrouter.ai/api/v1/chat/completions",
                             headers={"Authorization": f"Bearer {key}"},
                             json=body, timeout=60)
            r.raise_for_status()
            parsed = _extract_json(r.json()["choices"][0]["message"]["content"])
            if parsed:
                return bool(parsed.get("satisfied"))
            if attempt == 2:
                return False
        except Exception:
            if attempt == 2:
                return False
        time.sleep(2)
    return False


def run_cmdh(cmdh: str, query: str, limit: int) -> list[dict]:
    """Run `cmdh search`, return parsed JSON results (empty on any error)."""
    try:
        out = subprocess.run([cmdh, "search", query, "--limit", str(limit)],
                             capture_output=True, text=True, timeout=20)
        data = json.loads(out.stdout or "[]")
        return data if isinstance(data, list) else []
    except Exception:
        return []


def main() -> None:
    ap = argparse.ArgumentParser(description="Reverse-coverage cmdh search tester")
    ap.add_argument("--cmdh", default=os.path.expanduser("~/.local/share/cargo/bin/cmdh"))
    ap.add_argument("--db", default="../tmp/rebuild-v4/cmdhub.db",
                    help="master registry (probe commands)")
    ap.add_argument("--model", default="deepseek/deepseek-v4-flash")
    ap.add_argument("--proxy", default="http://127.0.0.1:1080")
    ap.add_argument("--queries-per-tool", type=int, default=3)
    ap.add_argument("--batch-size", type=int, default=20)
    ap.add_argument("--workers", type=int, default=16,
                    help="parallel cmdh search subprocesses")
    ap.add_argument("--limit", type=int, default=20, help="cmdh --limit (>=20 for near-miss)")
    ap.add_argument("--k", type=int, default=5, help="pass threshold")
    ap.add_argument("--sample", type=int, default=0, help="test only first N commands")
    ap.add_argument("--queries-cache", default="/tmp/coverage_queries.json")
    ap.add_argument("--fails-json", default="/tmp/coverage_fails.json")
    ap.add_argument("--report", default="", help="write markdown report to this path")
    ap.add_argument("--regen", action="store_true", help="ignore query cache")
    ap.add_argument("--judge", action="store_true",
                    help="LLM-judge each fail (does ANY top result satisfy the query?) "
                         "to report a corrected, equivalent-tool-aware pass rate")
    ap.add_argument("--judge-cache", default="/tmp/coverage_judge.json")
    args = ap.parse_args()

    import requests
    key = os.environ.get("OPENROUTER_API_KEY", "")
    if not key:
        print("[error] set OPENROUTER_API_KEY", file=sys.stderr); sys.exit(1)
    session = requests.Session()
    if args.proxy:
        session.proxies = {"https": args.proxy, "http": args.proxy}

    commands = load_commands(args.db)
    if args.sample:
        commands = commands[:args.sample]
    print(f"[coverage] {len(commands)} probe commands", file=sys.stderr)

    queries = generate_queries(commands, args.queries_cache, args.queries_per_tool,
                               args.batch_size, session, args.model, key, args.regen)

    pairs = [(c.cmd_path, q) for c in commands for q in queries.get(c.cmd_path, [])]
    print(f"[coverage] {len(pairs)} searches across {args.workers} workers",
          file=sys.stderr)
    searched = run_searches_parallel(args.cmdh, pairs, args.limit, args.workers)

    verdicts: list[Verdict] = []
    total = len(pairs)
    for cmd_path, q in pairs:
        results = searched.get((cmd_path, q), [])
        v = evaluate(cmd_path, results, args.k)
        v.query = q
        if v.status != "pass":
            v.category = categorize(cmd_path, results, args.k)
        verdicts.append(v)

    fails = [v for v in verdicts if v.status != "pass"]
    attractors = flag_attractors(fails)
    apply_attractor_category(fails, attractors)
    for v in fails:
        v.suggestion = suggest_override(v)

    md, data = render_report(verdicts, total)
    json.dump(data, open(args.fails_json, "w"), indent=2)
    if args.report:
        open(args.report, "w").write(md)
    print(md)
    print(f"\n[coverage] fails JSON -> {args.fails_json}", file=sys.stderr)

    if args.judge:
        from concurrent.futures import ThreadPoolExecutor
        cache: dict[str, bool] = {}
        if os.path.exists(args.judge_cache):
            cache = json.load(open(args.judge_cache))
        # judge each fail: did the top results satisfy the query anyway (equiv tool)?
        to_judge = [v for v in fails if v.query not in cache]
        print(f"[judge] {len(to_judge)} fails to judge ({len(cache)} cached)",
              file=sys.stderr)

        def jone(v: Verdict):
            res = searched.get((v.cmd_path, v.query), [])
            return v.query, judge_results(session, args.model, key, v.query, res)

        with ThreadPoolExecutor(max_workers=8) as pool:
            for i, (q, ok) in enumerate(pool.map(jone, to_judge)):
                cache[q] = ok
                if (i + 1) % 200 == 0:
                    json.dump(cache, open(args.judge_cache, "w"))
                    print(f"  [judge] {i+1}/{len(to_judge)}", file=sys.stderr)
        json.dump(cache, open(args.judge_cache, "w"))

        satisfied = sum(1 for v in fails if cache.get(v.query))
        true_pass = passes_count(verdicts) + satisfied
        true_fail = total - true_pass
        print(f"\n[judge] CORRECTED: {true_pass}/{total} satisfied "
              f"({true_pass / total * 100:.1f}%) — of {len(fails)} raw fails, "
              f"{satisfied} returned an equivalent tool, {true_fail} are real misses",
              file=sys.stderr)


if __name__ == "__main__":
    main()

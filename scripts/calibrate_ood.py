#!/usr/bin/env python3
"""Calibrate OOD confidence thresholds (HARD/SOFT) for db.rs.

Reproduces, in Python, exactly what `search_cascading` computes before re-ranking:
  - lowest_dist: min L2 distance from the query embedding to a node_type='root'
    command vector (k=100 KNN), the raw vector signal.
  - and_match / or_match: whether the FTS5 AND / OR query (preprocess_query +
    concept_synonyms, prefix `word*`) matches any apps_fts doc.
The production gate fires (bail to empty) iff: lowest_dist > HARD && !and_match && !or_match.

We measure lowest_dist distributions for POSITIVE queries (colloquial / CLI snippets /
golden) vs OOD queries, and report where to set HARD (empty cutoff) and SOFT (low-conf
band) so OOD is filtered while colloquial recall is preserved.

Usage:
  uv run --with onnxruntime --with sqlite-vec --with numpy python3 calibrate_ood.py \
      --db /path/to/v3/cmdhub.db
"""
from __future__ import annotations

import argparse
import math
import re
import sqlite3
import statistics
import sys
from pathlib import Path

import build_db  # tokenizer + vocab helpers (same embedding path as the real build)

HERE = Path(__file__).resolve().parent
SUITE = HERE.parent / "cmdhub-cli/tests/search_robustness.rs"
MODEL = Path("/home/fuyu/.local/share/cmdhub/models/bge-micro-v2.onnx")
VOCAB = build_db.VOCAB_GZ_PATH

STOP = {"how", "to", "a", "the", "on", "in", "of", "for", "with", "an", "is", "at",
        "by", "and", "or", "from", "my", "your", "our", "me", "us"}

# Mirror db.rs concept_synonyms (OR-query widening only).
SYNONYMS = {
    "networking": ["vpc", "subnet", "gateway", "route", "firewall"],
    "network": ["vpc", "subnet", "gateway", "route", "firewall"],
    "firewall": ["security", "firewall", "acl"],
    "storage": ["bucket", "volume", "disk", "blob"],
    "database": ["database", "sql", "table", "rds"], "db": ["database", "sql", "table", "rds"],
    "serverless": ["lambda", "function", "faas"],
    "container": ["container", "image", "pod"], "containers": ["container", "image", "pod"],
    "kubernetes": ["pod", "deployment", "namespace", "cluster"],
    "k8s": ["pod", "deployment", "namespace", "cluster"],
    "secret": ["secret", "credential", "key", "vault"], "secrets": ["secret", "credential", "key", "vault"],
    "dns": ["dns", "domain", "record", "zone"],
    "delete": ["remove", "unlink", "trash"], "erase": ["remove", "unlink", "trash"],
    "remove": ["delete", "unlink"],
    "clear": ["prune", "remove", "rm", "delete", "unused"], "clean": ["prune", "remove", "rm", "delete", "unused"],
    "cleanup": ["prune", "remove", "rm", "delete", "unused"], "purge": ["prune", "remove", "rm", "delete", "unused"],
    "prune": ["clean", "remove", "delete", "unused"],
    "view": ["show", "display"], "read": ["show", "display"],
    "deploy": ["apply", "install"], "deployment": ["apply", "install"],
    "history": ["log", "commits"],
    "cat": ["bat", "less", "pager"],
}


def preprocess_query(query: str, use_and: bool) -> str:
    base = [w.lower() for w in re.split(r"[^0-9a-zA-Z_]+", query) if w]
    base = [w for w in base if w not in STOP]
    if not base:
        return "*"
    terms = [f"{w}*" for w in base]
    if not use_and:
        seen = set(base)
        for w in base:
            for syn in SYNONYMS.get(w, []):
                if syn not in seen:
                    seen.add(syn)
                    terms.append(f"{syn}*")
    return " ".join(terms) if use_and else " OR ".join(terms)


def fts_match(conn, fts_query: str) -> bool:
    if fts_query == "*":
        return False
    try:
        n = conn.execute("SELECT count(*) FROM apps_fts WHERE apps_fts MATCH ?", (fts_query,)).fetchone()[0]
        return n > 0
    except sqlite3.OperationalError:
        return False


def load_session():
    import onnxruntime as ort
    opts = ort.SessionOptions()
    opts.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    sess = ort.InferenceSession(str(MODEL), sess_options=opts, providers=["CPUExecutionProvider"])
    name_map = {}
    for inp in sess.get_inputs():
        nl = inp.name.lower().replace("_", "").replace("-", "")
        if nl == "inputids":
            name_map["input_ids"] = inp.name
        elif nl == "attentionmask":
            name_map["attention_mask"] = inp.name
        elif nl == "tokentypeids":
            name_map["token_type_ids"] = inp.name
    return sess, name_map


def embed(sess, name_map, vocab, text: str):
    import numpy as np
    ids, mask = build_db._tokenize(vocab, text)
    ids_a = np.array([ids], dtype=np.int64)
    mask_a = np.array([mask], dtype=np.int64)
    feeds = {name_map["input_ids"]: ids_a, name_map["attention_mask"]: mask_a}
    if "token_type_ids" in name_map:
        feeds[name_map["token_type_ids"]] = np.zeros_like(ids_a)
    out = sess.run(None, feeds)[0]
    if out.ndim == 3:
        exp = mask_a.astype(np.float32)[:, :, None]
        pooled = (out * exp).sum(axis=1) / np.maximum(mask_a.sum(axis=1, keepdims=True), 1e-9)
    else:
        pooled = out
    norm = np.linalg.norm(pooled, axis=1, keepdims=True)
    return (pooled / np.maximum(norm, 1e-9))[0].astype(np.float32)


def lowest_root_dist(conn, vec) -> float:
    import struct
    qb = struct.pack(f"{len(vec)}f", *vec)
    row = conn.execute(
        "SELECT v.distance FROM ("
        "  SELECT cmd_path, distance FROM commands_vec WHERE embedding MATCH ? AND k = 100"
        ") v JOIN arguments arg ON v.cmd_path = arg.cmd_path "
        "WHERE arg.node_type = 'root' ORDER BY v.distance ASC LIMIT 1",
        (qb,),
    ).fetchone()
    return float(row[0]) if row else float("inf")


def parse_suite_queries():
    """Extract (query, category) for Colloquial + CLI Snippets cases."""
    txt = SUITE.read_text()
    out = []
    for m in re.finditer(r'query:\s*"((?:[^"\\]|\\.)*)"\s*,.*?category:\s*"([^"]+)"', txt, re.S):
        q, cat = m.group(1), m.group(2)
        if cat in ("Colloquial", "CLI Snippets"):
            out.append((q, cat))
    return out


GOLDEN = [
    "delete files", "list files in a directory with details", "search text inside files recursively",
    "find files by name", "show disk usage of a directory", "compress a folder into an archive",
    "monitor system processes", "kill a process by name", "show network connections",
    "download a file from a url", "make a directory", "change file permissions",
]

OOD = [
    "how to bake a chocolate cake", "weather forecast tomorrow in Paris",
    "who is the current president of the USA", "banana apple orange watermelon fresh juice",
    "calculate the distance from earth to moon in miles", "what time is it right now",
    "translate hello into french please", "best pizza toppings for a party",
    "lyrics to my favorite pop song", "history of the roman empire",
    "how tall is mount everest in meters", "recipe for chicken curry with rice",
]


def stats(label, dists):
    ds = sorted(d for d in dists if math.isfinite(d))
    if not ds:
        print(f"  {label:16s} (no finite)")
        return
    p = lambda q: ds[min(len(ds) - 1, int(q * len(ds)))]
    print(f"  {label:16s} n={len(ds):3d}  min={ds[0]:.3f}  p25={p(.25):.3f}  "
          f"median={statistics.median(ds):.3f}  p75={p(.75):.3f}  p90={p(.90):.3f}  max={ds[-1]:.3f}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", required=True)
    args = ap.parse_args()

    import sqlite_vec
    conn = sqlite3.connect(args.db)
    conn.enable_load_extension(True)
    sqlite_vec.load(conn)
    conn.enable_load_extension(False)

    vocab = build_db._load_vocab(VOCAB)
    sess, name_map = load_session()

    pos = parse_suite_queries()
    colloquial = [q for q, c in pos if c == "Colloquial"]
    snippets = [q for q, c in pos if c == "CLI Snippets"]
    print(f"[calib] colloquial={len(colloquial)} snippets={len(snippets)} golden={len(GOLDEN)} ood={len(OOD)}\n")

    groups = {"Colloquial": colloquial, "CLI Snippets": snippets, "Golden": GOLDEN, "OOD": OOD}
    dist_by = {}
    gate_detail = {}
    for name, qs in groups.items():
        dists = []
        fired = 0
        rows = []
        for q in qs:
            v = embed(sess, name_map, vocab, q)
            d = lowest_root_dist(conn, v)
            am = fts_match(conn, preprocess_query(q, True))
            om = fts_match(conn, preprocess_query(q, False))
            gate = (d > 1.14) and (not am) and (not om)  # current prod gate
            fired += 1 if gate else 0
            dists.append(d)
            rows.append((q, d, am, om, gate))
        dist_by[name] = dists
        gate_detail[name] = rows
        print(f"[{name}] current-gate fires on {fired}/{len(qs)}")

    print("\n=== lowest_root_dist distribution ===")
    for name in groups:
        stats(name, dist_by[name])

    print("\n=== OOD per-query (dist | and | or | current-gate-fires) ===")
    for q, d, am, om, g in gate_detail["OOD"]:
        print(f"  {d:.3f}  A={int(am)} O={int(om)}  fire={int(g)}  {q}")

    print("\n=== positives that the gate would WRONGLY empty (dist>HARD & no fts) — must be ~0 ===")
    for name in ("Colloquial", "CLI Snippets", "Golden"):
        bad = [(q, d) for q, d, am, om, g in gate_detail[name] if g]
        print(f"  {name}: {len(bad)} {[f'{q}={d:.2f}' for q, d in bad[:5]]}")

    all_pos = gate_detail["Colloquial"] + gate_detail["CLI Snippets"] + gate_detail["Golden"]

    # and/or match rates: or_match is universally true (prefix-OR matches the 103k corpus),
    # so the production `!or_match` guard makes the gate dead. and_match is the usable guard.
    for name in ("Colloquial", "CLI Snippets", "Golden", "OOD"):
        rows = gate_detail[name]
        a = sum(1 for q, d, am, om, g in rows if am)
        o = sum(1 for q, d, am, om, g in rows if om)
        print(f"[{name}] and_match={a}/{len(rows)}  or_match={o}/{len(rows)}")

    # CORRECTED gate: dist>HARD AND !and_match (drop the dead !or_match guard).
    print("\n=== HARD sweep (CORRECTED gate = dist>HARD & !and_match) ===")
    print("  HARD   OOD_filtered  POS_wrongly_emptied")
    for hard in (0.76, 0.78, 0.80, 0.81, 0.82, 0.83, 0.84, 0.85, 0.88, 0.90):
        ood_f = sum(1 for q, d, am, om, g in gate_detail["OOD"] if d > hard and not am)
        pos_e = sum(1 for q, d, am, om, g in all_pos if d > hard and not am)
        print(f"  {hard:.2f}   {ood_f:2d}/{len(OOD)}        {pos_e}/{len(all_pos)}")

    # SOFT band (low-confidence flag; just FLAGS, no drop) over a range of HARD anchors.
    print("\n=== SOFT band (flag dist in (SOFT,HARD], HARD=0.82) ===")
    print("  SOFT   POS_flagged  OOD_in_band")
    for soft in (0.70, 0.72, 0.74, 0.76, 0.78):
        pos_fl = sum(1 for q, d, am, om, g in all_pos if soft < d <= 0.82)
        ood_b = sum(1 for q, d, am, om, g in gate_detail["OOD"] if soft < d <= 0.82)
        print(f"  {soft:.2f}   {pos_fl}/{len(all_pos)}      {ood_b}/{len(OOD)}")


if __name__ == "__main__":
    main()

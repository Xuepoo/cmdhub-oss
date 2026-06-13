#!/usr/bin/env python3
"""Calibrate Stage-1 relevance ranking to stop burying canonical tools (#10).

Reproduces db.rs `search_cascading` Stage-1 app selection in Python so we can sweep
ranking levers WITHOUT rebuilding Rust, measuring two gates simultaneously:
  - Canonical basket: for concept-word queries (fuzzy find file -> fzf), is the
    canonical tool's app in Stage-1 top-K? (burial = not in top-K)
  - Golden guard: do the eval_golden intents still surface their accepted tools?
    (regression guard — the 88% popularity trap)

Levers swept:
  - cand: candidate query strategy
      "current"  = db.rs today (and_match ? and_query : or_query) — synonyms dropped on AND
      "or_syn"   = always OR-query (synonyms always in)
      "and_syn"  = AND-query + appended synonym OR-terms (keep AND precision, add canon recall)
  - pw_lin / pw_cube : popularity prior weights (db.rs: multi-token = 0.0 / 0.015)
  - cold_floor : low-popularity name-match penalty. fts-RRF scaled by (floor+(1-floor)*pop);
      1.0 = no penalty (current), <1.0 = obscure namesakes contribute less on name match.

Usage:
  uv run --with onnxruntime --with sqlite-vec --with numpy python3 calibrate_relevance.py \
      --db /path/to/v3/cmdhub.db
"""
from __future__ import annotations

import argparse
import sqlite3
import struct

import build_db
from calibrate_ood import SYNONYMS, embed, load_session, preprocess_query, fts_match
from eval_golden import GOLDEN

VOCAB = build_db.VOCAB_GZ_PATH

# (concept query, accepted canonical tool names) — the burial basket.
BASKET = [
    ("fuzzy find file", {"fzf"}),
    ("fuzzy finder", {"fzf", "skim", "sk"}),
    ("search text in files recursively", {"rg", "ripgrep", "grep", "ag", "ack"}),
    ("http client request", {"curl", "httpie", "http", "xh", "wget"}),
    ("json processor command line", {"jq"}),
    ("fast file finder", {"fd", "fd-find", "fdfind"}),
    ("syntax highlighting cat", {"bat"}),
    ("interactive disk usage", {"ncdu", "dust", "du", "dua", "gdu"}),
    ("process monitor top", {"htop", "top", "btop", "btm"}),
    ("download files from url", {"wget", "curl", "aria2", "aria2c"}),
    ("markdown viewer terminal", {"glow", "mdcat", "mdv"}),
    ("rust package manager", {"cargo"}),
    ("kubernetes command line", {"kubectl", "k9s"}),
    ("git diff viewer", {"delta", "diff-so-fancy", "difft", "git"}),
    ("count lines of code", {"tokei", "cloc", "scc"}),
    ("replace grep with faster tool", {"rg", "ripgrep"}),
    ("paginate output less", {"less", "bat", "most"}),
    ("compress files zip", {"zip", "7z", "gzip", "tar"}),
    ("show directory tree", {"tree", "eza", "exa", "lsd"}),
    ("benchmark a command", {"hyperfine"}),
]

TOPK = 5  # Stage-1 keeps top-5 apps (db.rs LIMIT 5)


def build_cand(query, conn, strategy):
    """Return the FTS MATCH candidate string for the chosen strategy."""
    and_q = preprocess_query(query, True)
    or_q = preprocess_query(query, False)
    and_match = fts_match(conn, and_q)
    if strategy == "current":
        return (and_q if and_match else or_q), and_match
    if strategy == "or_syn":
        return or_q, and_match
    if strategy == "and_syn":
        if not and_match:
            return or_q, and_match
        # AND terms + synonym-only OR terms (so canonical tools that only match via a
        # synonym name, e.g. fzf for "fuzzy", enter the candidate pool).
        base = [w for w in query.lower().replace("-", " ").split() if w]
        syn_terms = []
        seen = set(base)
        for w in base:
            for s in SYNONYMS.get(w, []):
                if s not in seen:
                    seen.add(s)
                    syn_terms.append(f"{s}*")
        if syn_terms:
            return f"({and_q}) OR " + " OR ".join(syn_terms), and_match
        return and_q, and_match
    raise ValueError(strategy)


STAGE1_SQL = """
WITH fts_matched AS (
  SELECT cmd_path, row_number() OVER (ORDER BY bm25(apps_fts, 0.0, 5.0, 2.0) ASC) as fts_pos
  FROM apps_fts WHERE apps_fts MATCH :cand LIMIT 300
),
fts_ordered AS (
  SELECT arg.app_id, MIN(m.fts_pos) as fts_pos
  FROM fts_matched m JOIN arguments arg ON m.cmd_path = arg.cmd_path GROUP BY arg.app_id
),
vec_knn AS (SELECT cmd_path, distance FROM commands_vec WHERE embedding MATCH :qv AND k = 200),
vec_rank AS (
  SELECT arg.app_id, row_number() OVER (ORDER BY vk.distance ASC) as vec_pos
  FROM vec_knn vk JOIN arguments arg ON vk.cmd_path = arg.cmd_path WHERE arg.node_type='root'
),
pre_scored AS (
  SELECT COALESCE(fts.app_id, vec.app_id) as app_id, fts.fts_pos as fts_pos, vec.vec_pos as vec_pos
  FROM (SELECT app_id FROM fts_ordered UNION SELECT app_id FROM vec_rank) u
  LEFT JOIN fts_ordered fts ON u.app_id = fts.app_id
  LEFT JOIN vec_rank vec ON u.app_id = vec.app_id
),
pop_ranked AS (
  SELECT ps.app_id, ps.fts_pos, ps.vec_pos, a.name as nm, COALESCE(a.popularity,0.0) as pop
  FROM pre_scored ps JOIN apps a ON ps.app_id = a.app_id
),
scored AS (
  SELECT app_id, nm,
    COALESCE((:cold_floor + (1.0-:cold_floor)*pop) * 1.0/(60.0+fts_pos), 0.0)
    + COALESCE(1.0/(60.0+vec_pos), 0.0)
    + :pw_lin*pop + :pw_cube*pop*pop*pop as rrf_score
  FROM pop_ranked
),
name_deduped AS (
  SELECT app_id, nm, rrf_score, row_number() OVER (PARTITION BY nm ORDER BY rrf_score DESC) as rn
  FROM scored
)
SELECT nm, rrf_score FROM name_deduped WHERE rn=1 ORDER BY rrf_score DESC LIMIT 30
"""


def stage1_apps(conn, qvec, cand, pw_lin, pw_cube, cold_floor):
    qb = struct.pack(f"{len(qvec)}f", *qvec)
    rows = conn.execute(
        STAGE1_SQL,
        {"cand": cand, "qv": qb, "pw_lin": pw_lin, "pw_cube": pw_cube, "cold_floor": cold_floor},
    ).fetchall()
    return [r[0] for r in rows]  # ordered app names


def name_tokens(nm):
    return {nm.lower(), nm.lower().split(".")[0]}


def eval_config(conn, sess, nm_map, vocab, cand_strategy, pw_lin, pw_cube, cold_floor):
    # Basket burial
    buried = 0
    for q, want in BASKET:
        cand, _ = build_cand(q, conn, cand_strategy)
        qv = embed(sess, nm_map, vocab, q)
        apps = stage1_apps(conn, qv, cand, pw_lin, pw_cube, cold_floor)
        rank = next((i + 1 for i, nm in enumerate(apps[:TOPK]) if nm in want), None)
        if rank is None:
            buried += 1
    # Golden guard (app-level: any top-5 app name in the accept list)
    g_found = 0
    for intent, accepts in GOLDEN:
        acc = {a.lower() for a in accepts}
        cand, _ = build_cand(intent, conn, cand_strategy)
        qv = embed(sess, nm_map, vocab, intent)
        apps = stage1_apps(conn, qv, cand, pw_lin, pw_cube, cold_floor)
        if any(name_tokens(nm) & acc for nm in apps[:TOPK]):
            g_found += 1
    return buried, g_found


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
    sess, nm_map = load_session()

    print(f"basket={len(BASKET)} golden={len(GOLDEN)} (Stage-1 top-{TOPK})\n")
    print(f"{'cand':9s} {'pw_lin':7s} {'pw_cube':8s} {'cold':5s}  buried/__  golden/__")
    configs = [
        # (cand_strategy, pw_lin, pw_cube, cold_floor)
        ("current", 0.0, 0.015, 1.0),   # baseline = db.rs today
        ("or_syn",  0.0, 0.015, 1.0),
        ("and_syn", 0.0, 0.015, 1.0),
        ("and_syn", 0.0, 0.030, 1.0),
        ("and_syn", 0.0, 0.030, 0.5),
        ("and_syn", 0.0, 0.050, 0.5),
        ("and_syn", 0.0, 0.050, 0.3),
        ("and_syn", 0.0, 0.080, 0.3),
        ("or_syn",  0.0, 0.050, 0.5),
        ("or_syn",  0.0, 0.080, 0.3),
    ]
    for cand, pl, pc, cf in configs:
        b, g = eval_config(conn, sess, nm_map, vocab, cand, pl, pc, cf)
        flag = "  <- baseline" if (cand, pl, pc, cf) == ("current", 0.0, 0.015, 1.0) else ""
        print(f"{cand:9s} {pl:<7.3f} {pc:<8.3f} {cf:<5.2f}  {b:2d}/{len(BASKET)}     {g:2d}/{len(GOLDEN)}{flag}")


if __name__ == "__main__":
    main()

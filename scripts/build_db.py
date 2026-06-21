#!/usr/bin/env python3
"""Build cmdhub.db from a PostgreSQL JSON export with real BGE-small-en-v1.5 embeddings.

Uses ProcessPoolExecutor (not threads) to bypass Python GIL for true CPU parallelism.
Each worker process independently loads vocab + ONNX model, tokenizes and infers.

Usage:
    uv run --with onnxruntime --with sqlite-vec python3 scripts/build_db.py \\
        --input /tmp/cmdhub_export.json \\
        --output /tmp/cmdhub.db \\
        [--model ~/.local/share/cmdhub/models/bge-small-en-v1.5.onnx] \\
        [--workers 8] \\
        [--batch-size 128] \\
        [--compress]
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
import sqlite3
import struct
import sys
import time
from concurrent.futures import ProcessPoolExecutor, as_completed
from pathlib import Path
from typing import Any


# ── Tokenizer ──────────────────────────────────────────────────────────────

VOCAB_GZ_PATH = str(Path(__file__).parent.parent / "cmdhub-cli/src/tokenizer/assets/vocab.txt.gz")
MAX_SEQ_LEN = 512
EMBED_DIM = 384  # BGE-small-en-v1.5 native output dim; no zero-padding


def _load_vocab(gz_path: str) -> dict[str, int]:
    with gzip.open(gz_path, "rt", encoding="utf-8") as f:
        return {w: i for i, w in enumerate(f.read().splitlines())}


def _preprocess(text: str) -> str:
    out: list[str] = []
    for c in text:
        cp = ord(c)
        if c.isascii() and (
            (33 <= cp < 48) or (58 <= cp < 65) or
            (91 <= cp < 97) or (123 <= cp < 127)
        ):
            out.extend((" ", c, " "))
        else:
            out.append(c)
    return "".join(out).lower()


def _tokenize_word(vocab: dict[str, int], word: str) -> list[int]:
    if not word:
        return []
    if word in vocab:
        return [vocab[word]]
    chars = list(word)
    start = 0
    sub: list[int] = []
    while start < len(chars):
        end = len(chars)
        found: tuple[int, int] | None = None
        while start < end:
            s = "".join(chars[start:end])
            lk = s if start == 0 else f"##{s}"
            if lk in vocab:
                found = (vocab[lk], end)
                break
            end -= 1
        if found is None:
            return [100]  # [UNK]
        sub.append(found[0])
        start = found[1]
    return sub


def _tokenize(vocab: dict[str, int], text: str) -> tuple[list[int], list[int]]:
    ids = [101]
    for word in _preprocess(text).split():
        ids.extend(_tokenize_word(vocab, word))
    ids.append(102)
    mask = [1] * len(ids)
    if len(ids) > MAX_SEQ_LEN:
        ids = ids[:MAX_SEQ_LEN]
        mask = mask[:MAX_SEQ_LEN]
    else:
        pad = MAX_SEQ_LEN - len(ids)
        ids += [0] * pad
        mask += [0] * pad
    return ids, mask


def _vec_to_bytes(vec: list[float]) -> bytes:
    """Pack model output (384-dim) as LE float32 bytes (EMBED_DIM * 4 bytes)."""
    return struct.pack(f"{EMBED_DIM}f", *vec[:EMBED_DIM])


# ── Worker (runs in subprocess, no shared state) ───────────────────────────

def _worker(
    vocab_gz: str,
    model_path: str,
    chunk: list[tuple[int, str]],  # (original_idx, text-to-embed)
    batch_size: int,
    providers: list[str] | None = None,
) -> list[tuple[int, bytes]]:
    """Subprocess worker: load vocab + ONNX, tokenize, infer, return [(idx, bytes)].

    With providers=['CUDAExecutionProvider', ...] this runs on the GPU in a single
    process (much faster, far less system RAM than N CPU processes).
    """
    import numpy as np
    import onnxruntime as ort

    vocab = _load_vocab(vocab_gz)

    opts = ort.SessionOptions()
    opts.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    opts.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
    if providers and any("CUDA" in p or "Tensorrt" in p for p in providers):
        # GPU: let ORT use as many CPU threads as it wants for tokenize-adjacent ops.
        sess = ort.InferenceSession(model_path, sess_options=opts, providers=providers)
        active = sess.get_providers()
        if not any("CUDA" in p or "Tensorrt" in p for p in active):
            raise RuntimeError(f"GPU requested but ORT fell back to {active}")
    else:
        opts.intra_op_num_threads = 2  # 2 ORT threads per CPU process; N×2 ≤ core count
        sess = ort.InferenceSession(model_path, sess_options=opts)

    # Map input names
    name_map: dict[str, str] = {}
    for inp in sess.get_inputs():
        nl = inp.name.lower().replace("_", "").replace("-", "")
        if nl == "inputids":
            name_map["input_ids"] = inp.name
        elif nl == "attentionmask":
            name_map["attention_mask"] = inp.name
        elif nl == "tokentypeids":
            name_map["token_type_ids"] = inp.name

    results: list[tuple[int, bytes]] = []

    for start in range(0, len(chunk), batch_size):
        sub = chunk[start : start + batch_size]
        ids_b: list[list[int]] = []
        mask_b: list[list[int]] = []
        for _, desc in sub:
            ids, mask = _tokenize(vocab, desc)
            ids_b.append(ids)
            mask_b.append(mask)

        ids_arr = np.array(ids_b, dtype=np.int64)
        mask_arr = np.array(mask_b, dtype=np.int64)
        feeds: dict[str, Any] = {
            name_map["input_ids"]: ids_arr,
            name_map["attention_mask"]: mask_arr,
        }
        if "token_type_ids" in name_map:
            feeds[name_map["token_type_ids"]] = np.zeros_like(ids_arr)

        output = sess.run(None, feeds)[0]  # (B, seq_len, D) or (B, D)
        if output.ndim == 3:
            exp = mask_arr.astype(np.float32)[:, :, None]
            pooled = (output * exp).sum(axis=1) / np.maximum(
                mask_arr.sum(axis=1, keepdims=True), 1e-9
            )
        else:
            pooled = output
        norms = np.linalg.norm(pooled, axis=1, keepdims=True)
        normed = pooled / np.maximum(norms, 1e-9)

        for (orig_idx, _), vec_row in zip(sub, normed.tolist()):
            results.append((orig_idx, _vec_to_bytes(vec_row)))

    return results


# ── SQLite schema ───────────────────────────────────────────────────────────

_DDL = """\
CREATE TABLE IF NOT EXISTS apps (
    app_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    os_aliases TEXT,
    install_instructions TEXT,
    popularity REAL DEFAULT 0.0
);
CREATE TABLE IF NOT EXISTS arguments (
    cmd_path TEXT PRIMARY KEY,
    app_id TEXT NOT NULL,
    node_name TEXT NOT NULL,
    node_type TEXT NOT NULL,
    description TEXT NOT NULL,
    risk_level TEXT NOT NULL,
    example_template TEXT,
    docker_image TEXT,
    script_url TEXT,
    source_url TEXT,
    topics TEXT,
    provenance TEXT NOT NULL DEFAULT 'inferred',
    FOREIGN KEY(app_id) REFERENCES apps(app_id) ON DELETE CASCADE
);
CREATE VIRTUAL TABLE IF NOT EXISTS apps_fts USING fts5(
    cmd_path UNINDEXED,
    name,
    capabilities
);
CREATE VIRTUAL TABLE IF NOT EXISTS commands_vec USING vec0(
    cmd_path TEXT PRIMARY KEY,
    embedding float[384]
);
CREATE TABLE IF NOT EXISTS sync_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"""


def _init_db(path: str) -> sqlite3.Connection:
    import sqlite_vec

    conn = sqlite3.connect(path, check_same_thread=False)
    conn.enable_load_extension(True)
    sqlite_vec.load(conn)
    conn.enable_load_extension(False)
    conn.execute("PRAGMA journal_mode = WAL")
    conn.execute("PRAGMA synchronous = NORMAL")
    conn.execute("PRAGMA foreign_keys = ON")
    conn.execute("PRAGMA cache_size = -65536")
    for stmt in _DDL.strip().split(";"):
        stmt = stmt.strip()
        if stmt:
            conn.execute(stmt)
    conn.commit()
    return conn


# ── Main ────────────────────────────────────────────────────────────────────

_NOISE_LEAVES = {"help", "version", "completion", "completions", "bash", "zsh",
                 "fish", "powershell", "--help", "-h", "--version"}


def _is_noise_command(cmd_path: str) -> bool:
    """True for non-root help/version/completion commands worth excluding from search."""
    if "." not in cmd_path:
        return False  # keep all root commands
    parts = cmd_path.split(".")
    leaf = parts[-1].lower()
    if leaf in _NOISE_LEAVES:
        return True
    # shell-completion subtrees: anything under a `.completion(s).` path
    return "completion" in parts[1:] or "completions" in parts[1:]


# ── Canonicalization & cross-source dedup ───────────────────────────────────
# Crawled package names often encode a SUBCOMMAND as a fused tool name
# ("podman-images" is really `podman images`), and the same tool arrives from
# several sources under different app_ids (io.podman.*, com.github.*, org.archlinux.*).
# We merge only unambiguous duplicates: same canonical tool + same leaf command.
# Probe-verified rows win over LLM-inferred ones; dropped rows donate their topics.

# Compound tools that look like "<base>-<word>" but are SEPARATE binaries, not a
# fused subcommand of <base>. Conservative blocklist — extend as real data demands.
_COMPOUND_TOOLS = {"compose", "machine", "buildx", "swarm", "creds", "desktop", "cli"}
# Fused-subcommand suffixes seen in crawled package names: "podman-image(s)" == podman image.
_FUSED_SUFFIXES = ("-image", "-images", "-container", "-containers", "-volume", "-volumes",
                   "-network", "-networks", "-pod", "-pods", "-system")


def _canonical_tool(root: str) -> str:
    """Canonical binary name for a command's ROOT segment. Collapses crawled
    fused-subcommand names (podman-images -> podman) but never compound tools
    (podman-compose stays podman-compose)."""
    r = root.strip().lower()
    if "-" in r and r.split("-", 1)[1] in _COMPOUND_TOOLS:
        return r  # compound tool: keep distinct
    for suf in _FUSED_SUFFIXES:
        if r.endswith(suf):
            return r[: -len(suf)]
    return r


def _clean_cmd_path(cmd_path: str) -> str:
    """Clean dirty command paths (spaces -> dots, docker_compose -> docker.compose, strip flags)."""
    # 1. Replace docker_compose variants
    path = cmd_path.replace("docker_compose_up", "docker.compose.up")
    path = path.replace("docker_compose.up_build", "docker.compose.up")
    path = path.replace("docker_compose.up_detached", "docker.compose.up")
    path = path.replace("docker_compose.up_project", "docker.compose.up")
    path = path.replace("docker_compose.logs_container", "docker.compose.logs")
    path = path.replace("docker_compose", "docker.compose")

    # 2. Split by whitespace and discard any segment starting with '-' (and all subsequent segments)
    parts = path.split()
    cleaned_parts = []
    for p in parts:
        if p.startswith("-"):
            break
        cleaned_parts.append(p)
    path = ".".join(cleaned_parts)

    # 3. Collapse multiple consecutive dots and strip leading/trailing dots
    while ".." in path:
        path = path.replace("..", ".")
    path = path.strip(".")

    return path


def _node_type_for_path(cmd_path: str) -> str:
    """Reconcile node_type with the dot-presence invariant that validate_db enforces.

    A cleaned path carrying a dot is a subcommand (e.g. the dirty root `arc amend`
    becomes `arc.amend`); one collapsed to the bare binary is a root (`beep -r` →
    `beep`). `_clean_cmd_path` only rewrites the path string, so without this the
    rewritten rows keep their stale node_type and trip `node_type_invariant`."""
    return "sub" if "." in cmd_path else "root"


def _canonical_path(cmd_path: str) -> str:
    """Canonical identity of a command across crawl sources. A fused-subcommand root is
    unfolded into a real path segment (podman-image.prune → podman.image.prune) and
    segments are singular-stemmed for the KEY ONLY (podman.images ~ podman.image — real
    CLIs alias these). Compound tools (podman-compose) are preserved verbatim, and the
    full path keeps distinct subcommands apart (image.prune ≠ container.prune)."""
    parts = cmd_path.lower().split(".")
    root, rest = parts[0], parts[1:]
    canon_root = _canonical_tool(root)
    if canon_root != root:
        # The stripped fused suffix becomes a real path segment ("podman-images" → "images").
        rest = [root[len(canon_root) + 1:]] + rest
    def stem(w: str) -> str:
        return w[:-1] if w.endswith("s") and len(w) > 3 else w
    return ".".join([canon_root] + [stem(w) for w in rest])


def _canonicalize_and_dedup(arguments: list[dict], apps: list[dict]) -> list[dict]:
    """Merge unambiguous cross-source duplicates: identical canonical FULL path.
    Keep the probe row (else highest-popularity); union the dropped rows' topics into
    the kept row so search recall survives the merge. Runs BEFORE embedding so vectors
    are computed on the final merged text. Conservative by design: distinct
    subcommands / distinct tools are never merged."""
    pop = {a["app_id"]: float(a.get("popularity") or 0.0) for a in apps}
    name_by_app = {a["app_id"]: (a.get("name") or "").lower() for a in apps}
    # Commands per app across the whole registry — used as a tie-break, and to find the
    # consolidated tool for root re-anchoring below.
    tree_size: dict[str, int] = {}
    for a in arguments:
        tree_size[a["app_id"]] = tree_size.get(a["app_id"], 0) + 1

    # ── Strict root re-anchoring ────────────────────────────────────────────
    # A tool's ROOT row (cmd_path == binary) may have been imported under a near-empty
    # namesake app while the CONSOLIDATED tool (its whole subcommand tree) lives under a
    # different app that has NO root row — e.g. `podman` root @com.example.podman (2 cmds)
    # but podman.image.prune & 108 more @org.cmdhub.podman. Stage-1 selects one app per
    # name (by match score), so the namesake wins and the real tool's subcommands never
    # surface. Re-anchor the root to the dominant same-name app — but ONLY when that app
    # is OVERWHELMINGLY richer (>=20 cmds AND >=5x the current owner). This strict gate
    # fires for clear consolidations (~18 major tools) and never for tools without a
    # dominant variant (curl/wget/tar each have ~1 cmd) — which is what made the earlier
    # tree-size-first version regress. Popularity is unaffected (these ties are all ~1.0).
    richest_for_name: dict[str, str] = {}
    for app_id, n in name_by_app.items():
        if n and (n not in richest_for_name or tree_size.get(app_id, 0) > tree_size.get(richest_for_name[n], 0)):
            richest_for_name[n] = app_id
    reanchored = 0
    for a in arguments:
        if "." in a["cmd_path"]:
            continue
        n = a["cmd_path"].lower()
        if name_by_app.get(a["app_id"]) != n:
            continue  # only move a root among apps that share its exact name
        target = richest_for_name.get(n)
        if target and target != a["app_id"]:
            t_tree, o_tree = tree_size.get(target, 0), tree_size.get(a["app_id"], 0)
            if t_tree >= 20 and t_tree >= 5 * max(o_tree, 1):
                a["app_id"] = target
                reanchored += 1
    if reanchored:
        print(f"[build-db] Re-anchored {reanchored} roots to their consolidated tool app",
              file=sys.stderr, flush=True)

    groups: dict[str, list[dict]] = {}
    for a in arguments:
        groups.setdefault(_canonical_path(a["cmd_path"]), []).append(a)

    out: list[dict] = []
    merged = 0
    for canon, rows in groups.items():
        if len(rows) == 1:
            out.append(rows[0])
            continue
        # Keep preference (popularity stays PRIMARY so this never displaces a high-pop
        # owner — that was the v3 regression): probe-verified first; then the row whose
        # OWN path already is the canonical one (a fused-root fragment like
        # `podman-image.prune` names a non-existent binary, breaking `cmdh run`); then
        # highest popularity; then — only to break a popularity TIE (e.g. many podman
        # apps all capped at 1.0) — the app with the richest command tree, i.e. the
        # consolidated real tool (so the `podman` root lands on org.cmdhub.podman with
        # 110 cmds, not the 2-command namesake, which lets Stage-1 select it); then
        # app_id for determinism.
        rows.sort(key=lambda r: (
            r.get("provenance") != "probe",
            r["cmd_path"].lower() != canon,
            -pop.get(r["app_id"], 0.0),
            -tree_size.get(r["app_id"], 0),
            r["app_id"],
        ))
        keep = rows[0]
        topic_set: list[str] = []
        for r in rows:
            for t in (r.get("topics") or "").split():
                if t not in topic_set:
                    topic_set.append(t)
        keep["topics"] = " ".join(topic_set)
        out.append(keep)
        merged += len(rows) - 1
    if merged:
        print(f"[build-db] Canonicalized/merged {merged} duplicate commands",
              file=sys.stderr, flush=True)
    return out


def _default_overrides_path() -> str:
    """Repo-local default: <repo>/data/search_overrides.json. Override via
    CMDHUB_BUILD_OVERRIDES env or the --overrides CLI flag (cloud-native injection)."""
    return str(Path(__file__).resolve().parent.parent / "data" / "search_overrides.json")


def _apply_overrides(arguments: list[dict], apps: list[dict], overrides_path: str | None) -> list[dict]:
    """Apply build-time search-quality overrides before embedding.

    Applied after dedup/noise-filtering and before FTS + vector construction, so the
    corrected description/topics flow into BOTH indexes. This is the durable home for
    golden-eval fixes (see scripts/eval_golden.py) — patching the live SQLite directly
    is lost on the next rebuild, but these overrides re-apply every build.
    """
    if not overrides_path or not os.path.exists(overrides_path):
        return arguments
    with open(overrides_path, encoding="utf-8") as f:
        ov = json.load(f)

    by_path = {a["cmd_path"]: a for a in arguments}
    app_ids = {a["app_id"] for a in apps}

    n_patch = 0
    for cmd_path, patch in ov.get("patch", {}).items():
        a = by_path.get(cmd_path)
        if a is None:
            continue
        if "description" in patch:
            a["description"] = patch["description"]
        if "topics" in patch:
            a["topics"] = patch["topics"]
        if "topics_append" in patch:
            a["topics"] = ((a.get("topics") or "") + " " + patch["topics_append"]).strip()
        n_patch += 1

    n_add = 0
    for rec in ov.get("add", []):
        cp = rec["cmd_path"]
        if cp in by_path:
            continue
        # Keep FK integrity: only add commands whose app already exists.
        if rec.get("app_id") not in app_ids:
            print(f"[build-db] override add skipped (app missing): {cp}", file=sys.stderr, flush=True)
            continue
        for k in ("example_template", "docker_image", "script_url", "source_url", "topics"):
            rec.setdefault(k, None)
        arguments.append(rec)
        by_path[cp] = rec
        n_add += 1

    if n_patch or n_add:
        print(f"[build-db] Applied overrides from {overrides_path}: "
              f"patched {n_patch}, added {n_add}", file=sys.stderr, flush=True)
    return arguments


def _embed_text(arg: dict) -> str:
    """Text fed to the embedder: the command path (as words) + its description.

    Embedding the path too means a query like "configure networking aws" matches
    "aws ec2 create-vpc" via the path tokens, not the description alone — and it
    spreads out otherwise-identical descriptions (e.g. many "Display help.").
    """
    path_words = arg["cmd_path"].replace(".", " ").replace("-", " ")
    desc = arg.get("description") or ""
    topics = arg.get("topics") or ""  # LLM tags (azure, networking, ...) sharpen recall
    return f"{path_words}. {desc} {topics}".strip()


# Keyword heuristic for risk_level (replaces per-command LLM judgement for the bulk).
# A destructive/state-changing verb in the command path or description bumps the level.
_DANGEROUS_KW = (
    "delete", "destroy", "terminate", "remove", "rm ", "drop", "purge", "wipe",
    "deregister", "revoke", "kill", "force-delete", "shutdown", "rmdir",
)
_MEDIUM_KW = (
    "create", "update", "modify", "put", "set", "apply", "deploy", "restart",
    "reboot", "scale", "attach", "detach", "rotate", "reset", "disable", "enable",
    "stop", "start", "prune", "push", "import", "restore", "rollback", "patch",
    "associate", "disassociate", "register", "add", "remove-",
)


def _risk_level(cmd_path: str, description: str, existing: str | None) -> str:
    """Derive risk from verbs in the leaf command + description. Keeps any explicit
    non-safe level already set (e.g. by an LLM pass); only upgrades from 'safe'/None."""
    if existing and existing not in ("safe", "", None):
        return existing
    leaf = cmd_path.split(".")[-1].lower()
    hay = f"{leaf} {description.lower()}"
    if any(k in hay for k in _DANGEROUS_KW):
        return "dangerous"
    if any(k in hay for k in _MEDIUM_KW):
        return "medium"
    return "safe"


def build(
    export_path: str,
    output_path: str,
    model_path: str,
    workers: int,
    batch_size: int,
    compress: bool,
    device: str = "cpu",
    overrides_path: str | None = None,
) -> None:
    t0 = time.time()

    print(f"[build-db] Loading {export_path}", file=sys.stderr, flush=True)
    with open(export_path, encoding="utf-8") as f:
        data = json.load(f)
    apps: list[dict] = data["apps"]
    raw_args: list[dict] = data["arguments"]

    # Deduplicate by cmd_path, and drop noise subcommands (help/version/completion):
    # these have near-identical descriptions across thousands of tools, are never the
    # target of a search, and only dilute the vector space. Root commands are kept.
    seen: set[str] = set()
    arguments: list[dict] = []
    noise = 0
    for a in raw_args:
        a["cmd_path"] = _clean_cmd_path(a["cmd_path"])
        cp = a["cmd_path"]
        a["node_type"] = _node_type_for_path(cp)
        if cp in seen:
            continue
        seen.add(cp)
        if _is_noise_command(cp):
            noise += 1
            continue
        arguments.append(a)
    dup = len(raw_args) - len(seen)
    if dup:
        print(f"[build-db] Dropped {dup} duplicate cmd_paths", file=sys.stderr, flush=True)
    if noise:
        print(f"[build-db] Dropped {noise} help/version/completion noise commands", file=sys.stderr, flush=True)
    # Apply build-time overrides before embedding so corrected text reaches FTS + vectors.
    arguments = _apply_overrides(arguments, apps, overrides_path)
    # Merge cross-source duplicates BEFORE embedding so vectors reflect merged text (§4.3).
    arguments = _canonicalize_and_dedup(arguments, apps)

    total = len(arguments)
    print(f"[build-db] {len(apps)} apps, {total} arguments", file=sys.stderr, flush=True)

    emb_results: dict[int, bytes] = {}
    completed = 0

    if device == "cuda":
        # Single-process GPU path: one CUDA session, no ProcessPool. Fast + low system RAM.
        print(f"[build-db] Embedding on GPU (CUDAExecutionProvider), batch_size={batch_size}",
              file=sys.stderr, flush=True)
        chunk = [(i, _embed_text(arguments[i])) for i in range(total)]
        providers = ["CUDAExecutionProvider", "CPUExecutionProvider"]
        for orig_idx, emb_bytes in _worker(VOCAB_GZ_PATH, model_path, chunk, batch_size, providers):
            emb_results[orig_idx] = emb_bytes
        completed = total
        rate = completed / (time.time() - t0) if time.time() > t0 else 0
        print(f"[build-db] {completed}/{total} (100%) — {rate:.0f}/s (GPU)", file=sys.stderr, flush=True)
    else:
        # Partition into CPU worker chunks
        chunk_size = max(1, (total + workers - 1) // workers)
        chunks: list[list[tuple[int, str]]] = []
        for w in range(workers):
            s = w * chunk_size
            e = min(s + chunk_size, total)
            if s < total:
                chunks.append([(i, _embed_text(arguments[i])) for i in range(s, e)])

        actual_workers = len(chunks)
        print(
            f"[build-db] Embedding: {actual_workers} CPU processes × batch_size={batch_size}",
            file=sys.stderr, flush=True,
        )

        with ProcessPoolExecutor(max_workers=actual_workers) as pool:
            futs = {
                pool.submit(_worker, VOCAB_GZ_PATH, model_path, chunk, batch_size): i
                for i, chunk in enumerate(chunks)
            }
            for fut in as_completed(futs):
                try:
                    chunk_res = fut.result()
                except Exception as exc:
                    print(f"\n[error] Worker failed: {exc}", file=sys.stderr, flush=True)
                    raise
                for orig_idx, emb_bytes in chunk_res:
                    emb_results[orig_idx] = emb_bytes
                completed += len(chunk_res)
                pct = completed * 100 // total
                elapsed = time.time() - t0
                rate = completed / elapsed if elapsed > 0 else 0
                eta = int((total - completed) / rate) if rate > 0 else 0
                print(
                    f"[build-db] {completed}/{total} ({pct}%) — {rate:.0f}/s — ETA {eta}s",
                    file=sys.stderr, flush=True,
                )

    t_embed = time.time() - t0
    print(f"[build-db] Embedding done in {t_embed:.1f}s", file=sys.stderr, flush=True)

    # Write SQLite
    print(f"[build-db] Writing {output_path}", file=sys.stderr, flush=True)
    if os.path.exists(output_path):
        os.remove(output_path)
    conn = _init_db(output_path)

    def strip_json_nulls(s: str | None) -> str | None:
        """Remove null/empty-valued keys from JSON string so Rust serde can parse it."""
        if not s:
            return s
        try:
            obj = json.loads(s)
            if isinstance(obj, dict):
                cleaned = {k: v for k, v in obj.items() if v is not None and v != ""}
                return json.dumps(cleaned) if cleaned else None
        except Exception:
            pass
        return s

    app_name_map = {a["app_id"]: a["name"] for a in apps}

    def _fts_name(app_id: str, binary_name: str) -> str:
        """Return FTS name field: 'binary_name pkg_alias' when they differ.

        For arch packages: 'org.archlinux.antigravity-cli' → pkg alias 'antigravity-cli'.
        If pkg alias == binary name, just return the binary name (no duplication).
        """
        parts = app_id.rsplit(".", 1)
        if len(parts) != 2:
            return binary_name
        pkg_alias = parts[1]
        if pkg_alias == binary_name or pkg_alias in binary_name:
            return binary_name
        return f"{binary_name} {pkg_alias}"

    conn.executemany(
        "INSERT OR REPLACE INTO apps VALUES (?,?,?,?,?)",
        [
            (
                a["app_id"], a["name"],
                strip_json_nulls(a.get("os_aliases")),
                strip_json_nulls(a.get("install_instructions")),
                float(a.get("popularity") or 0.0),
            )
            for a in apps
        ],
    )

    CHUNK = 500
    for start in range(0, total, CHUNK):
        batch = arguments[start : start + CHUNK]
        conn.executemany(
            "INSERT OR REPLACE INTO arguments "
            "(cmd_path,app_id,node_name,node_type,description,risk_level,"
            " example_template,docker_image,script_url,source_url,topics,provenance) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
            [
                (
                    a["cmd_path"], a["app_id"], a["node_name"],
                    # Enforce structural invariant from cmd_path shape
                    "root" if "." not in a["cmd_path"] else "sub",
                    a["description"],
                    _risk_level(a["cmd_path"], a.get("description") or "", a.get("risk_level")),
                    a.get("example_template"),
                    a.get("docker_image"), a.get("script_url"), a.get("source_url"),
                    a.get("topics"),
                    a.get("provenance") or "inferred",
                )
                for a in batch
            ],
        )
        conn.executemany(
            "DELETE FROM apps_fts WHERE cmd_path = ?",
            [(a["cmd_path"],) for a in batch],
        )
        conn.executemany(
            "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES (?,?,?)",
            [(a["cmd_path"],
              _fts_name(a["app_id"], app_name_map.get(a["app_id"], a["app_id"])),
              # capabilities = path words + description + LLM topics → search matches
              # "vpc"→create-vpc and brand/concept words ("azure"→az) via the topic tags
              f"{a['cmd_path'].replace('.', ' ').replace('-', ' ')} {a.get('description') or ''} {a.get('topics') or ''}")
             for a in batch],
        )
        conn.executemany(
            "DELETE FROM commands_vec WHERE cmd_path = ?",
            [(a["cmd_path"],) for a in batch],
        )
        conn.executemany(
            "INSERT INTO commands_vec (cmd_path, embedding) VALUES (?,?)",
            [(a["cmd_path"], emb_results[start + j]) for j, a in enumerate(batch)],
        )
        conn.commit()
        pct = min(start + CHUNK, total) * 100 // total
        print(
            f"[build-db] Written {min(start + CHUNK, total)}/{total} ({pct}%)",
            file=sys.stderr, flush=True,
        )

    # Merge install_instructions: copy arch/tldr source into same-named github/gitlab apps that
    # have none. Official sources have richer probe data but may lack package manager entries.
    merged = conn.execute("""
        UPDATE apps SET install_instructions = (
            SELECT b.install_instructions FROM apps b
            WHERE b.name = apps.name
              AND b.app_id != apps.app_id
              AND b.install_instructions IS NOT NULL
              AND length(b.install_instructions) > 2
            ORDER BY CASE
                WHEN b.app_id LIKE 'org.archlinux.%' THEN 1
                WHEN b.app_id LIKE 'org.tldr.%' THEN 2
                ELSE 0
            END ASC
            LIMIT 1
        )
        WHERE (install_instructions IS NULL OR length(install_instructions) <= 2)
          AND (app_id LIKE 'com.github.%' OR app_id LIKE 'com.gitlab.%')
    """).rowcount
    conn.commit()
    print(f"[build-db] Merged install_instructions into {merged} official-source apps", file=sys.stderr, flush=True)

    conn.execute(
        "INSERT OR REPLACE INTO sync_meta (key, value) VALUES ('schema_version', '2')",
    )
    conn.execute(
        "INSERT OR REPLACE INTO sync_meta (key, value) VALUES ('last_sync_time', ?)",
        (str(int(time.time())),),
    )
    conn.commit()
    conn.close()

    elapsed = time.time() - t0
    db_size = os.path.getsize(output_path) / 1_048_576
    print(
        f"[build-db] Done in {elapsed:.1f}s — DB size: {db_size:.1f} MB",
        file=sys.stderr, flush=True,
    )

    if compress:
        _compress(output_path, apps, total)


def _compress(db_path: str, apps: list[dict], total: int) -> None:
    try:
        import zstandard as zstd
    except ImportError:
        print("[warn] zstandard not installed, skipping compression", file=sys.stderr, flush=True)
        return

    zst_path = db_path + ".zst"
    print(f"[build-db] Compressing → {zst_path} (level 19)", file=sys.stderr, flush=True)
    with open(db_path, "rb") as f:
        db_bytes = f.read()
    compressed = zstd.ZstdCompressor(level=19).compress(db_bytes)
    sha256 = hashlib.sha256(compressed).hexdigest()
    with open(zst_path, "wb") as f:
        f.write(compressed)
    manifest = {
        "sha256": sha256,
        "size_bytes": len(compressed),
        "app_count": len(apps),
        "command_count": total,
        "built_at": __import__("datetime").datetime.now(__import__("datetime").timezone.utc).isoformat(),
    }
    manifest_path = db_path + ".manifest.json"
    with open(manifest_path, "w") as f:
        json.dump(manifest, f, indent=2)
    print(f"[build-db] Compressed: {len(compressed)/1e6:.1f} MB  SHA-256: {sha256}", file=sys.stderr, flush=True)


def main() -> None:
    ap = argparse.ArgumentParser(description="Build cmdhub.db with real BGE-small-en-v1.5 embeddings")
    ap.add_argument("--input", "-i", required=True)
    ap.add_argument("--output", "-o", default="/tmp/cmdhub.db")
    ap.add_argument(
        "--model", "-m",
        default=os.path.expanduser("~/.local/share/cmdhub/models/bge-small-en-v1.5.onnx"),
    )
    ap.add_argument("--workers", "-w", type=int, default=8)
    ap.add_argument("--batch-size", type=int, default=128)
    ap.add_argument("--device", choices=["cpu", "cuda"], default="cpu",
                    help="cuda = single-process GPU embedding (fast, low system RAM)")
    ap.add_argument("--compress", action="store_true")
    ap.add_argument(
        "--overrides",
        default=os.environ.get("CMDHUB_BUILD_OVERRIDES", _default_overrides_path()),
        help="Build-time search-quality overrides JSON (applied before embedding). "
             "Pass '' to disable. Env: CMDHUB_BUILD_OVERRIDES.",
    )
    args = ap.parse_args()

    if not os.path.exists(args.model):
        print(f"[error] ONNX model not found: {args.model}", file=sys.stderr, flush=True)
        sys.exit(1)

    build(
        export_path=args.input,
        output_path=args.output,
        model_path=args.model,
        workers=args.workers,
        batch_size=args.batch_size,
        compress=args.compress,
        device=args.device,
        overrides_path=args.overrides or None,
    )


if __name__ == "__main__":
    main()

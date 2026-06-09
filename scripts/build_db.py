#!/usr/bin/env python3
"""Build cmdhub.db from a PostgreSQL JSON export with real BGE-micro-v2 embeddings.

Uses ProcessPoolExecutor (not threads) to bypass Python GIL for true CPU parallelism.
Each worker process independently loads vocab + ONNX model, tokenizes and infers.

Usage:
    uv run --with onnxruntime --with sqlite-vec python3 scripts/build_db.py \\
        --input /tmp/cmdhub_export.json \\
        --output /tmp/cmdhub.db \\
        [--model ~/.local/share/cmdhub/models/bge-micro-v2.onnx] \\
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
EMBED_DIM = 512  # storage dim (model output is 384, padded to 512)


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
    """Pack model output (384-dim) padded to 512 floats as LE f32 blob."""
    padded = vec[:EMBED_DIM] + [0.0] * max(0, EMBED_DIM - len(vec))
    return struct.pack(f"{EMBED_DIM}f", *padded)


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
    install_instructions TEXT
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
    FOREIGN KEY(app_id) REFERENCES apps(app_id) ON DELETE CASCADE
);
CREATE VIRTUAL TABLE IF NOT EXISTS apps_fts USING fts5(
    cmd_path UNINDEXED,
    name,
    capabilities
);
CREATE VIRTUAL TABLE IF NOT EXISTS commands_vec USING vec0(
    cmd_path TEXT PRIMARY KEY,
    embedding float[512]
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


def _embed_text(arg: dict) -> str:
    """Text fed to the embedder: the command path (as words) + its description.

    Embedding the path too means a query like "configure networking aws" matches
    "aws ec2 create-vpc" via the path tokens, not the description alone — and it
    spreads out otherwise-identical descriptions (e.g. many "Display help.").
    """
    path_words = arg["cmd_path"].replace(".", " ").replace("-", " ")
    desc = arg.get("description") or ""
    return f"{path_words}. {desc}".strip()


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
        cp = a["cmd_path"]
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
        "INSERT OR REPLACE INTO apps VALUES (?,?,?,?)",
        [
            (
                a["app_id"], a["name"],
                strip_json_nulls(a.get("os_aliases")),
                strip_json_nulls(a.get("install_instructions")),
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
            " example_template,docker_image,script_url,source_url) VALUES (?,?,?,?,?,?,?,?,?,?)",
            [
                (
                    a["cmd_path"], a["app_id"], a["node_name"],
                    # Enforce structural invariant from cmd_path shape
                    "root" if "." not in a["cmd_path"] else "sub",
                    a["description"],
                    _risk_level(a["cmd_path"], a.get("description") or "", a.get("risk_level")),
                    a.get("example_template"),
                    a.get("docker_image"), a.get("script_url"), a.get("source_url"),
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
              # capabilities = path words + description so keyword search hits e.g. "vpc"→create-vpc
              f"{a['cmd_path'].replace('.', ' ').replace('-', ' ')} {a.get('description') or ''}")
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
        "built_at": __import__("datetime").datetime.utcnow().isoformat() + "Z",
    }
    manifest_path = db_path + ".manifest.json"
    with open(manifest_path, "w") as f:
        json.dump(manifest, f, indent=2)
    print(f"[build-db] Compressed: {len(compressed)/1e6:.1f} MB  SHA-256: {sha256}", file=sys.stderr, flush=True)


def main() -> None:
    ap = argparse.ArgumentParser(description="Build cmdhub.db with real BGE-micro-v2 embeddings")
    ap.add_argument("--input", "-i", required=True)
    ap.add_argument("--output", "-o", default="/tmp/cmdhub.db")
    ap.add_argument(
        "--model", "-m",
        default=os.path.expanduser("~/.local/share/cmdhub/models/bge-micro-v2.onnx"),
    )
    ap.add_argument("--workers", "-w", type=int, default=8)
    ap.add_argument("--batch-size", type=int, default=128)
    ap.add_argument("--device", choices=["cpu", "cuda"], default="cpu",
                    help="cuda = single-process GPU embedding (fast, low system RAM)")
    ap.add_argument("--compress", action="store_true")
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
    )


if __name__ == "__main__":
    main()

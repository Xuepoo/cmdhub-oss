#!/usr/bin/env python3
"""LLM-based correction of mismatched/suspected fabricated examples in cmdhub.db.

Uses OpenRouter (deepseek-v4-flash) with high concurrency to parse and correct
example templates that fail validate_db.py heuristics. DB writes are serialized
via a thread-safe Queue to prevent SQLite database lockups.

Usage:
    OPENROUTER_API_KEY=... python3 llm_fix_fabricated.py --db ~/.local/share/cmdhub/cmdhub.db [--workers 50]
"""
from __future__ import annotations

import argparse
import json
import os
import queue
import sqlite3
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import requests

SYS_PROMPT = (
    "You are a CLI command example formatting tool for an AI coding agent.\n"
    "Your goal is to correct a command's 'example_template' so it strictly follows the "
    "hierarchical registration in the database and passes validation rules.\n\n"
    "CRITICAL RULES:\n"
    "1. The example MUST begin with the dotted path parts separated by spaces (excluding sudo).\n"
    "   - If cmd_path is 'git.remote.add', example MUST start with 'git remote add'.\n"
    "   - If cmd_path is 'docker_compose.logs', example MUST start with 'docker_compose logs' (root must match exactly).\n"
    "2. Any word after the command path prefix that is a parameter value, variable, number, or argument "
    "   MUST be enclosed in double curly braces (e.g. {{value}}, {{name}}, {{seconds}}), UNLESS it is an option/flag starting with '-'.\n"
    "   - Example: 'nettop l 5' for 'nettop.l' is INVALID because '5' is treated as an unregistered subcommand. "
    "     It MUST be rewritten as 'nettop l {{interval}}' or 'nettop l -i {{interval}}'.\n"
    "   - Example: 'doctl compute droplet create' for 'doctl.compute' is INVALID because 'droplet' is not in the path. "
    "     It MUST be rewritten as 'doctl compute {{resource}} {{action}}' or 'doctl compute --help'.\n"
    "3. Keep the original semantic intent of the command. Make it a realistic, runnable one-line command template.\n"
    "4. Return ONLY a JSON block containing the corrected template:\n"
    "   {\"corrected_example\": \"<your_corrected_example>\"}\n"
    "   Do not output any explanations or extra markdown wrapping outside the JSON."
)

FEW_SHOT_SAMPLES = (
    "\n\nExamples:\n"
    "Input:\n"
    "  cmd_path: nettop.l\n"
    "  invalid_example: nettop l 5\n"
    "Output:\n"
    "  {\"corrected_example\": \"nettop l {{interval_seconds}}\"}\n\n"
    "Input:\n"
    "  cmd_path: doctl.compute\n"
    "  invalid_example: doctl compute droplet create --name my-droplet\n"
    "Output:\n"
    "  {\"corrected_example\": \"doctl compute {{resource}} {{action}} --name {{name}}\"}\n\n"
    "Input:\n"
    "  cmd_path: docker_compose.logs\n"
    "  invalid_example: docker compose logs --follow my-container\n"
    "Output:\n"
    "  {\"corrected_example\": \"docker_compose logs --follow {{container_name}}\"}\n"
)


def is_fabricated(cmd_path: str, ex: str) -> bool:
    """Returns True if the example violates validate_db.py heuristics."""
    ex = ex.strip()
    if not ex:
        return False
    tokens = ex.split()
    if tokens and tokens[0] == "sudo":
        tokens = tokens[1:]
    if not tokens:
        return False

    # 1. The example must invoke the contract's own binary (root segment).
    root = cmd_path.split(".", 1)[0].lower()
    first = tokens[0].lower()
    if first not in (root, f"./{root}"):
        return True

    # 2. For subcommand contracts, check if stray subcommands exist.
    if "." not in cmd_path:
        return False
    path_words = [seg.lower() for seg in cmd_path.split(".")[1:]]
    for i, t in enumerate(tokens[1:]):
        if t.startswith("-") or "{{" in t or "=" in t:
            break
        if i >= len(path_words) or path_words[i] != t.lower():
            return True
    return False


def call_llm(s: requests.Session, key: str, model: str, cmd_path: str, desc: str, invalid_ex: str) -> str | None:
    user_content = (
        f"cmd_path: {cmd_path}\n"
        f"description: {desc}\n"
        f"invalid_example: {invalid_ex}"
    )

    body = {
        "model": model,
        "temperature": 0.1,
        "messages": [
            {"role": "system", "content": SYS_PROMPT + FEW_SHOT_SAMPLES},
            {"role": "user", "content": user_content}
        ]
    }

    for attempt in range(4):
        try:
            r = s.post(
                "https://openrouter.ai/api/v1/chat/completions",
                headers={"Authorization": f"Bearer {key}"},
                json=body,
                timeout=45
            )
            if r.status_code == 429:
                time.sleep(2 * (attempt + 1))
                continue
            r.raise_for_status()
            res_text = r.json()["choices"][0]["message"]["content"].strip()

            # Parse JSON out of response
            start = res_text.find("{")
            end = res_text.rfind("}")
            if start != -1 and end != -1:
                obj = json.loads(res_text[start:end+1])
                corrected = obj.get("corrected_example")
                if corrected:
                    return corrected.strip()
            return None
        except Exception:
            time.sleep(1.5)
    return None


def db_writer(db_path: str, write_queue: queue.Queue, total_to_write: int, stop_event: threading.Event):
    """Single-threaded writer that serializes database updates."""
    conn = sqlite3.connect(db_path)
    # Enable WAL mode for concurrency and performance
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA synchronous=NORMAL")

    written = 0
    while not stop_event.is_set() or not write_queue.empty():
        try:
            item = write_queue.get(timeout=1.0)
        except queue.Empty:
            continue

        cmd_path, new_example = item
        try:
            conn.execute(
                "UPDATE arguments SET example_template = ? WHERE cmd_path = ?",
                (new_example, cmd_path)
            )
            # Re-index in FTS
            # FTS5 will need rebuilding eventually, but we keep it updated
            conn.execute(
                "UPDATE apps_fts SET capabilities = (SELECT description || ' ' || COALESCE(example_template, '') FROM arguments WHERE cmd_path = ?) WHERE cmd_path = ?",
                (cmd_path, cmd_path)
            )
            written += 1
            if written % 100 == 0:
                conn.commit()
                print(f"[db-writer] Saved {written}/{total_to_write} corrections", flush=True)
        except Exception as e:
            print(f"[db-writer] Error updating {cmd_path}: {e}", file=sys.stderr)
        finally:
            write_queue.task_done()

    conn.commit()
    conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    conn.close()
    print(f"[db-writer] Finished. Total updates saved: {written}", flush=True)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", default=os.path.expanduser("~/.local/share/cmdhub/cmdhub.db"))
    ap.add_argument("--workers", type=int, default=50)
    ap.add_argument("--model", default="deepseek/deepseek-v4-flash")
    args = ap.parse_args()

    key = os.environ.get("OPENROUTER_API_KEY", "")
    if not key:
        print("[error] Please set the OPENROUTER_API_KEY environment variable.", file=sys.stderr)
        sys.exit(1)

    db_path = Path(args.db).resolve()
    if not db_path.exists():
        print(f"[error] SQLite DB not found at: {db_path}", file=sys.stderr)
        sys.exit(1)

    print(f"[llm-clean] Reading database {db_path}...")
    conn = sqlite3.connect(str(db_path))
    conn.row_factory = sqlite3.Row

    # Detect if provenance column exists in arguments table
    cursor = conn.cursor()
    cursor.execute("PRAGMA table_info(arguments)")
    cols = {row[1] for row in cursor.fetchall()}
    prov_sel = "provenance" if "provenance" in cols else "'inferred' AS provenance"

    rows = conn.execute(
        f"SELECT cmd_path, description, example_template, {prov_sel} FROM arguments "
        "WHERE example_template IS NOT NULL AND example_template != ''"
    ).fetchall()
    conn.close()

    todo = []
    for r in rows:
        # We only clean LLM-inferred rows that violate the validate heuristics
        if (r["provenance"] or "inferred") == "inferred" and is_fabricated(r["cmd_path"], r["example_template"]):
            todo.append({
                "cmd_path": r["cmd_path"],
                "description": r["description"],
                "example_template": r["example_template"]
            })

    total = len(todo)
    print(f"[llm-clean] Found {len(rows)} commands with examples. {total} need LLM correction.")
    if total == 0:
        print("[llm-clean] All examples comply with heuristics. No cleaning needed!")
        return

    # Start serialized database writer
    write_queue = queue.Queue()
    stop_event = threading.Event()
    writer_thread = threading.Thread(
        target=db_writer,
        args=(str(db_path), write_queue, total, stop_event)
    )
    writer_thread.start()

    s = requests.Session()
    lock = threading.Lock()
    completed = [0]
    failed = [0]
    t0 = time.time()

    def worker(item):
        cmd = item["cmd_path"]
        corrected = call_llm(s, key, args.model, cmd, item["description"], item["example_template"])

        with lock:
            completed[0] += 1
            if corrected:
                # Queue the write
                write_queue.put((cmd, corrected))

                # Check validation of the corrected version
                if is_fabricated(cmd, corrected):
                    # Flag if the LLM output itself still fails heuristics
                    # (This helps check prompt engineering robustness)
                    print(f"[warn] LLM output still fails heuristics: {cmd} -> {corrected}", file=sys.stderr)
            else:
                failed[0] += 1

            if completed[0] % 100 == 0:
                el = time.time() - t0
                rate = completed[0] / el if el else 0
                eta = (total - completed[0]) / rate if rate else 0
                print(f"[llm-clean] Progress: {completed[0]}/{total} ({rate:.1f}/s) — ETA {eta/60:.1f}min (errors: {failed[0]})", flush=True)

    print(f"[llm-clean] Starting ThreadPoolExecutor with {args.workers} workers...")
    try:
        with ThreadPoolExecutor(max_workers=args.workers) as pool:
            pool.map(worker, todo)
    except KeyboardInterrupt:
        print("\n[llm-clean] Interrupted by user. Wrapping up database writes...")

    # Wait for writer to finish saving all queue items
    stop_event.set()
    writer_thread.join()

    print(f"[llm-clean] Done. Processed {completed[0]} entries in {(time.time()-t0)/60:.1f}min. Failures: {failed[0]}")


if __name__ == "__main__":
    main()

"""Batch loop: queue -> probe -> merge -> checkpoint, with cache clearing."""

import subprocess
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from . import container
from .checkpoint import Checkpoint
from .probe_one import probe_one
from .queue import load_workitems


def merge(
    master_db: Path, scratch_db: Path, tool: str, app_id: str, repo_root: Path
) -> bool:
    r = subprocess.run(
        [
            "uv",
            "run",
            "--with",
            "sqlite-vec",
            "python3",
            str(repo_root / "scripts" / "merge_probe_into_master.py"),
            "--master",
            str(master_db),
            "--probe-db",
            str(scratch_db),
            "--tool",
            tool,
            "--app-id",
            app_id,
        ],
        capture_output=True,
        text=True,
    )
    return r.returncode == 0


def run(
    master_db: Path,
    extractor: Path,
    repo_root: Path,
    work_root: Path,
    checkpoint_path: Path,
    batch_size: int = 300,
    concurrency: int = 2,
    min_pop: float = 0.8,
) -> None:
    ck = Checkpoint(checkpoint_path)
    todo = [
        it
        for it in load_workitems(str(master_db), min_pop)
        if not ck.is_done(it.binary)
    ]
    print(f"[sandbox-probe] {len(todo)} tools to probe")

    for start in range(0, len(todo), batch_size):
        batch = todo[start : start + batch_size]
        print(f"[batch] {start}..{start + len(batch)}")
        with ThreadPoolExecutor(max_workers=concurrency) as ex:
            results = list(
                ex.map(lambda it: (it, probe_one(it, extractor, work_root)), batch)
            )
        for it, res in results:
            if res.status == "merged-candidate":
                ok = merge(master_db, res.scratch_db, it.binary, it.app_id, repo_root)
                ck.set(
                    it.binary,
                    "merged" if ok else "failed",
                    res.reason if ok else "merge-fail",
                )
            elif res.status == "no-subcommands":
                ck.set(it.binary, "no-subcommands", "no-subcommands")
            else:
                prev = ck.get(it.binary)
                attempts = (prev[2] if prev else 0) + 1
                ck.set(
                    it.binary, "perma-failed" if attempts >= 2 else "failed", res.reason
                )
        container.clear_cache()
        print(f"[batch] checkpoint: {ck.counts()}")
    print(f"[sandbox-probe] done. final: {ck.counts()}")

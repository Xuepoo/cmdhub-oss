import argparse
from pathlib import Path

from .runner import run

if __name__ == "__main__":
    ap = argparse.ArgumentParser(prog="sandbox_probe")
    ap.add_argument("--master", required=True, type=Path)
    ap.add_argument("--extractor", required=True, type=Path)
    ap.add_argument("--repo-root", required=True, type=Path)
    ap.add_argument("--work-root", default=Path("/tmp/sandbox-probe"), type=Path)
    ap.add_argument(
        "--checkpoint", default=Path("/tmp/sandbox-probe/checkpoint.db"), type=Path
    )
    ap.add_argument("--batch-size", type=int, default=50)
    ap.add_argument("--concurrency", type=int, default=2)
    ap.add_argument("--min-pop", type=float, default=0.8)
    a = ap.parse_args()
    a.work_root.mkdir(parents=True, exist_ok=True)
    a.checkpoint.parent.mkdir(parents=True, exist_ok=True)
    run(
        a.master,
        a.extractor,
        a.repo_root,
        a.work_root,
        a.checkpoint,
        a.batch_size,
        a.concurrency,
        a.min_pop,
    )

#!/usr/bin/env python3
"""Ingestion adapter: PyPI console applications → normalized ACI records.

PyPI has no API to filter "has CLI", but each package's JSON exposes its trove
classifiers. We take the most-downloaded packages (hugovk's top-pypi-packages) and
keep those classified `Environment :: Console` — an accurate CLI signal — emitting
name + summary + pip/uv install. Breadth layer.

Usage:
    uv run --with requests python3 scripts/adapter_pypi.py --out /tmp/pypi.json [--proxy ...] [--top N]
"""
from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

import requests

TOP_LIST = "https://hugovk.github.io/top-pypi-packages/top-pypi-packages-30-days.min.json"


def fetch(out_path: Path, proxy: str, top: int, workers: int) -> None:
    proxies = {"https": proxy, "http": proxy} if proxy else {}
    s = requests.Session()
    s.headers["User-Agent"] = "cmdhub-adapter/0.1 (txx15132@gmail.com)"

    print(f"[pypi] downloading top package list ...", flush=True)
    rows = s.get(TOP_LIST, proxies=proxies, timeout=60).json().get("rows", [])
    names = [r["project"] for r in rows[:top]]
    print(f"[pypi] checking {len(names)} packages for Environment::Console "
          f"({workers} workers) ...", flush=True)

    from concurrent.futures import ThreadPoolExecutor
    import threading
    lock = threading.Lock()
    records: list[dict] = []
    done = [0]

    def check(name: str) -> None:
        try:
            r = s.get(f"https://pypi.org/pypi/{name}/json", proxies=proxies, timeout=15)
            if r.status_code == 200:
                info = r.json().get("info", {})
                if any(c.startswith("Environment :: Console") for c in (info.get("classifiers") or [])):
                    desc = (info.get("summary") or "").strip()
                    rec = {
                        "app_id": f"org.pypi.{name}", "name": name, "cmd_path": name,
                        "description": desc or f"{name} (PyPI console application)",
                        "install_instructions": {"pip": f"pip install {name}", "uv": f"uv tool install {name}"},
                        "source": "pypi",
                    }
                    with lock:
                        records.append(rec)
        except Exception:
            pass
        with lock:
            done[0] += 1
            if done[0] % 1000 == 0:
                print(f"[pypi] {done[0]}/{len(names)} checked, {len(records)} CLIs", flush=True)

    with ThreadPoolExecutor(max_workers=workers) as pool:
        list(pool.map(check, names))

    out_path.write_text(json.dumps(records, ensure_ascii=False))
    print(f"[pypi] wrote {len(records)} records → {out_path}")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--proxy", default="")
    ap.add_argument("--top", type=int, default=15000)
    ap.add_argument("--workers", type=int, default=12)
    a = ap.parse_args()
    fetch(a.out, a.proxy, a.top, a.workers)


if __name__ == "__main__":
    main()

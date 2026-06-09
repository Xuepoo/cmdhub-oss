#!/usr/bin/env python3
"""Ingestion adapter: crates.io command-line-utilities → normalized ACI records.

Breadth-layer: enumerates every Rust CLI crate (name + description + cargo install),
no subcommand probing. Cross-distro install (yay/brew/dnf...) is layered on later by
the Repology/AUR enrichment passes. Output is a JSON list consumed by import_registry.py.

Usage:
    uv run --with requests python3 scripts/adapter_crates.py --out /tmp/crates.json [--proxy ...]
"""
from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

import requests


def fetch(out_path: Path, proxy: str, max_pages: int) -> None:
    proxies = {"https": proxy, "http": proxy} if proxy else {}
    s = requests.Session()
    s.headers["User-Agent"] = "cmdhub-adapter/0.1 (txx15132@gmail.com)"

    records: list[dict] = []
    page = 1
    while page <= max_pages:
        url = ("https://crates.io/api/v1/crates"
               f"?category=command-line-utilities&per_page=100&page={page}&sort=downloads")
        for attempt in range(4):
            try:
                r = s.get(url, proxies=proxies, timeout=20)
                if r.status_code == 429:
                    time.sleep(3 * (attempt + 1)); continue
                r.raise_for_status()
                break
            except Exception as e:
                if attempt == 3:
                    print(f"[warn] page {page}: {e}", file=sys.stderr); r = None
                else:
                    time.sleep(2)
        if r is None:
            break
        crates = r.json().get("crates", [])
        if not crates:
            break
        for c in crates:
            name = c.get("id") or c.get("name")
            if not name:
                continue
            desc = (c.get("description") or "").strip()
            records.append({
                "app_id": f"io.crates.{name}",
                "name": name,
                "cmd_path": name,
                "description": desc or f"{name} (Rust command-line tool)",
                "install_instructions": {"cargo": f"cargo install {name}"},
                "source": "crates.io",
            })
        print(f"[crates] page {page}: +{len(crates)} (total {len(records)})", flush=True)
        page += 1
        time.sleep(1.1)  # crates.io asks ~1 req/sec

    out_path.write_text(json.dumps(records, ensure_ascii=False))
    print(f"[crates] wrote {len(records)} records → {out_path}")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--proxy", default="")
    ap.add_argument("--max-pages", type=int, default=200)
    fetch(**{"out_path": (a := ap.parse_args()).out, "proxy": a.proxy, "max_pages": a.max_pages})


if __name__ == "__main__":
    main()

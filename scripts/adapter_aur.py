#!/usr/bin/env python3
"""Ingestion adapter: full AUR metadata dump → normalized ACI records.

Downloads packages-meta-ext-v1.json.gz (~all AUR packages), drops obvious non-CLI
assets (fonts/themes/icons/lib32/dkms/-git duplicates), and emits name + description
+ yay/paru install. Breadth layer; cross-distro install added later by enrichment.

Usage:
    uv run --with requests python3 scripts/adapter_aur.py --out /tmp/aur.json [--proxy ...]
"""
from __future__ import annotations

import argparse
import gzip
import io
import json
import re
import sys
from pathlib import Path

import requests

# Names that are almost never an interactive CLI tool → skip to keep the index useful.
_SKIP = re.compile(
    r"(^lib32-|^lib\d|-dkms$|-dbg$|-debug$|-git$|-svn$|-hg$|-bzr$"
    r"|^ttf-|^otf-|-fonts?$|-theme$|-themes$|-icon-theme$|-icons$|-cursor-theme$"
    r"|-wallpapers?$|-gtk-theme$|-kde-theme$|-cursors$|-sounds?$|-emoji)",
    re.IGNORECASE,
)


def fetch(out_path: Path, proxy: str) -> None:
    proxies = {"https": proxy, "http": proxy} if proxy else {}
    s = requests.Session()
    s.headers["User-Agent"] = "cmdhub-adapter/0.1 (txx15132@gmail.com)"
    url = "https://aur.archlinux.org/packages-meta-ext-v1.json.gz"
    print(f"[aur] downloading {url} ...", flush=True)
    r = s.get(url, proxies=proxies, timeout=120)
    r.raise_for_status()
    data = json.load(gzip.GzipFile(fileobj=io.BytesIO(r.content)))
    print(f"[aur] {len(data)} packages in dump", flush=True)

    records: list[dict] = []
    skipped = 0
    for p in data:
        name = p.get("Name")
        if not name or _SKIP.search(name):
            skipped += 1
            continue
        desc = (p.get("Description") or "").strip()
        records.append({
            "app_id": f"org.archlinux.aur.{name}",
            "name": name,
            "cmd_path": name,
            "description": desc or f"{name} (AUR package)",
            "install_instructions": {"yay": f"yay -S {name}", "paru": f"paru -S {name}"},
            "source": "aur",
        })
    out_path.write_text(json.dumps(records, ensure_ascii=False))
    print(f"[aur] wrote {len(records)} records ({skipped} skipped) → {out_path}")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--proxy", default="")
    a = ap.parse_args()
    fetch(a.out, a.proxy)


if __name__ == "__main__":
    main()

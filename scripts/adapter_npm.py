#!/usr/bin/env python3
"""Ingestion adapter: npm CLI packages → normalized ACI records.

Enumerates packages tagged with the `cli` keyword via the npm registry search API
(the closest thing to a "has CLI" filter at scale), emits name + description +
`npm install -g`. Breadth layer.

Usage:
    uv run --with requests python3 scripts/adapter_npm.py --out /tmp/npm.json [--proxy ...] [--max N]
"""
from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

import requests


def _get_npm_metadata(s: requests.Session, name: str, proxies: dict) -> tuple[bool, str]:
    """Returns (has_bin, repo_url) for the npm package."""
    try:
        r = s.get(f"https://registry.npmjs.org/{name}", proxies=proxies, timeout=10)
        if r.status_code != 200:
            return False, ""
        d = r.json()
        latest = d.get("dist-tags", {}).get("latest")
        if not latest:
            return False, ""
        ver_data = d.get("versions", {}).get(latest, {})
        has_bin = bool(ver_data.get("bin"))

        repo = ver_data.get("repository") or d.get("repository") or ver_data.get("homepage") or d.get("homepage") or ""
        repo_url = ""
        if isinstance(repo, dict):
            repo_url = repo.get("url") or ""
        elif isinstance(repo, str):
            repo_url = repo

        if repo_url.startswith("git+"):
            repo_url = repo_url[4:]
        if repo_url.endswith(".git"):
            repo_url = repo_url[:-4]
        return has_bin, repo_url
    except Exception:
        return False, ""


def fetch(out_path: Path, proxy: str, max_pkgs: int) -> None:
    proxies = {"https": proxy, "http": proxy} if proxy else {}
    s = requests.Session()
    s.headers["User-Agent"] = "cmdhub-adapter/0.1 (txx15132@gmail.com)"

    seen: set[str] = set()
    records: list[dict] = []
    frm = 0
    SIZE = 250
    stale = 0  # consecutive pages that added no new package (npm returns dups past its cap)
    while len(records) < max_pkgs and frm <= 12000 and stale < 3:
        url = f"https://registry.npmjs.org/-/v1/search?text=keywords:cli&size={SIZE}&from={frm}"
        for attempt in range(4):
            try:
                r = s.get(url, proxies=proxies, timeout=20)
                if r.status_code == 429:
                    time.sleep(3 * (attempt + 1)); continue
                r.raise_for_status(); break
            except Exception as e:
                if attempt == 3:
                    print(f"[warn] from={frm}: {e}", file=sys.stderr); r = None
                else:
                    time.sleep(2)
        if r is None:
            break
        objs = r.json().get("objects", [])
        if not objs:
            break
        before = len(seen)  # track unique names seen, not records (bin-check filters records)
        for o in objs:
            pkg = o.get("package", {})
            name = pkg.get("name")
            if not name or name in seen:
                continue
            seen.add(name)
            has_bin, repo_url = _get_npm_metadata(s, name, proxies)
            if not has_bin:  # keep only real CLIs, not libraries
                continue
            desc = (pkg.get("description") or "").strip()
            records.append({
                "app_id": f"com.npmjs.{name.lstrip('@').replace('/', '-')}",
                "name": name.split("/")[-1],
                "cmd_path": name.split("/")[-1],
                "description": desc or f"{name} (npm CLI package)",
                "install_instructions": {"npm": f"npm install -g {name}"},
                "source": "npm",
                "source_url": repo_url,
            })
        stale = stale + 1 if len(seen) == before else 0
        print(f"[npm] from={frm}: seen {len(seen)}, cli {len(records)}", flush=True)
        frm += SIZE
        time.sleep(0.5)

    out_path.write_text(json.dumps(records, ensure_ascii=False))
    print(f"[npm] wrote {len(records)} records → {out_path}")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--proxy", default="")
    ap.add_argument("--max", type=int, default=12000)
    a = ap.parse_args()
    fetch(a.out, a.proxy, a.max)


if __name__ == "__main__":
    main()

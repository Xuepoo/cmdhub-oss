"""Install + probe ONE tool in the sandbox; return a typed result."""

import json
import sqlite3
from dataclasses import dataclass
from pathlib import Path

from . import container
from .queue import WorkItem


@dataclass
class ProbeResult:
    status: str  # merged-candidate | no-subcommands | failed
    reason: str  # probe-ok | no-subcommands | install-fail | no-package | timeout
    scratch_db: Path | None = None


def probe_one(item: WorkItem, extractor: Path, work_root: Path) -> ProbeResult:
    name = item.binary.replace(".", "_").replace("/", "_")
    pkg = item.pkg or container.resolve_pkg_in_container(item.binary)
    if not pkg:
        return ProbeResult("failed", "no-package")

    scratch = work_root / name
    scratch.mkdir(parents=True, exist_ok=True)
    targets = scratch / "targets.json"
    targets.write_text(
        json.dumps({"targets": [{"name": item.binary, "path": item.binary}]})
    )

    try:
        if not container.install(pkg, name):
            return ProbeResult("failed", "install-fail")
        if not container.probe(name, extractor, scratch, targets):
            return ProbeResult("failed", "timeout")
    except Exception:
        container._run(["podman", "rm", "-f", name], timeout=30)
        return ProbeResult("failed", "timeout")

    db = scratch / "cmdhub" / "cmdhub.db"
    if not db.exists():
        return ProbeResult("failed", "install-fail")
    n = (
        sqlite3.connect(str(db))
        .execute(
            "SELECT count(*) FROM arguments WHERE cmd_path LIKE ?||'.%'", (item.binary,)
        )
        .fetchone()[0]
    )
    if n == 0:
        return ProbeResult("no-subcommands", "no-subcommands", db)
    return ProbeResult("merged-candidate", "probe-ok", db)

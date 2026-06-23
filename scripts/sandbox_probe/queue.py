"""Build the popularity-ordered work queue from the master DB."""

import json
import re
import sqlite3
from collections import Counter
from dataclasses import dataclass
from typing import Optional


@dataclass
class WorkItem:
    binary: str
    app_id: str
    pkg: Optional[str]  # None -> caller does in-container `pacman -F` fallback


def resolve_pkg(binary: str, install_json: Optional[str]) -> Optional[str]:
    """Consensus package name from install_instructions; None if unresolvable."""
    if not install_json:
        return None
    try:
        d = json.loads(install_json)
    except Exception:
        return None
    names = []
    for cmd in d.values():
        toks = str(cmd).split()
        if not toks:
            continue
        n = toks[-1].split("/")[-1].lower()
        n = re.sub(r"[-_]?bin$", "", n)
        if re.match(r"^[a-z][a-z0-9._+-]{1,}$", n):
            names.append(n)
    if not names:
        return None
    name, _ = Counter(names).most_common(1)[0]
    return name


def load_workitems(master_db: str, min_pop: float = 0.8) -> list[WorkItem]:
    c = sqlite3.connect(master_db)
    rows = c.execute(
        "SELECT a.cmd_path, a.app_id, p.install_instructions "
        "FROM arguments a JOIN apps p ON p.app_id = a.app_id "
        "WHERE a.node_type='root' AND a.provenance!='probe' AND p.popularity >= ? "
        "ORDER BY p.popularity DESC",
        (min_pop,),
    ).fetchall()
    c.close()
    return [
        WorkItem(binary=cp, app_id=aid, pkg=resolve_pkg(cp, inst))
        for cp, aid, inst in rows
    ]

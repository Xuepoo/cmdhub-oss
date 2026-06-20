#!/usr/bin/env python3
"""Reverse-coverage search tester: for each probe-verified command, generate
name-free task queries, run `cmdh search`, report unfindable tools categorized
with fix suggestions. Discovery tool, not a release gate. See
docs/superpowers/specs/2026-06-20-coverage-search-tester-design.md
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import sqlite3
import subprocess
import sys
import time
from collections import Counter
from dataclasses import dataclass, field


@dataclass
class Command:
    cmd_path: str
    description: str


@dataclass
class Verdict:
    cmd_path: str
    query: str
    status: str                       # "pass" | "near_miss" | "fail"
    rank: int | None                  # 1-based rank of cmd_path, else None
    category: str | None = None       # set for fails/near-misses
    blockers: list[str] = field(default_factory=list)  # cmd_paths above source
    suggestion: str | None = None


def load_commands(db_path: str) -> list[Command]:
    """Probe-verified commands (cmd_path + description) from the master registry."""
    con = sqlite3.connect(db_path)
    rows = con.execute(
        "SELECT cmd_path, COALESCE(description,'') FROM arguments "
        "WHERE provenance='probe' AND cmd_path IS NOT NULL"
    ).fetchall()
    con.close()
    return [Command(cmd_path=p, description=d) for p, d in rows if p]

# CmdHub FAQ

Common questions about `cmdh` search, results, and the offline database.

## Search & results

### Why did a search return only one result?
Earlier builds defaulted `--limit` to 1. It now defaults to **5**. Use `--limit N`
to get more candidates (useful for agents that want to choose among options):

```bash
cmdh search "create a vpc on aws" --limit 10
```

### A natural-language query returns nothing — why?
Verbose queries like *"I want to know how to configure networking using AWS"* used to
hit a vector-similarity cutoff and return empty. This is fixed: the cutoff now only
applies when the query matches **nothing** by keyword either. Phrase queries around the
action and tool (`configure networking aws`, `create a vpc on aws`) for best results.

### The result is a related-but-not-exact subcommand. Is that expected?
Search is hybrid (keyword FTS + semantic vector) with a path-match re-rank that lifts the
command whose name best matches your words. The exact command is usually in the top few;
raise `--limit`. AWS uses `describe-*` where you might say "list", so
`aws ec2 describe-instances` answers "list ec2 instances".

### How accurate is it? Do I need an internet connection or an LLM?
No. Search is **fully offline** (SQLite FTS5 + local ONNX vector search, sub-millisecond).
No API calls, no tokens. The data was built once from real `--help` output.

## Install commands

### `install_command` is `null` or looks wrong
- **Wrong/stale value** (e.g. `sudo netpbm`): rebuild/reinstall `cmdh`; old binaries
  predate install-command normalization.
- **`null`**: the tool ships only via a package manager you haven't configured. `cmdh`
  prefers your `~/.config/cmdhub` `package_managers` + your OS's system manager, then falls
  back to any available method. A tool that's pip-only (e.g. `oci-cli`) will show
  `pip install ...` even if you prefer `cargo`.
- Configure your preferred managers in `~/.config/cmdhub/config.toml` under `[install]`.

### Why is a tool listed under `pacman` when it's actually an npm/pip package?
Cross-platform install data is enriched by name-matching against package repositories,
which occasionally mismatches a same-named but unrelated package. Directly-probed tools
(aws, gcloud, kubectl, gh, …) carry curated, correct install methods. Report bad ones.

## Coverage & data

### Why are some cloud-CLI subcommands missing?
Coverage is built by recursively probing installed CLIs. The big cloud CLIs (aws, gcloud,
oci, az, terraform/opentofu, aliyun, tccli, openstack) contribute most subcommands and are
probed deeply; coverage is expanded continuously. If a subcommand is missing it simply
hasn't been probed yet.

### Are Cisco IOS / Huawei VRP commands included?
Not yet. Those are device operating-system CLIs that can't be installed and probed on a
normal machine; they need documentation-based ingestion (planned).

### Why are `help` / `version` / `completion` subcommands not in results?
They're filtered out deliberately — near-identical across thousands of tools and never the
target of a search.

### How is `risk_level` determined?
A keyword heuristic on the command verb: `delete/terminate/destroy/...` → `dangerous`,
`create/update/apply/...` → `medium`, else `safe`. Agents should respect `dangerous`.

## Updating the database

### How do I get the latest data?
`cmdh sync` pulls the latest signed `cmdhub.db.zst` from the CDN and atomically replaces
the local DB after Ed25519 verification.

### How is the database built? (contributors)
`probe_cli.py` (recursive `--help` capture) → `import_deep_cli.py` / `import_probe_results.py`
→ `export_sqlite.py` → `build_db.py` (real BGE-micro-v2 embeddings; `--device cuda` for GPU)
→ `validate_db.py`. See `scripts/` and the cold-start pipeline notes.

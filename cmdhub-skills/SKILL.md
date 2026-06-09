---
name: cmdhub-command-discovery
description: >-
  Discover the right command-line tool or subcommand for a task, offline and
  instantly, via `cmdh`. Use whenever you need to find which CLI command does
  something ("how do I create a VPC on AWS", "delete files", "convert image to
  webp"), get its install method for the current OS, or check a command's risk
  level before running it. Backed by a local hybrid (FTS + vector) search over a
  registry of 100k+ tools and subcommands — no network, sub-millisecond.
license: MIT
---

# CmdHub Command Discovery

`cmdh` turns a natural-language intent into concrete CLI commands with their
install instructions, risk level, and usage template. It searches a local,
signed offline database (SQLite FTS5 + ONNX vector search, RRF fusion).

## When to use this skill

- You need the command/subcommand for a task but don't remember the exact name
  (e.g. "list ec2 instances" → `aws ec2 describe-instances`).
- You need the correct **install command for the user's OS** (Arch→`yay/pacman`,
  Debian/Fedora→`apt/dnf` or `cargo/pip/npm`, macOS→`brew`, Windows→`scoop`).
- You must check a command's **risk level** (`safe`/`medium`/`dangerous`) before
  executing — never auto-run a `dangerous` command without confirmation.
- You want offline, zero-token discovery instead of guessing.

## How to use

Search (JSON to stdout; logs to stderr):

```bash
cmdh search "I want to create a vpc on aws" --limit 5
```

Each result is an ACI contract:

```json
{
  "app_id": "org.cmdhub.aws-cli",
  "name": "aws",
  "cmd_path": "aws.ec2.create-vpc",
  "node_type": "sub",
  "description": "Creates a VPC with the specified CIDR blocks.",
  "risk_level": "dangerous",
  "install_command": "sudo pacman -S aws-cli-v2",
  "example_template": "aws ec2 create-vpc --cidr-block {{cidr}}",
  "status": "installed"
}
```

Guidance for agents:

- Prefer the result whose `cmd_path` and `description` best match the intent;
  raise `--limit` to see more candidates. Phrase queries with the resource/verb
  ("create vpc", "list instances") for the sharpest results.
- If `status` is `not_installed`, surface `install_command` (already resolved for
  the host OS) before running.
- Respect `risk_level`: confirm with the user before any `dangerous` command.
- Use `--full` / `--usage` / `--minimal` to control output verbosity.

## MCP integration

For tool-calling agents, run the MCP server (`cmdhub-mcp`, stdio JSON-RPC) which
exposes two tools:

- `cmdhub_search` — `{ "query": string, "limit"?: number }` → ranked ACI results.
- `cmdhub_execute` — `{ "cmd_path": string, "args"?: [string] }` → runs the tool
  (refuses `dangerous` commands unless explicitly allowed).

Register it in your MCP client config; no API key or network required.

## Keeping data fresh

`cmdh sync` pulls the latest signed `cmdhub.db.zst` from the CDN (Ed25519-verified,
atomic swap). The database is continuously updated from package registries
(crates.io, AUR, PyPI, npm, Homebrew) and deep `--help` probes.

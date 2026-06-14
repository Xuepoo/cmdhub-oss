# CmdHub: Agent-Computer Utility Command Hub

> English | [简体中文](./README.zh-CN.md)

> A decentralized, intent-driven CLI command registry and offline search infrastructure built for AI Agents and modern developers.

## What is CmdHub?

CmdHub provides standardized, machine-readable **ACI (Agent-Computer Interface)** contracts for CLI tools, enabling AI Agents to discover and execute terminal commands with zero hallucination, millisecond latency, and full security guardrails.

### Architecture

```
cmdhub-oss/                  # This repository (open-source)
├── cmdhub-cli/              # cmdh — local CLI client (Rust)
├── cmdhub-mcp/              # MCP server for IDE/Agent integration
├── cmdhub-shared/           # Shared types and ACI schema definitions
├── cmdhub-skills/           # Plugin/skill system
└── schemas/                 # ACI JSON Schema definitions
```

### Key Features

- **Offline Hybrid Search**: FTS5 full-text + sqlite-vec vector search with RRF fusion (< 1ms)
- **MCP Server**: Native Model Context Protocol for Claude Code, Cursor, etc.
- **Safety Guardrail**: Risk-level blocking for dangerous commands
- **XDG Compliant**: Strict adherence to XDG Base Directory Specification
- **Ed25519 Signed DB**: Cryptographic verification of all distributed databases

## Quick Start

```bash
# Install from source
cargo install cmdhub-cli

# Or build locally
cargo build --release -p cmdhub-cli

# Update the local database
cmdh update

# Search for a command
cmdh search "extract tar excluding node_modules"

# Start MCP server
cmdh mcp --transport stdio
```

## Use as an AI Agent Skill

CmdHub ships an [Agent Skill](./cmdhub-skills/SKILL.md) that teaches AI agents (Claude
Code, etc.) when and how to use `cmdh` for offline command discovery. Install it with the
[`skills`](https://github.com/obra/skills) CLI:

```bash
npx skills add https://github.com/Xuepoo/cmdhub-oss/tree/main/cmdhub-skills
```

The skill covers search tips, OS-aware install resolution, risk handling, and the
`cmdhub_search` / `cmdhub_execute` MCP tools.

## Documentation

Full architecture, API, and design specifications are in the [cmdhub-docs](https://github.com/Xuepoo/cmdhub/tree/main/cmdhub-docs) directory.

Key documents:
- [Product Requirements](./cmdhub-docs/01-prd.md)
- [Architecture Design](./cmdhub-docs/02-architecture-design.md)
- [CLI Design Spec](./cmdhub-docs/04-cli-design-spec.md)
- [ACI Schema Definition](./cmdhub-docs/09-aci-schema-definition.md)
- [MCP Protocol Spec](./cmdhub-docs/11-mcp-server-protocol.md)

## Development

```bash
# Format check
cargo fmt --all -- --check

# Lint
cargo clippy --all-targets --all-features -- -D warnings

# Test
cargo test --all-features --workspace

# Pre-commit
pre-commit run --all-files
```

## License

MIT License — see [LICENSE](./LICENSE)

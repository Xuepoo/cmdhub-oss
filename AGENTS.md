# AGENTS.md — AI Coding Agent Instructions

This file provides mandatory instructions for AI Coding Agents (Claude Code, Cursor, Codex, Hermes, etc.) working on the CmdHub OSS codebase. Read this file before making any changes.

## Project Overview

CmdHub is a decentralized CLI command registry for AI Agents. This repo (`cmdhub-oss`) contains:
- **cmdhub-cli** (`cmdh`): The local CLI client — offline search, MCP server, safety guardrail
- **cmdhub-mcp**: MCP server for IDE/Agent integration
- **cmdhub-shared**: Shared types, ACI schema definitions, common utilities

Tech stack: **Pure Rust**, no Python/Node/Go in this repo.

## Mandatory Pre-Commit Checklist

Before ANY commit, run ALL of these:

```bash
cargo fmt --all -- --check          # 1. Formatting
cargo clippy --all-targets --all-features -- -D warnings  # 2. Lint (zero warnings)
cargo test --all-features --workspace  # 3. All tests pass
```

## Coding Conventions

### Language & Style
- **All code, comments, variable names, and documentation MUST be in English**
- Use `cargo fmt` — no manual formatting
- Use `cargo clippy` — fix ALL warnings, never suppress with `#[allow(...)]` unless mathematically justified
- Line length: 100 chars max (enforced by rustfmt.toml)

### Error Handling
- Use `anyhow::Result` for application-level errors (CLI binary)
- Use `thiserror` for library-level error types (cmdhub-shared)
- **NEVER use `.unwrap()` or `.expect()`** in production code unless the invariant is mathematically guaranteed and documented with a comment
- Propagate errors with `?` operator

### STDOUT / STDERR Separation (Critical for Agent Consumption)
- **STDOUT** (`println!`): ONLY valid JSON (ACI schemas) or final deterministic results. Agents pipe this to `jq`.
- **STDERR** (`eprintln!` / `tracing`): ALL debug logs, progress bars, warnings, update prompts, security alerts
- This is not optional — breaking this breaks Agent pipelines

### XDG Compliance (Critical)
- **Config**: `$XDG_CONFIG_HOME/cmdhub` (~/.config/cmdhub)
- **Data**: `$XDG_DATA_HOME/cmdhub` (~/.local/share/cmdhub)
- **Cache**: `$XDG_CACHE_HOME/cmdhub` (~/.cache/cmdhub)
- NEVER write to `~/.cmdhub` or any non-XDG path
- Use the `directories` or `etcetera` crate — no manual path concatenation

### Security
- Never commit secrets, API keys, or Ed25519 private keys
- Run `gitleaks detect --source .` before pushing
- Dangerous commands must be blocked by the safety guardrail (risk_level == "dangerous")
- Atomic DB updates: download → verify signature → `std::fs::rename()`

## Architecture Notes

### Workspace Structure
```
cmdhub-oss/
├── Cargo.toml              # Workspace root
├── cmdhub-shared/          # Shared lib: ACI types, schema, error types
│   └── src/lib.rs
├── cmdhub-cli/             # Binary: cmdh CLI client
│   └── src/main.rs         # Entry point, clap command tree
├── cmdhub-mcp/             # Binary: MCP server (stdio/sse)
│   └── src/main.rs
├── schemas/                # ACI JSON Schema definitions
└── tests/                  # Integration tests
```

### Key Crates (planned dependencies)
- `clap` (derive) — CLI argument parsing
- `rusqlite` + `sqlite-vec` — Local database with vector search
- `tokio` — Async runtime (for MCP server)
- `axum` — HTTP server (for SSE transport)
- `serde` / `serde_json` — Serialization
- `anyhow` / `thiserror` — Error handling
- `tracing` / `tracing-subscriber` — Structured logging (to STDERR)
- `zstd` — Zstandard decompression for DB updates
- `ed25519-dalek` — Signature verification
- `reqwest` — HTTP client for DB updates
- `directories` — XDG path resolution
- `fs2` — File locking for concurrent update protection

### Data Flow
```
User/Agent → cmdh search "intent"
  → FTS5 BM25 channel  → Top 10
  → sqlite-vec channel  → Top 10
  → RRF fusion (k=60)   → Top 1 ACI contract
  → STDOUT (JSON)
```

## Git Workflow

1. **Issue first**: Always create/assign a GitHub issue
2. **Branch naming**: `feat/issue-N-description`, `fix/issue-N-description`, `docs/description`
3. **Conventional Commits**: `type(scope): description`
   - Types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`
   - Scopes: `cli`, `mcp`, `shared`, `schema`, `ci`, `docs`
4. **Squash merge only** into `main`
5. **No force-push** on `main`

## File Organization

- Unit tests: `#[cfg(test)] mod tests` at the bottom of each `.rs` file
- Integration tests: `tests/` directory at workspace root
- Benchmarks: `benches/` directory
- JSON Schemas: `schemas/` directory
- Never create files in the wrong crate — check which crate owns the module first

## Anti-Patterns (DO NOT)

- `.unwrap()` in production paths
- `println!()` for debug output (use `eprintln!` or `tracing`)
- `std::process::exit()` — use proper error propagation
- Hardcoded paths — use XDG via `directories` crate
- `unsafe` blocks — if absolutely needed, document the safety invariant in a `// SAFETY:` comment
- Mixing business logic in `main.rs` — extract to modules
- Adding Node.js, Python, or Go dependencies — this is a pure Rust project

## Testing Strategy

- **Unit tests**: Test individual functions, especially parsing and scoring logic
- **Integration tests**: Test full CLI invocations via `assert_cmd` or similar
- **Schema tests**: Validate ACI JSON output against the JSON Schema
- **Property tests**: Use `proptest` for search ranking and RRF scoring

## When You're Stuck

1. Check if the question is answered in `cmdhub-docs/` (the sibling docs repo)
2. Read `06-development-guidelines.md` and `07-workflow.md` for conventions
3. Check `09-aci-schema-definition.md` for ACI contract structure
4. Check `11-mcp-server-protocol.md` for MCP tool definitions

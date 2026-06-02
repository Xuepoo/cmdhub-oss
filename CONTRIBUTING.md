# Contributing to CmdHub OSS

Thank you for your interest in contributing to CmdHub! This guide covers everything you need to know to get started.

## Table of Contents

- [Getting Started](#getting-started)
- [Development Setup](#development-setup)
- [Workflow](#workflow)
- [Coding Standards](#coding-standards)
- [Testing](#testing)
- [Submitting Changes](#submitting-changes)
- [AI Agent Contributions](#ai-agent-contributions)

## Getting Started

### Prerequisites

```bash
# Rust toolchain (stable)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup component add rustfmt clippy rust-analyzer

# Pre-commit hooks
pip install pre-commit
pre-commit install
pre-commit install --hook-type commit-msg

# Recommended CLI tools
cargo install just          # Task runner
cargo install cargo-deny    # License/advisory auditing
cargo install cargo-edit    # Version management
```

### First-Time Setup

```bash
# Clone with submodules
git clone --recurse-submodules git@github.com:Xuepoo/cmdhub.git
cd cmdhub/cmdhub-oss

# Install pre-commit hooks
pre-commit install
pre-commit install --hook-type commit-msg

# Verify everything works
cargo build
cargo test --all-features --workspace
cargo clippy --all-targets --all-features -- -D warnings
```

## Development Setup

### Local Infrastructure

CmdHub OSS does NOT require external infrastructure for development — it runs fully offline. However, if you're testing the update flow:

```bash
# From monorepo root
just up    # Starts PostgreSQL, Redis, MinIO (for cloud development)
```

### IDE Setup

Recommended: VS Code with `rust-analyzer` extension, or Neovim with LSP.

Key settings:
- `rust-analyzer.check.command`: "clippy"
- `rust-analyzer.cargo.features`: "all"

## Workflow

We follow **Issue → Branch → Commit → PR → Review → Merge**.

### 1. Find or Create an Issue

Browse [open issues](https://github.com/Xuepoo/cmdhub-oss/issues) or create one using the templates.

### 2. Create a Branch

```bash
git checkout -b feat/issue-123-add-vector-search
```

Branch naming:
- `feat/issue-N-description` — new features
- `fix/issue-N-description` — bug fixes
- `docs/description` — documentation only
- `refactor/description` — code refactoring
- `test/description` — test additions

### 3. Write Code (TDD Preferred)

```bash
# Write failing test first
cargo test --all-features  # Watch it fail

# Implement the feature
cargo test --all-features  # Watch it pass

# Refactor
cargo clippy --all-targets --all-features -- -D warnings
```

### 4. Commit

```bash
git add -A
git commit -m "feat(cli): add RRF search scoring"
```

Commit message format: **Conventional Commits**

```
type(scope): description

[optional body]

[optional footer]
```

Types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`

Scopes: `cli`, `mcp`, `shared`, `schema`, `ci`, `docs`

### 5. Open a Pull Request

Use the PR template. Include:
- What changed and why
- Testing plan
- Breaking changes (if any)

### 6. Code Review

All PRs require:
- Passing CI checks (fmt, clippy, test, docs, typos)
- At least one review approval
- No merge conflicts

## Coding Standards

### Language
- **All code, comments, variable names, documentation in English**
- Technical terms keep original casing (gVisor, Ed25519, pgvector)

### Formatting
```bash
cargo fmt --all              # Auto-format
cargo fmt --all -- --check   # Check only (CI mode)
```

### Linting
```bash
cargo clippy --all-targets --all-features -- -D warnings
```

Zero warnings allowed. Never suppress with `#[allow(...)]` unless the invariant is mathematically justified and documented.

### Error Handling
- Use `anyhow::Result` for application-level errors
- Use `thiserror` for library-level typed errors
- **NEVER** use `.unwrap()` or `.expect()` in production code
- Propagate with `?` operator

### STDOUT/STDERR Separation (Critical)
- **STDOUT** (`println!`): ONLY valid JSON (ACI schemas) or final results
- **STDERR** (`eprintln!`/`tracing`): ALL logs, progress, warnings, debug output
- This is mandatory — Agents pipe STDOUT to `jq`

### XDG Compliance
- Config: `$XDG_CONFIG_HOME/cmdhub` (~/.config/cmdhub)
- Data: `$XDG_DATA_HOME/cmdhub` (~/.local/share/cmdhub)
- Cache: `$XDG_CACHE_HOME/cmdhub` (~/.cache/cmdhub)
- Use the `directories` crate — no manual path construction

## Testing

### Unit Tests

Place `#[cfg(test)]` modules at the bottom of each `.rs` file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rrf_scoring() {
        // ...
    }
}
```

### Integration Tests

Place in `/tests` directory at workspace root:

```rust
// tests/search_integration.rs
use assert_cmd::Command;

#[test]
fn test_search_returns_aci_json() {
    Command::cargo_bin("cmdh").unwrap()
        .args(&["search", "list files"])
        .assert()
        .success()
        .stdout(predicates::str::contains("app_id"));
}
```

### Running Tests

```bash
cargo test --all-features --workspace          # All tests
cargo test -p cmdhub-shared                    # Single crate
cargo test test_rrf                            # Filter by name
cargo test --all-features -- --nocapture       # Show stdout
```

### Security Checks

```bash
gitleaks detect --source .     # No secrets committed
cargo deny check               # License & advisory audit
```

## Submitting Changes

### Pre-Submit Checklist

```bash
# 1. Format
cargo fmt --all -- --check

# 2. Lint
cargo clippy --all-targets --all-features -- -D warnings

# 3. Test
cargo test --all-features --workspace

# 4. Docs
cargo doc --no-deps --all-features --workspace

# 5. Security
gitleaks detect --source .

# 6. Typos
typos
```

### Merge Policy

- **Squash merge only** into `main`
- No force-push on `main`
- All CI checks must pass
- At least one approval required

## AI Agent Contributions

If you are an AI Coding Agent (Claude Code, Cursor, Codex, etc.):

1. **Read `AGENTS.md` first** — it contains mandatory conventions
2. **Read `cmdhub-docs/06-development-guidelines.md`** — enforced rules
3. **Run all checks before declaring done** — `cargo fmt`, `clippy`, `test`
4. **STDOUT purity is non-negotiable** — never `println!` debug output
5. **No `.unwrap()` in production paths** — use `?` propagation

## Questions?

- Open a [Discussion](https://github.com/Xuepoo/cmdhub-oss/discussions)
- Check existing [Issues](https://github.com/Xuepoo/cmdhub-oss/issues)

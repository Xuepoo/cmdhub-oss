# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1] - 2026-06-15

### Fixed
- **Semantic search now works on fresh installs.** The default model SHA-256 was a placeholder that never matched the published `bge-micro-v2.onnx`, so verification failed and search silently fell back to FTS-only. ([#14])
- **Correct CDN domain.** The previously published v0.1.0 binary pointed at a retired `cdn.cmdhub.xyz`; builds now use `cdn.cmdhub.org`.

### Added
- **Embedded starter database** — a compact, offline top-~1500-command registry (with vectors) is bundled in the binary, so `cmdh search` returns results immediately on a fresh install with no network and no prior `cmdh update`. Run `cmdh update` to overlay the full 100k+ catalog from the CDN.
- `CMDH_NO_STARTER` env var to skip starter hydration (testing/CI).

### Changed
- The embedded starter is only used when the local DB is missing or empty; a corrupt DB is left for the existing recovery path (`cmdh update --force`).

## [0.1.0-alpha.5] - 2026-06-04

### Added
- Automated integration test suite (`tests/mcp_integration_tests.rs`) for `cmdhub-mcp` daemon verifying JSON-RPC stdio protocol parsing and `STDOUT` redirection isolation.
- Platform compilation verification script (`tmp/verify_platforms.sh`) for host-native targets.

## [0.1.0-alpha.3] - 2026-06-04

### Added
- Dynamic download and automatic model management of the local ONNX embedding model to XDG Cache when vector search is triggered.
- A visual chunk-streaming progress bar outputting to STDERR during model downloads.
- Verification checks for model integrity (SHA-256 hash validation) before finalizing installations.
- Network-isolated container sandboxing for recursive CLI help extraction in `cmdh-extractor` using Podman/Docker.
- Support for SQLite WAL (Write-Ahead Logging) mode and synchronous=NORMAL on all open database connections for concurrent reader-writer safety.
- Safe hot database updates via SQLite Online Backup API (Backup) preventing file inode inconsistencies during updates.

### Changed
- Replaced the hybrid search RRF vector ranking queries with the `sqlite-vec` recommended KNN MATCH syntax, improving large-dataset query performance.
- Optimized FTS5 phrase search routing by prioritizing exact implicit AND queries before falling back to logical OR queries.
- Refactored default embedding model path to comply with XDG Base Directory specification cache directories.
- Converted Model Context Protocol (`cmdhub-mcp`) server main loop to support async runtime execution.

## [0.1.0-alpha.2] - 2026-06-04

### Added
- Shell autocompletion support for `bash`, `zsh`, and `fish` via `cmdh completions <shell>` subcommand.
- Expanded ACI schema definition (`AciCommandContract`) and SQLite database tables with `docker_image`, `script_url`, and `source_url`.
- Added support for Windows host platform detection and smart `scoop` installation command recommendation.

### Changed
- Updated internal database conversions and row indexing code (`DbArgument` and `DbAciRecord`) in `db.rs` to support expanded fields seamlessly.
- Configured workspace dependencies for `clap_complete` library.

### Fixed
- Fixed CLI integration tests and mock skill registration values to include extended schema fields.

## [0.1.0-alpha.1] - 2026-06-02

### Added
- Initial project scaffold with Rust workspace (cmdhub-cli, cmdhub-mcp, cmdhub-shared)
- ACI JSON Schema definition (`schemas/aci-command-contract.schema.json`)
- CI pipeline: fmt, clippy, test, docs, schema-lint, typos
- Release pipeline: cross-compile 6 targets, GitHub Release, crates.io publish
- Docker multi-stage build with GHCR publish
- Pre-commit hooks (fmt, clippy, test, conventional commits, typos)
- Dependabot configuration for Cargo and GitHub Actions
- Issue templates: Bug Report, Feature Request, New Tool Submission
- PR template with mandatory checklist
- Security policy (SECURITY.md)
- Release Drafter configuration
- AGENTS.md for AI coding agent instructions
- Contributing guidelines (CONTRIBUTING.md)

### Changed

### Deprecated

### Removed

### Fixed

### Security

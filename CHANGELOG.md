# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0-alpha.3] - 2026-06-04

### Added
- Dynamic download and automatic model management of the local ONNX embedding model to XDG Cache when vector search is triggered.
- A visual chunk-streaming progress bar outputting to STDERR during model downloads.
- Verification checks for model integrity (SHA-256 hash validation) before finalizing installations.

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

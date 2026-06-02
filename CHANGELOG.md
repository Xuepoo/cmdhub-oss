# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

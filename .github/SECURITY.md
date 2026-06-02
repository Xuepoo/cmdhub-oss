# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |

## Reporting a Vulnerability

**DO NOT** open a public GitHub issue for security vulnerabilities.

Instead, please report them via GitHub Security Advisories (preferred).

### What to include:
1. Description of the vulnerability
2. Steps to reproduce
3. Potential impact
4. Suggested fix (if any)

### Response timeline:
- **Acknowledgment**: within 48 hours
- **Initial assessment**: within 1 week
- **Fix release**: within 2 weeks for critical issues

## Security Design Principles

CmdHub follows these security principles:
- **gVisor sandboxing** for all user-submitted code execution
- **Ed25519 signature verification** for all distributed database files
- **Atomic file replacement** to prevent corruption
- **Dangerous command blocking** in the local safety guardrail

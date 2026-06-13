# CmdHub Configuration Specification

This document details the configuration system of the CmdHub client (`cmdh`), including standard directories, format presets, OS detector mapping, and package manager fallback mechanisms.

---

## 1. Quick Start Setup

To initialize a default configuration file dynamically customized for your host environment, run:

```bash
cmdh init
```

By default, the setup wizard will detect your host operating system and write a commented `config.toml` file to your XDG configuration path (usually `~/.config/cmdhub/config.toml`).

*Note: If a configuration file already exists, the command will exit gracefully to prevent accidental overrides. To force-overwrite existing configurations, use the `--force` flag:*

```bash
cmdh init --force
```

---

## 2. Configuration Schema (`config.toml`)

Below is a complete description of the parameters available inside `config.toml`:

```toml
# The API endpoint for online database synchronization
api_url = "https://api.cmdhub.org"

[output]
# Formatting preset for STDOUT query output JSON representation.
# Available options:
# - "full"    : Output all ACI contract metadata and a single resolved install_command.
# - "usage"   : Output only "cmd_path" and "example_template" (ideal for quick usage checks).
# - "minimal" : Output only the "cmd_path" string.
mode = "full"

[install]
# Manual Operating System override.
# By default, this key is omitted or commented out, letting cmdh dynamically detect your host OS.
# Supported overrides: "macos", "arch", "ubuntu", "debian", "fedora", "centos", "rhel", "gentoo", "alpine", "opensuse", "nixos".
# os = "arch"

# Preferred order of fallback package managers when no system-level installer command is found.
# cmdh will check for the availability of these keys in order.
package_managers = ["uv", "npm", "cargo", "go"]
```

---

## 3. Command Line Format Overrides

You can temporarily override your default `config.toml` output preset using Clap flags on the command line during a search:

- **Force Full output:**
  ```bash
  cmdh search "git clone" --full
  ```
- **Force Usage only:**
  ```bash
  cmdh search "git clone" --usage-only # or -u
  ```
- **Force Minimal output:**
  ```bash
  cmdh search "git clone" --minimal # or -m
  ```

*These command-line flags are mutually exclusive.*

---

## 4. Supported Platforms & Package Managers

When compiling or executing search results, CmdHub resolves the installation directives based on the host OS detector. The detector strips surrounding quotes and resolves derivative systems (via `ID_LIKE` keys in `/etc/os-release`).

| Host OS / Derivative | System Package Manager Resolved |
| :--- | :--- |
| **macOS** | `brew` |
| **Arch Linux** / Manjaro / CachyOS | `pacman` |
| **Debian** / **Ubuntu** / Mint / Pop!_OS | `apt` |
| **Fedora** | `dnf` |
| **CentOS** / **RHEL** / Rocky Linux / AlmaLinux | `yum` (or `dnf`) |
| **Gentoo** | `emerge` |
| **Alpine Linux** | `apk` |
| **OpenSUSE** / SUSE Enterprise | `zypper` |
| **NixOS** | `nix-env` |

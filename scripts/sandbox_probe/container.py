"""Thin podman wrappers for the two-stage install-probe."""

import subprocess
from pathlib import Path

BASE_IMAGE = "arch-probe-base:latest"


def _run(args: list[str], timeout: int) -> subprocess.CompletedProcess:
    return subprocess.run(args, capture_output=True, text=True, timeout=timeout)


def install(pkg: str, container_name: str, install_timeout: int = 180) -> bool:
    """Stage 1: pacman -S (network ON), then commit to an intermediate image.
    Runs the package manager ONLY — never executes the installed binary. Tries the
    official repos first, then yay (AUR). Returns True on success."""
    cmd = (
        f"sudo pacman -S --noconfirm {pkg} || sudo -u builder yay -S --noconfirm {pkg}"
    )
    r = _run(
        ["podman", "run", "--name", container_name, BASE_IMAGE, "bash", "-c", cmd],
        timeout=install_timeout,
    )
    if r.returncode != 0:
        _run(["podman", "rm", "-f", container_name], timeout=30)
        return False
    c = _run(
        ["podman", "commit", container_name, f"probe-int:{container_name}"], timeout=60
    )
    _run(["podman", "rm", "-f", container_name], timeout=30)
    return c.returncode == 0


def resolve_pkg_in_container(binary: str, timeout: int = 60) -> str | None:
    """pkg-resolution fallback: ask `pacman -F` which package owns the binary."""
    r = _run(
        [
            "podman",
            "run",
            "--rm",
            BASE_IMAGE,
            "bash",
            "-c",
            f"pacman -F --machinereadable usr/bin/{binary} 2>/dev/null | head -1",
        ],
        timeout=timeout,
    )
    parts = r.stdout.split("\0") if r.stdout else []
    return parts[1] if len(parts) >= 2 else None


def probe(
    container_name: str,
    extractor_host_path: Path,
    scratch_dir: Path,
    targets_json: Path,
    probe_timeout: int = 120,
) -> bool:
    """Stage 2: run the mounted cmdh-extractor inside the committed image with
    --network none. The container is the isolation boundary; CMDH_NO_SANDBOX=1 tells
    the extractor not to nest another sandbox. Writes to the mounted scratch dir."""
    img = f"probe-int:{container_name}"
    r = _run(
        [
            "podman",
            "run",
            "--rm",
            "--network",
            "none",
            "--memory",
            "700m",
            "-e",
            "CMDH_NO_SANDBOX=1",
            "-e",
            "XDG_CONFIG_HOME=/probe/cfg",
            "-e",
            "XDG_DATA_HOME=/probe/data",
            "-v",
            f"{extractor_host_path}:/usr/local/bin/cmdh-extractor:ro",
            "-v",
            f"{targets_json}:/probe/cfg/cmdhub/targets.json:ro",
            "-v",
            f"{scratch_dir}:/probe/data",
            img,
            "/usr/local/bin/cmdh-extractor",
        ],
        timeout=probe_timeout,
    )
    _run(["podman", "rmi", "-f", img], timeout=60)
    return r.returncode == 0


def clear_cache() -> None:
    """Bound the 20 GB disk between batches."""
    _run(
        [
            "bash",
            "-c",
            "yes | sudo pacman -Scc 2>/dev/null; podman image prune -f",
        ],
        timeout=120,
    )

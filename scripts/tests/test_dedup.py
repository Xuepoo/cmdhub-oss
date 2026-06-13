import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import build_db  # noqa: E402


def test_canonical_key_collapses_fused_subcommand_but_not_compound():
    assert build_db._canonical_tool("podman-image") == "podman"
    assert build_db._canonical_tool("podman-images") == "podman"
    assert build_db._canonical_tool("podman-compose") == "podman-compose"  # compound tool: NOT collapsed
    assert build_db._canonical_tool("docker-compose") == "docker-compose"
    assert build_db._canonical_tool("docker-volume") == "docker"
    assert build_db._canonical_tool("ripgrep") == "ripgrep"
    assert build_db._canonical_tool("git") == "git"


def test_dedup_prefers_probe_and_unions_topics():
    args = [
        {"cmd_path": "podman-image.prune", "app_id": "a1", "node_name": "prune",
         "description": "remove unused images", "risk_level": "dangerous",
         "topics": "podman cleanup", "provenance": "inferred"},
        {"cmd_path": "podman.image.prune", "app_id": "a2", "node_name": "prune",
         "description": "Remove unused images.", "risk_level": "dangerous",
         "topics": "podman prune verified", "provenance": "probe"},
        {"cmd_path": "podman-compose.images", "app_id": "a3", "node_name": "images",
         "description": "list", "risk_level": "safe",
         "topics": "compose", "provenance": "inferred"},
    ]
    out = build_db._canonicalize_and_dedup(args, apps=[])
    prunes = [a for a in out if a["node_name"] == "prune"]
    assert len(prunes) == 1                      # the two prune rows merged into one
    assert prunes[0]["provenance"] == "probe"    # probe version kept
    assert "cleanup" in prunes[0]["topics"]      # inferred topics unioned in
    assert "verified" in prunes[0]["topics"]
    assert any(a["node_name"] == "images" for a in out)  # compose NOT merged away


def test_canonical_path_unfolds_fused_roots_and_stems_plurals():
    assert build_db._canonical_path("podman-image.prune") == "podman.image.prune"
    assert build_db._canonical_path("podman-images.filter") == "podman.image.filter"
    assert build_db._canonical_path("podman.image.prune") == "podman.image.prune"
    assert build_db._canonical_path("podman-compose.images") == "podman-compose.image"
    assert build_db._canonical_path("git.log") == "git.log"


def test_dedup_never_merges_distinct_subtrees_with_same_leaf():
    # image.prune and container.prune are BOTH real podman commands sharing the leaf
    # "prune" — the full-path key must keep them apart (regression for the leaf-key bug).
    args = [
        {"cmd_path": "podman.image.prune", "app_id": "a", "node_name": "prune",
         "description": "remove unused images", "risk_level": "dangerous",
         "topics": "", "provenance": "inferred"},
        {"cmd_path": "podman.container.prune", "app_id": "a", "node_name": "prune",
         "description": "remove stopped containers", "risk_level": "dangerous",
         "topics": "", "provenance": "inferred"},
    ]
    out = build_db._canonicalize_and_dedup(args, apps=[])
    assert len(out) == 2


def test_dedup_keeps_distinct_subcommands_apart():
    args = [
        {"cmd_path": "git.log", "app_id": "g", "node_name": "log",
         "description": "show history", "risk_level": "safe", "topics": "", "provenance": "probe"},
        {"cmd_path": "git.show", "app_id": "g", "node_name": "show",
         "description": "show object", "risk_level": "safe", "topics": "", "provenance": "probe"},
    ]
    out = build_db._canonicalize_and_dedup(args, apps=[])
    assert len(out) == 2  # different leaves never merge


def test_dedup_prefers_canonical_path_row_over_fragment():
    # Both inferred: keep the row whose own path IS canonical (`podman.image.prune`,
    # real binary `podman`) over the fused fragment (`podman-image.prune`, whose
    # binary `podman-image` doesn't exist and would break `cmdh run`) — even when the
    # fragment's app is more popular.
    args = [
        {"cmd_path": "podman-image.prune", "app_id": "frag", "node_name": "prune",
         "description": "remove unused images", "risk_level": "dangerous",
         "topics": "fragtopic", "provenance": "inferred"},
        {"cmd_path": "podman.image.prune", "app_id": "canon", "node_name": "prune",
         "description": "Remove unused images.", "risk_level": "dangerous",
         "topics": "canontopic", "provenance": "inferred"},
    ]
    apps = [{"app_id": "frag", "popularity": 0.9}, {"app_id": "canon", "popularity": 0.2}]
    out = build_db._canonicalize_and_dedup(args, apps)
    assert len(out) == 1
    assert out[0]["cmd_path"] == "podman.image.prune"
    assert "fragtopic" in out[0]["topics"]


def test_dedup_falls_back_to_popularity_when_no_probe():
    args = [
        {"cmd_path": "tool-image.prune", "app_id": "lo", "node_name": "prune",
         "description": "d1", "risk_level": "safe", "topics": "x", "provenance": "inferred"},
        {"cmd_path": "tool.image.prune", "app_id": "hi", "node_name": "prune",
         "description": "d2", "risk_level": "safe", "topics": "y", "provenance": "inferred"},
    ]
    apps = [{"app_id": "lo", "popularity": 0.1}, {"app_id": "hi", "popularity": 0.9}]
    out = build_db._canonicalize_and_dedup(args, apps)
    assert len(out) == 1
    assert out[0]["app_id"] == "hi"              # higher-popularity row kept
    assert "x" in out[0]["topics"] and "y" in out[0]["topics"]

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import validate_db  # noqa: E402


def test_flags_inferred_example_invoking_wrong_binary():
    rows = [
        # probe row: never flagged, even with an odd example
        {"cmd_path": "podman.images", "node_name": "images", "provenance": "probe",
         "example_template": "podman images --filter dangling=true"},
        # the real-world fabrication: contract root is the fused 'podman-images'
        # but the example invokes plain 'podman' — the contract path can't exist
        {"cmd_path": "podman-images.filter", "node_name": "filter", "provenance": "inferred",
         "example_template": "podman images filter --filter dangling=true"},
    ]
    warns = validate_db.check_fabricated_examples(rows)
    assert any("podman-images.filter" in w for w in warns)
    assert not any("podman.images:" in w for w in warns)  # probe row untouched


def test_flags_inferred_example_with_unknown_subcommand_word():
    rows = [
        {"cmd_path": "tool.list", "node_name": "list", "provenance": "inferred",
         "example_template": "tool list everything --json"},
    ]
    warns = validate_db.check_fabricated_examples(rows)
    assert any("tool.list" in w and "everything" in w for w in warns)


def test_clean_inferred_examples_not_flagged():
    rows = [
        {"cmd_path": "aws.ec2.describe-instances", "node_name": "describe-instances",
         "provenance": "inferred",
         "example_template": "aws ec2 describe-instances --region {{region}}"},
        {"cmd_path": "git.log", "node_name": "log", "provenance": "inferred",
         "example_template": "git log --oneline"},
        # root contract demoing a typical sub usage is legitimate
        {"cmd_path": "zoxide", "node_name": "zoxide", "provenance": "inferred",
         "example_template": "zoxide query {{dir}}"},
        # flag-style leaf: example goes straight to flags
        {"cmd_path": "rm.-r", "node_name": "-r", "provenance": "inferred",
         "example_template": "rm -r {{dir}}"},
        # empty example: nothing to check
        {"cmd_path": "fd.hidden", "node_name": "hidden", "provenance": "inferred",
         "example_template": None},
    ]
    assert validate_db.check_fabricated_examples(rows) == []

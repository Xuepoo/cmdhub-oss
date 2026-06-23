import importlib.util
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("checkpoint", ROOT / "checkpoint.py")
cp = importlib.util.module_from_spec(spec)
sys.modules["checkpoint"] = cp
spec.loader.exec_module(cp)


def test_set_get_roundtrip(tmp_path):
    db = cp.Checkpoint(tmp_path / "ck.db")
    db.set("ncdu", status="merged", reason="probe-ok")
    assert db.get("ncdu") == ("merged", "probe-ok", 1)


def test_is_done_only_terminal(tmp_path):
    db = cp.Checkpoint(tmp_path / "ck.db")
    db.set("a", status="merged", reason="probe-ok")
    db.set("b", status="perma-failed", reason="install-fail")
    db.set("c", status="no-subcommands", reason="no-subcommands")
    db.set("d", status="probed", reason="probe-ok")
    assert db.is_done("a") and db.is_done("b") and db.is_done("c")
    assert not db.is_done("d") and not db.is_done("unseen")


def test_attempts_increment(tmp_path):
    db = cp.Checkpoint(tmp_path / "ck.db")
    db.set("x", status="failed", reason="timeout")
    db.set("x", status="failed", reason="timeout")
    assert db.get("x")[2] == 2

import sqlite3
import importlib.util
import pathlib
spec = importlib.util.spec_from_file_location(
    "reprobe_batch", pathlib.Path(__file__).parent / "reprobe_batch.py")
rb = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rb)


def _db():
    c = sqlite3.connect(":memory:")
    c.execute("CREATE TABLE arguments (cmd_path TEXT, app_id TEXT, node_type TEXT, provenance TEXT)")
    c.executemany("INSERT INTO arguments VALUES (?,?,?,?)", [
        ("npm", "org.tldr.npm", "root", "probe"),
        ("npm.access", "org.tldr.npm", "sub", "probe"),
        ("npm.prune", "org.tldr.npm", "sub", "probe"),
        ("git", "org.archlinux.git", "root", "probe"),
    ])
    return c


def test_resolve_app_id_unique():
    assert rb.resolve_app_id(_db(), "npm") == "org.tldr.npm"


def test_resolve_app_id_missing_root():
    assert rb.resolve_app_id(_db(), "doesnotexist_x") is None


def test_subtree_size_counts_root_and_children():
    assert rb.subtree_size(_db(), "npm") == 3   # npm + npm.access + npm.prune


def test_subtree_size_single():
    assert rb.subtree_size(_db(), "git") == 1

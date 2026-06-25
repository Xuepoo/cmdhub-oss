import sqlite3
import importlib.util
import pathlib

spec = importlib.util.spec_from_file_location(
    "enrich_topics", pathlib.Path(__file__).parent / "enrich_topics.py")
et = importlib.util.module_from_spec(spec)
spec.loader.exec_module(et)


def test_parse_topics_cleans_and_dedups():
    raw = "NPM, node,, npm , Package, dependency, dependency"
    assert et.parse_topics(raw) == "npm, node, package, dependency"


def test_parse_topics_caps_at_15():
    raw = ", ".join(f"w{i}" for i in range(30))
    assert len(et.parse_topics(raw).split(", ")) == 15


def test_build_prompt_anchors_to_description():
    sysmsg, user = et.build_prompt("npm.uninstall", "Remove a package", "npm")
    assert "npm uninstall" in user
    assert "Remove a package" in user
    assert "npm" in user
    assert "do not invent" in sysmsg.lower()


def _db():
    c = sqlite3.connect(":memory:")
    c.execute("CREATE TABLE apps (app_id TEXT, popularity REAL)")
    c.execute("CREATE TABLE arguments (cmd_path TEXT, app_id TEXT, node_type TEXT, provenance TEXT, description TEXT, topics TEXT)")
    c.executemany("INSERT INTO apps VALUES (?,?)", [("a.npm", 0.9), ("a.obscure", 0.1)])
    c.executemany("INSERT INTO arguments VALUES (?,?,?,?,?,?)", [
        ("npm.uninstall", "a.npm", "sub", "probe", "Remove a package", None),
        ("npm.install", "a.npm", "sub", "probe", "Install deps", ""),
        ("npm.access", "a.npm", "sub", "probe", "Manage access", "npm access existing"),
        ("obscure.x", "a.obscure", "sub", "probe", "do thing", None),
    ])
    return c


def test_select_targets_only_empty_topics():
    rows = et.select_targets(_db(), tools=None, min_pop=0.0, limit=100)
    paths = {r[0] for r in rows}
    assert paths == {"npm.uninstall", "npm.install", "obscure.x"}


def test_select_targets_min_pop_and_tools():
    rows = et.select_targets(_db(), tools=["npm"], min_pop=0.5, limit=100)
    paths = {r[0] for r in rows}
    assert paths == {"npm.uninstall", "npm.install"}

import importlib.util
import json
import pathlib
import sqlite3
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("queue", ROOT / "queue.py")
q = importlib.util.module_from_spec(spec)
sys.modules["queue"] = q
spec.loader.exec_module(q)


def test_pkg_from_install_instructions():
    inst = json.dumps(
        {
            "pacman": "pacman -S maven",
            "apt": "apt install maven",
            "brew": "brew install maven",
        }
    )
    assert q.resolve_pkg("mvn", inst) == "maven"


def test_pkg_strips_bin_suffix_and_path():
    inst = json.dumps(
        {"emerge": "emerge dev-java/maven-bin", "pacman": "pacman -S maven"}
    )
    assert q.resolve_pkg("maven", inst) == "maven"


def test_pkg_none_when_no_install():
    assert q.resolve_pkg("ncdu", None) is None
    assert q.resolve_pkg("ncdu", "{}") is None


def _mk_master(tmp_path):
    db = tmp_path / "m.db"
    c = sqlite3.connect(db)
    c.execute(
        "CREATE TABLE apps (app_id TEXT PRIMARY KEY, name TEXT, install_instructions TEXT, popularity REAL)"
    )
    c.execute(
        "CREATE TABLE arguments (cmd_path TEXT, app_id TEXT, node_type TEXT, provenance TEXT)"
    )
    rows = [
        ("org.x.ncdu", "ncdu", '{"pacman":"pacman -S ncdu"}', 0.9),
        ("org.x.git", "git", '{"pacman":"pacman -S git"}', 0.95),
    ]
    for aid, name, inst, pop in rows:
        c.execute("INSERT INTO apps VALUES (?,?,?,?)", (aid, name, inst, pop))
    c.execute("INSERT INTO arguments VALUES ('ncdu','org.x.ncdu','root','inferred')")
    c.execute("INSERT INTO arguments VALUES ('git','org.x.git','root','probe')")
    c.commit()
    c.close()
    return str(db)


def test_load_workitems_filters_and_orders(tmp_path):
    master = _mk_master(tmp_path)
    items = q.load_workitems(master, min_pop=0.8)
    assert [i.binary for i in items] == ["ncdu"]  # git excluded (already probe)
    assert items[0].app_id == "org.x.ncdu"
    assert items[0].pkg == "ncdu"

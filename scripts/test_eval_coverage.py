import sqlite3, sys, importlib.util, pathlib

spec = importlib.util.spec_from_file_location(
    "ec", str(pathlib.Path(__file__).parent / "eval_coverage.py"))
ec = importlib.util.module_from_spec(spec)
sys.modules["ec"] = ec          # register so dataclass annotation resolution works
spec.loader.exec_module(ec)


def _mk_db(tmp_path):
    db = tmp_path / "r.db"
    con = sqlite3.connect(db)
    con.execute("CREATE TABLE arguments (cmd_path TEXT, description TEXT, "
                "node_type TEXT, provenance TEXT)")
    con.executemany(
        "INSERT INTO arguments VALUES (?,?,?,?)",
        [("tar", "GNU tar archives files", "root", "probe"),
         ("tar.create", "create an archive", "sub", "probe"),
         ("foo", "inferred junk", "root", "inferred")],
    )
    con.commit(); con.close()
    return str(db)


def test_load_commands_only_probe(tmp_path):
    db = _mk_db(tmp_path)
    cmds = ec.load_commands(db)
    paths = {c.cmd_path for c in cmds}
    assert paths == {"tar", "tar.create"}        # inferred 'foo' excluded
    assert all(c.description for c in cmds)

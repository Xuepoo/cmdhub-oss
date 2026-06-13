import json
import sqlite3
import subprocess
import sys
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
import build_db  # noqa: E402


def test_arguments_ddl_has_provenance():
    assert "provenance" in build_db._DDL
    assert "DEFAULT 'inferred'" in build_db._DDL


def test_export_carries_provenance(tmp_path):
    db = tmp_path / "src.db"
    c = sqlite3.connect(db)
    c.executescript(
        "CREATE TABLE apps(app_id TEXT, name TEXT, os_aliases TEXT, install_instructions TEXT, popularity REAL);"
        "CREATE TABLE arguments(cmd_path TEXT, app_id TEXT, node_name TEXT, node_type TEXT,"
        " description TEXT, risk_level TEXT, example_template TEXT, docker_image TEXT,"
        " script_url TEXT, source_url TEXT, topics TEXT, provenance TEXT DEFAULT 'inferred');"
        "INSERT INTO apps VALUES ('t','tool',NULL,NULL,0.5);"
        "INSERT INTO arguments(cmd_path,app_id,node_name,node_type,description,risk_level,provenance)"
        " VALUES ('tool','t','tool','root','d','safe','probe');"
    )
    c.commit()
    c.close()
    out = tmp_path / "export.json"
    subprocess.run(
        [sys.executable, str(SCRIPTS / "export_sqlite.py"), "--db", str(db), "--out", str(out)],
        check=True,
    )
    data = json.loads(out.read_text())
    assert data["arguments"][0]["provenance"] == "probe"


def test_export_defaults_inferred_when_column_absent(tmp_path):
    db = tmp_path / "old.db"
    c = sqlite3.connect(db)
    c.executescript(
        "CREATE TABLE apps(app_id TEXT, name TEXT, os_aliases TEXT, install_instructions TEXT, popularity REAL);"
        "CREATE TABLE arguments(cmd_path TEXT, app_id TEXT, node_name TEXT, node_type TEXT,"
        " description TEXT, risk_level TEXT, example_template TEXT, docker_image TEXT,"
        " script_url TEXT, source_url TEXT, topics TEXT);"
        "INSERT INTO apps VALUES ('t','tool',NULL,NULL,0.5);"
        "INSERT INTO arguments(cmd_path,app_id,node_name,node_type,description,risk_level)"
        " VALUES ('tool','t','tool','root','d','safe');"
    )
    c.commit()
    c.close()
    out = tmp_path / "export.json"
    subprocess.run(
        [sys.executable, str(SCRIPTS / "export_sqlite.py"), "--db", str(db), "--out", str(out)],
        check=True,
    )
    data = json.loads(out.read_text())
    assert data["arguments"][0]["provenance"] == "inferred"

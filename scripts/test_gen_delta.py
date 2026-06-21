import sqlite3, importlib.util, pathlib, sys
spec = importlib.util.spec_from_file_location(
    "bd", str(pathlib.Path(__file__).parent / "build_db.py"))
bd = importlib.util.module_from_spec(spec); sys.modules["bd"] = bd
spec.loader.exec_module(bd)


def test_model_id_helper():
    # _model_id returns a stable id (basename) for a model path
    assert bd._model_id("/opt/cmdhub/bge-small-en-v1.5.onnx") == "bge-small-en-v1.5"
    assert bd._model_id("/x/bge-micro-v2.onnx") == "bge-micro-v2"


def _mk_db(tmp_path, name, rows, model="bge-small-en-v1.5"):
    # rows: list of (cmd_path, description, vec_bytes)
    import sqlite_vec
    db = tmp_path / name
    con = sqlite3.connect(db)
    con.enable_load_extension(True)
    sqlite_vec.load(con)
    con.execute("CREATE TABLE sync_meta (key TEXT PRIMARY KEY, value TEXT)")
    con.execute("INSERT INTO sync_meta VALUES ('embed_model', ?)", (model,))
    con.execute("CREATE TABLE arguments (cmd_path TEXT, app_id TEXT, description TEXT, topics TEXT)")
    con.execute("CREATE VIRTUAL TABLE commands_vec USING vec0(cmd_path TEXT PRIMARY KEY, embedding float[384])")
    for cp, desc, vec in rows:
        con.execute("INSERT INTO arguments (cmd_path, app_id, description, topics) VALUES (?,?,?,'')",
                    (cp, "app." + cp, desc))
        con.execute("INSERT INTO commands_vec (cmd_path, embedding) VALUES (?,?)", (cp, vec))
    con.commit(); con.close()
    return str(db)


def test_load_prev_vectors_matches_by_embed_text(tmp_path):
    import struct
    v = struct.pack("384f", *([0.1] * 384))
    prev = _mk_db(tmp_path, "prev.db", [("tar", "archive files", v)])
    reuse, model = bd._load_reuse_vectors(prev)
    # keyed by (cmd_path, embed_text) -> vec bytes; embed_text = "tar. archive files"
    assert reuse[("tar", "tar. archive files")] == v
    assert model == "bge-small-en-v1.5"


def test_load_prev_vectors_model_mismatch_returns_none(tmp_path):
    import struct
    v = struct.pack("384f", *([0.1] * 384))
    prev = _mk_db(tmp_path, "prevm.db", [("tar", "archive files", v)], model="bge-micro-v2")
    reuse, model = bd._load_reuse_vectors(prev)
    assert model == "bge-micro-v2"   # caller compares to current and refuses reuse

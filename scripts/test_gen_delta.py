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


def _load_gd():
    import importlib.util
    gd_spec = importlib.util.spec_from_file_location(
        "gd", str(pathlib.Path(__file__).parent / "gen_delta.py"))
    gd = importlib.util.module_from_spec(gd_spec); sys.modules["gd"] = gd
    gd_spec.loader.exec_module(gd)
    return gd


def _mk_full_db(tmp_path, name, apps, args, vecs):
    import sqlite_vec
    db = tmp_path / name
    c = sqlite3.connect(db); c.enable_load_extension(True); sqlite_vec.load(c)
    c.execute("CREATE TABLE apps (app_id TEXT PRIMARY KEY, name TEXT, install_instructions TEXT)")
    c.execute("CREATE TABLE arguments (cmd_path TEXT, app_id TEXT, node_name TEXT, node_type TEXT, description TEXT, risk_level TEXT, example_template TEXT, docker_image TEXT, script_url TEXT, source_url TEXT)")
    c.execute("CREATE VIRTUAL TABLE commands_vec USING vec0(cmd_path TEXT PRIMARY KEY, embedding float[384])")
    for a in apps:
        c.execute("INSERT INTO apps VALUES (?,?,?)", a)
    for g in args:
        c.execute("INSERT INTO arguments (cmd_path,app_id,node_name,node_type,description,risk_level,example_template,docker_image,script_url,source_url) VALUES (?,?,?,?,?,?,?,?,?,?)", g)
    for cp, vec in vecs:
        c.execute("INSERT INTO commands_vec (cmd_path, embedding) VALUES (?,?)", (cp, vec))
    c.commit(); c.close(); return str(db)


def test_diff_payload(tmp_path):
    import struct
    gd = _load_gd()
    v = struct.pack("384f", *([0.2] * 384))
    v2 = struct.pack("384f", *([0.9] * 384))
    prev = _mk_full_db(tmp_path, "p.db",
        [("a.keep", "keep", None), ("a.del", "del", None)],
        [("keep", "a.keep", "keep", "root", "old desc", "safe", None, None, None, None),
         ("gone", "a.del", "gone", "root", "x", "safe", None, None, None, None)],
        [("keep", v), ("gone", v)])
    new = _mk_full_db(tmp_path, "n.db",
        [("a.keep", "keep", None), ("a.new", "new", None)],
        [("keep", "a.keep", "keep", "root", "NEW desc", "safe", None, None, None, None),
         ("fresh", "a.new", "fresh", "root", "y", "safe", None, None, None, None)],
        [("keep", v2), ("fresh", v)])
    payload = gd.diff(prev, new)
    assert payload["deleted_apps"] == ["a.del"]
    assert {a["app_id"] for a in payload["apps"]} == {"a.keep", "a.new"}  # keep changed, new added
    cps = {g["cmd_path"] for g in payload["arguments"]}
    assert cps == {"keep", "fresh"}                  # changed + new args
    vc = {x["cmd_path"]: x["embedding"] for x in payload["command_vecs"]}
    assert set(vc) == {"keep", "fresh"}              # changed + new vecs
    assert len(vc["keep"]) == 384 and abs(vc["keep"][0] - 0.9) < 1e-5   # float32, new value


def test_diff_within_app_command_deletion(tmp_path):
    # A command removed from an app whose row is otherwise unchanged: the app must
    # be emitted (dirty) with ONLY its surviving commands, so the client's
    # wipe+reinsert drops the removed command. This is the app-scoped invariant.
    import struct
    gd = _load_gd()
    v = struct.pack("384f", *([0.3] * 384))
    prev = _mk_full_db(tmp_path, "p2.db",
        [("a.git", "git", None)],
        [("git", "a.git", "git", "root", "vcs", "safe", None, None, None, None),
         ("git.svn", "a.git", "svn", "sub", "subversion bridge", "safe", None, None, None, None)],
        [("git", v), ("git.svn", v)])
    new = _mk_full_db(tmp_path, "n2.db",
        [("a.git", "git", None)],
        [("git", "a.git", "git", "root", "vcs", "safe", None, None, None, None)],
        [("git", v)])
    payload = gd.diff(prev, new)
    assert payload["deleted_apps"] == []
    assert {a["app_id"] for a in payload["apps"]} == {"a.git"}   # dirty: command removed
    assert {g["cmd_path"] for g in payload["arguments"]} == {"git"}  # only survivor
    assert {x["cmd_path"] for x in payload["command_vecs"]} == {"git"}


def test_diff_noop_when_identical(tmp_path):
    import struct
    gd = _load_gd()
    v = struct.pack("384f", *([0.4] * 384))
    rows_apps = [("a.ls", "ls", None)]
    rows_args = [("ls", "a.ls", "ls", "root", "list files", "safe", None, None, None, None)]
    rows_vecs = [("ls", v)]
    prev = _mk_full_db(tmp_path, "p3.db", rows_apps, rows_args, rows_vecs)
    new = _mk_full_db(tmp_path, "n3.db", rows_apps, rows_args, rows_vecs)
    payload = gd.diff(prev, new)
    assert payload == {"deleted_apps": [], "apps": [], "arguments": [], "command_vecs": []}


def test_sign_file_helper(tmp_path):
    import importlib.util
    s_spec = importlib.util.spec_from_file_location(
        "sd", str(pathlib.Path(__file__).parent / "sign_db.py"))
    sd = importlib.util.module_from_spec(s_spec); sys.modules["sd"] = sd
    s_spec.loader.exec_module(sd)
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    kp = tmp_path / "k.bin"
    kp.write_bytes(Ed25519PrivateKey.generate().private_bytes_raw())
    payload = tmp_path / "delta.json.zst"; payload.write_bytes(b"\x28\xb5\x2f\xfd\x00")
    sha, sig = sd.sign_file(str(payload), str(kp))
    assert len(sig) == 64 and len(sha) == 64  # ed25519 sig, hex sha256
    # signature verifies against the key's public half over SHA256(blob)
    import hashlib
    pk = Ed25519PrivateKey.from_private_bytes(kp.read_bytes()).public_key()
    pk.verify(sig, hashlib.sha256(payload.read_bytes()).digest())  # raises if bad


def _load_verify():
    import importlib.util
    spec = importlib.util.spec_from_file_location(
        "ve", str(pathlib.Path(__file__).parent / "verify_delta_equivalence.py"))
    ve = importlib.util.module_from_spec(spec); sys.modules["ve"] = ve
    spec.loader.exec_module(ve)
    return ve


def test_incremental_equals_full(tmp_path):
    # Core correctness guarantee: applying the delta to prev yields exactly the new DB.
    # Exercises add (jq), change (grep desc+vec), delete-app (curl), within-app
    # command delete (git loses git.svn). Vectors fabricated (no model needed).
    import struct
    ve = _load_verify()
    va = struct.pack("384f", *([0.1] * 384))
    vb = struct.pack("384f", *([0.7] * 384))
    prev = _mk_full_db(tmp_path, "pe.db",
        [("a.tar", "tar", None), ("a.grep", "grep", None),
         ("a.curl", "curl", "brew install curl"), ("a.git", "git", None)],
        [("tar", "a.tar", "tar", "root", "archive", "safe", None, None, None, None),
         ("grep", "a.grep", "grep", "root", "search", "safe", None, None, None, None),
         ("curl", "a.curl", "curl", "root", "transfer", "safe", None, None, None, None),
         ("git", "a.git", "git", "root", "vcs", "safe", None, None, None, None),
         ("git.svn", "a.git", "svn", "sub", "svn bridge", "safe", None, None, None, None)],
        [("tar", va), ("grep", va), ("curl", va), ("git", va), ("git.svn", va)])
    new = _mk_full_db(tmp_path, "ne.db",
        [("a.tar", "tar", None), ("a.grep", "grep", None),
         ("a.git", "git", None), ("a.jq", "jq", None)],
        [("tar", "a.tar", "tar", "root", "archive", "safe", None, None, None, None),
         ("grep", "a.grep", "grep", "root", "search regex in files", "safe", None, None, None, None),
         ("git", "a.git", "git", "root", "vcs", "safe", None, None, None, None),
         ("jq", "a.jq", "jq", "root", "process json", "safe", None, None, None, None)],
        [("tar", va), ("grep", vb), ("git", va), ("jq", va)])
    problems = ve.verify(str(prev), str(new))
    assert problems == [], problems

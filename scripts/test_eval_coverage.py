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


# --- T2: name_echo_filter ---
def test_name_echo_filter():
    assert ec.name_echo_filter("tar", "compress a folder into an archive") is True
    assert ec.name_echo_filter("tar", "create a tar archive") is False
    assert ec.name_echo_filter("kubectl.scale", "resize a deployment") is True
    assert ec.name_echo_filter("kubectl.scale", "scale a deployment") is False
    assert ec.name_echo_filter("gh.pr.create", "open a pull request") is True
    assert ec.name_echo_filter("gh.pr.create", "gh create a pr") is False


# --- T3: evaluate ---
def _results(*paths):
    return [{"cmd_path": p, "verified": True, "description": ""} for p in paths]


def test_evaluate_pass_nearmiss_fail():
    v = ec.evaluate("tar", _results("tar", "zip", "gzip"), k=5)
    assert v.status == "pass" and v.rank == 1
    res = _results(*[f"x{i}" for i in range(6)], "tar")  # tar is 7th
    v = ec.evaluate("tar", res, k=5)
    assert v.status == "near_miss" and v.rank == 7
    v = ec.evaluate("tar", _results("a", "b", "c"), k=5)
    assert v.status == "fail" and v.rank is None


# --- T4: categorize ---
def test_categorize():
    assert ec.categorize("tar", []) == "not_found"
    res = [{"cmd_path": "ouch", "verified": False},
           {"cmd_path": "zipfoo", "verified": False}]
    assert ec.categorize("tar", res) == "canonical_burial"
    res = [{"cmd_path": "gh.search.prs", "verified": True}]
    assert ec.categorize("gh.pr.create", res) == "sibling_misorder"
    res = [{"cmd_path": "rsync", "verified": True}]
    assert ec.categorize("scp", res) == "genuine_ambiguity"


# --- T5: flag_attractors ---
def test_flag_attractors():
    fails = []
    for q in ("q1", "q2", "q3"):
        fails.append(ec.Verdict("tool" + q, q, "fail", None, "canonical_burial",
                                ["link-grammar", "x"]))
    fails.append(ec.Verdict("toolq4", "q4", "fail", None, "canonical_burial",
                            ["oneoff", "y"]))
    attractors = ec.flag_attractors(fails, min_hits=3)
    assert "link-grammar" in attractors
    assert "oneoff" not in attractors
    ec.apply_attractor_category(fails, attractors)
    assert fails[0].category == "inferred_attractor"
    assert fails[-1].category == "canonical_burial"


# --- T6: suggest_override ---
def test_suggest_override():
    v = ec.Verdict("tar", "compress a folder into an archive", "fail", None,
                   "canonical_burial", ["ouch"])
    s = ec.suggest_override(v)
    assert s is not None
    assert "compress" in s and "archive" in s and "folder" in s
    assert " a " not in f" {s} " and "into" not in s.split()
    assert ec.suggest_override(ec.Verdict("x", "q", "fail", None,
                               "genuine_ambiguity", [])) is None
    assert ec.suggest_override(ec.Verdict("x", "q", "fail", None,
                               "not_found", [])) is None


# --- T7: generate_queries cache + filter ---
def test_generate_queries_cache_and_filter(tmp_path, monkeypatch):
    cache = tmp_path / "q.json"
    cmds = [ec.Command("tar", "archive files"),
            ec.Command("grep", "search text in files")]
    calls = {"n": 0}

    def fake_llm(batch, session, model, key):
        calls["n"] += 1
        return {"tar": ["compress a folder", "tar things up"],
                "grep": ["search text in files", "find a pattern"]}
    monkeypatch.setattr(ec, "_llm_generate_batch", fake_llm)

    q1 = ec.generate_queries(cmds, str(cache), per_tool=3, batch_size=10,
                             session=None, model="m", key="k")
    assert "tar things up" not in q1["tar"]
    assert "compress a folder" in q1["tar"]
    assert calls["n"] == 1
    q2 = ec.generate_queries(cmds, str(cache), per_tool=3, batch_size=10,
                             session=None, model="m", key="k")
    assert calls["n"] == 1
    assert q2 == q1


# --- T8: render_report ---
def test_render_report():
    verdicts = [
        ec.Verdict("tar", "compress a folder", "fail", None, "canonical_burial",
                   ["ouch", "zipx"], "compress folder archive"),
        ec.Verdict("ls", "list files", "pass", 1, None, ["ls"]),
        ec.Verdict("scp", "copy over network", "near_miss", 8, "genuine_ambiguity",
                   ["rsync"]),
    ]
    md, data = ec.render_report(verdicts, total_queries=3)
    assert "1/3" in md or "33" in md
    assert "tar" in md and "canonical_burial" in md and "compress folder archive" in md
    assert {d["cmd_path"] for d in data} == {"tar", "scp"}


# --- T9: run_cmdh parsing ---
def test_run_cmdh_parses_json(monkeypatch):
    class P:
        stdout = '[{"cmd_path":"tar","verified":true,"description":"x"}]'
    monkeypatch.setattr(ec.subprocess, "run", lambda *a, **k: P())
    out = ec.run_cmdh("cmdh", "compress", 5)
    assert out and out[0]["cmd_path"] == "tar"

    class Bad:
        stdout = "not json"
    monkeypatch.setattr(ec.subprocess, "run", lambda *a, **k: Bad())
    assert ec.run_cmdh("cmdh", "x", 5) == []


# --- T11: parallel search preserves (cmd_path, query) pairing ---
def test_run_searches_parallel(monkeypatch):
    # fake run_cmdh returns the query echoed as the single result's cmd_path
    monkeypatch.setattr(ec, "run_cmdh",
                        lambda cmdh, q, limit: [{"cmd_path": q, "verified": True}])
    pairs = [("tar", "qA"), ("grep", "qB"), ("ls", "qC")]
    out = ec.run_searches_parallel("cmdh", pairs, limit=20, workers=4)
    # result dict keyed by (cmd_path, query) -> results, all present and correct
    assert out[("tar", "qA")][0]["cmd_path"] == "qA"
    assert out[("grep", "qB")][0]["cmd_path"] == "qB"
    assert out[("ls", "qC")][0]["cmd_path"] == "qC"
    assert len(out) == 3


# --- T12: robust LLM JSON parse (markdown fence / preamble) ---
def test_extract_json_robust():
    assert ec._extract_json('{"a":[1]}') == {"a": [1]}
    assert ec._extract_json('```json\n{"a":[1]}\n```') == {"a": [1]}
    assert ec._extract_json('Here you go:\n{"a":[1]}\nhope it helps') == {"a": [1]}
    assert ec._extract_json('garbage no json') == {}


# --- T13: judge corrects "equivalent tool returned" fails ---
def test_judge_results_parse(monkeypatch):
    # judge_results returns True when the LLM says a returned cmd satisfies the query
    class FakeResp:
        def raise_for_status(self): pass
        def json(self):
            return {"choices": [{"message": {"content": '{"satisfied": true, "note": "tracepath works"}'}}]}
    class FakeSession:
        def post(self, *a, **k): return FakeResp()
    ok = ec.judge_results(FakeSession(), "m", "k",
                          "trace network path to a host",
                          [{"cmd_path": "tracepath", "description": "trace path"}])
    assert ok is True

    class FakeRespNo:
        def raise_for_status(self): pass
        def json(self):
            return {"choices": [{"message": {"content": '{"satisfied": false, "note": "unrelated"}'}}]}
    class FakeSessionNo:
        def post(self, *a, **k): return FakeRespNo()
    bad = ec.judge_results(FakeSessionNo(), "m", "k", "q",
                           [{"cmd_path": "aws.iam.foo", "description": "x"}])
    assert bad is False

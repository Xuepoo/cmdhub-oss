import json
import importlib.util
import pathlib
import tempfile

spec = importlib.util.spec_from_file_location(
    "eval_intent", pathlib.Path(__file__).parent / "eval_intent.py")
ei = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ei)


def test_load_intents_parses_pairs():
    p = pathlib.Path(tempfile.mktemp(suffix=".json"))
    p.write_text(json.dumps([{"query": "install npm package", "accepts": ["npm.install", "npm"]}]))
    out = ei.load_intents(str(p))
    assert out == [("install npm package", ["npm.install", "npm"])]


def test_recall_at_k_logic():
    assert ei.recall_at_k(1, 1) is True
    assert ei.recall_at_k(1, 5) is True
    assert ei.recall_at_k(4, 1) is False
    assert ei.recall_at_k(4, 5) is True
    assert ei.recall_at_k(None, 5) is False

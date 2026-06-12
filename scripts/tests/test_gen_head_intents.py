import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import gen_head_intents as ghi  # noqa: E402


def test_select_head_picks_top_n_by_popularity():
    apps = [
        {"app_id": "a", "name": "a", "popularity": 0.1},
        {"app_id": "b", "name": "b", "popularity": 0.9},
        {"app_id": "c", "name": "c", "popularity": 0.5},
    ]
    head = ghi.select_head(apps, n=2)
    assert [h["app_id"] for h in head] == ["b", "c"]


def test_parse_intents_response_extracts_list():
    content = '```json\n["delete a folder", "remove recursively", "force delete"]\n```'
    assert ghi.parse_intents(content, limit=5) == ["delete a folder", "remove recursively", "force delete"]


def test_parse_intents_response_truncates_and_handles_garbage():
    assert ghi.parse_intents("not json", limit=5) == []
    big = json.dumps([f"intent {i}" for i in range(10)])
    assert len(ghi.parse_intents(big, limit=5)) == 5
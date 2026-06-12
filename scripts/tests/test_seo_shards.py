import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import seo_shards  # noqa: E402

FIXTURE = json.loads((Path(__file__).parent / "fixtures" / "sample_export.json").read_text())


def _group():
    apps = {a["app_id"]: a for a in FIXTURE["apps"]}
    args_by_app: dict[str, list] = {}
    for arg in FIXTURE["arguments"]:
        args_by_app.setdefault(arg["app_id"], []).append(arg)
    return apps, args_by_app


def test_build_shard_root_fields_and_tree():
    apps, args_by_app = _group()
    shard = seo_shards.build_shard(
        apps["org.gnu.coreutils.rm"],
        args_by_app["org.gnu.coreutils.rm"],
        intents=["how to delete a directory recursively"],
        generated_at="2026-06-12T00:00:00Z",
    )
    assert shard["app_id"] == "org.gnu.coreutils.rm"
    assert shard["name"] == "rm"
    # app-level description + topics come from the root node
    assert shard["description"] == "remove files or directories"
    assert shard["topics"] == ["coreutils", "delete", "filesystem", "remove"]
    assert shard["popularity"] == 0.9
    assert shard["install"] == {"os_aliases": "rm", "instructions": "apt install coreutils"}
    assert shard["source_url"] == "https://github.com/coreutils/coreutils"
    assert shard["intents"] == ["how to delete a directory recursively"]
    assert shard["generated_at"] == "2026-06-12T00:00:00Z"
    # tree sorted by cmd_path, each node carries its own fields
    assert [n["cmd_path"] for n in shard["tree"]] == ["rm", "rm.-r"]
    recursive = shard["tree"][1]
    assert recursive["risk_level"] == "dangerous"
    assert recursive["node_type"] == "sub"


def test_build_shard_handles_null_install_and_no_intents():
    apps, args_by_app = _group()
    shard = seo_shards.build_shard(
        apps["ai.openclaw.cli"], args_by_app["ai.openclaw.cli"], intents=[], generated_at="t"
    )
    assert shard["install"] == {"os_aliases": None, "instructions": None}
    assert shard["intents"] == []
    assert shard["topics"] == ["openclaw", "messaging", "cli"]

def test_build_index_sorted_by_popularity_desc():
    index = seo_shards.build_index(FIXTURE["apps"])
    assert [e["app_id"] for e in index] == [
        "org.gnu.coreutils.rm",  # 0.9
        "ai.openclaw.cli",       # 0.5
        "org.npmjs.rm",          # 0.3
    ]
    assert index[0] == {"app_id": "org.gnu.coreutils.rm", "name": "rm", "popularity": 0.9}


def test_build_manifest():
    m = seo_shards.build_manifest(build_id="20260612", count=3, generated_at="2026-06-12T00:00:00Z")
    assert m == {"build_id": "20260612", "count": 3, "generated_at": "2026-06-12T00:00:00Z"}

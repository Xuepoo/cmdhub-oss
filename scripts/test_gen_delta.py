import sqlite3, importlib.util, pathlib, sys
spec = importlib.util.spec_from_file_location(
    "bd", str(pathlib.Path(__file__).parent / "build_db.py"))
bd = importlib.util.module_from_spec(spec); sys.modules["bd"] = bd
spec.loader.exec_module(bd)


def test_model_id_helper():
    # _model_id returns a stable id (basename) for a model path
    assert bd._model_id("/opt/cmdhub/bge-small-en-v1.5.onnx") == "bge-small-en-v1.5"
    assert bd._model_id("/x/bge-micro-v2.onnx") == "bge-micro-v2"

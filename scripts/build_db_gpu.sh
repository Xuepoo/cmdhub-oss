#!/bin/bash
# GPU launcher for build_db.py. The system has CUDA 13 but onnxruntime-gpu needs
# CUDA 12 + cuDNN 9, so we install those as pip wheels into a dedicated venv and
# point the dynamic loader at them. Usage:
#   bash scripts/build_db_gpu.sh --input X.json --output Y.db --device cuda [--compress]
set -euo pipefail
VENV="${CMDHUB_GPU_VENV:-/tmp/cmdhub-gpu-venv}"
HERE="$(cd "$(dirname "$0")" && pwd)"

if [ ! -x "$VENV/bin/python" ]; then
  echo "[gpu] creating venv $VENV ..." >&2
  uv venv "$VENV" --python 3.12 >&2
  uv pip install --python "$VENV" --quiet \
    onnxruntime-gpu numpy sqlite-vec zstandard \
    nvidia-cudnn-cu12 nvidia-cublas-cu12 nvidia-cuda-runtime-cu12 \
    nvidia-cufft-cu12 nvidia-curand-cu12 nvidia-cuda-nvrtc-cu12 >&2
fi

# Prepend every nvidia cu12 lib dir to the loader path so ORT finds libcublasLt.so.12 etc.
LIBS="$("$VENV/bin/python" - <<'PY'
import nvidia, glob, os
print(":".join(glob.glob(os.path.join(os.path.dirname(nvidia.__file__), "*", "lib"))))
PY
)"
export LD_LIBRARY_PATH="${LIBS}:${LD_LIBRARY_PATH:-}"
exec "$VENV/bin/python" "$HERE/build_db.py" "$@"

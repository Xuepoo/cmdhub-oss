# Sandbox install-probe runbook

Probe `--help` for tools NOT installed locally by installing them in throwaway
containers on the VPS, then ship the results through the normal release pipeline.
Spec: `docs/superpowers/specs/2026-06-23-sandbox-install-probe-pipeline-design.md`.

## Hardware split
- **VPS (`snow`)** — install + probe (network-stable) + merge into master (CPU/sqlite).
- **Local (GPU)** — `build_db --reuse-vectors` + golden gate + sign + publish + reseed.

## One-time setup (VPS)
1. Build the base image:
   ```bash
   cd ~/cmdhub-oss/scripts/sandbox_probe
   podman build -t arch-probe-base:latest -f Containerfile.arch-probe-base .
   ```
2. Ship a **portable** `cmdh-extractor` from the local box. The global CachyOS cargo
   config bakes `target-cpu=native`, which **SIGILLs (exit 132) on the VPS CPU** — even
   inside a container. Rebuild with a generic target first:
   ```bash
   # local
   cd cmdhub-oss
   RUSTFLAGS="-C target-cpu=x86-64-v2" cargo build --release --bin cmdh-extractor
   scp target/release/cmdh-extractor snow:~/cmdh-extractor && ssh snow chmod +x ~/cmdh-extractor
   ```
3. Sync the package + merge script to the VPS:
   ```bash
   rsync -az scripts/sandbox_probe scripts/merge_probe_into_master.py snow:~/cmdhub-oss/scripts/
   ```
4. rsync the current published master DB to `snow:~/master.db`.

## Run a batch (VPS, background, resumable)
```bash
ssh snow
cd ~/cmdhub-oss
nohup uv run python3 -m scripts.sandbox_probe \
  --master ~/master.db --extractor ~/cmdh-extractor --repo-root "$PWD" \
  --batch-size 300 --concurrency 2 --min-pop 0.8 \
  > ~/sandbox-probe.log 2>&1 &
```
- Resumable: re-run the identical command after any interruption; the checkpoint
  (`/tmp/sandbox-probe/checkpoint.db`) skips `merged` / `perma-failed` / `no-subcommands`.
- Disk is bounded per batch (`pacman -Scc` + image prune between batches).
- Progress: `grep checkpoint ~/sandbox-probe.log` shows per-batch status counts.

## Ship (LOCAL, after rsync `~/master.db` back)
```bash
# local — build on a COPY for rollback safety
rsync -az snow:~/master.db ./probed-master.db
cp probed-master.db build-copy.db
uv run --with sqlite-vec python3 cmdhub-oss/scripts/export_sqlite.py --db build-copy.db --out export.json
bash cmdhub-oss/scripts/build_db_gpu.sh --input export.json --output new.db \
  --device cuda --compress --reuse-vectors <last-published.db>
# GATE — must be 26/26 / MRR >= 0.917
XDG_DATA_HOME=<g> uv run python3 cmdhub-oss/scripts/eval_golden.py --limit 5
```
- **Gate FAILS** → do NOT publish; the last-good master stays live. Diff `build-copy.db`
  vs last-good to find the regressing tool; mark it in the VPS checkpoint to re-probe or
  skip; rebuild without it.
- **Gate PASSES** → standard release: `sign_db.py` → `gen_delta.py` (verify == full) →
  `publish_r2.sh cdn-cmdhub <release-dir>` → cloud reseed (seed_explore + intents). See
  `docs/probe-batch-runbook.md`.

## Status / reason taxonomy (checkpoint.db)
| outcome | status | reason | merged? |
|---|---|---|---|
| probe yields subtree | `merged` | `probe-ok` | yes |
| pacman/yay can't install | `failed`→`perma-failed` | `install-fail` / `no-package` | no |
| installed, only a root (single-command tool) | `no-subcommands` | `no-subcommands` | no |
| install/probe timed out | `failed` | `timeout` | no |

`installed`/`probed` are non-terminal intermediates (a crash mid-tool); resume re-runs
them. If `install-fail` > 40% in a batch, stop and investigate the image/package mapping.

## Verified (2026-06-23, VPS smoke)
- base image build, `pacman -F` pkg resolution, install + isolated probe all work.
- `gh` (pkg resolved via `pacman -F` fallback) → 216 subcommands; `rclone` → 104.
- single-command tools (ncdu/iperf3/nnn) → `no-subcommands` (correct, not merged).
- `--network none` probe stage blocks egress (pacman -Sy fails).
- checkpoint resume: second run reports "0 tools to probe".

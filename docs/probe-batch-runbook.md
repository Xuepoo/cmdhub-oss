# Probe-Verification Batch Runbook (weekly cadence)

Replaces LLM-inferred CLI contracts with real `--help` ground truth, marked
`provenance='probe'`, gated by validate + golden, signed, published to R2, and
reflected in cloud Explore. Demand-driven: each batch drains new beta feedback
and advances the popularity tier.

**Runs on the workstation** (local 24-core / RTX 4060). Probe is offline-safe;
`build_db` needs the GPU; the VPS OOMs on build_db. No EC2 impact — this is a
data pipeline, not a prod service change, until the publish step.

**The single trust signal is `provenance='probe'`** end-to-end: importer →
`export_sqlite` → `build_db` dedup (probe wins over inferred) → CLI search
`verified` → cloud `commands.verified`. Anything else is `inferred`.

---

## Paths

| Role | Path |
|---|---|
| Master sqlite (dataset of record) | `cmdhub/tmp/rebuild-v4/cmdhub.db` |
| Pipeline scripts | `cmdhub/cmdhub-oss/scripts/` |
| Probe (docker-isolated) | `cmdhub-claw/docker_probe/` |
| Cloud re-seed | `cmdhub/cmdhub-cloud/cloud-deploy/scripts/seed_explore.py` |
| Prod node (SSM, no SSH) | `i-037abe4f7659090cd` (us-east-1) |

`MASTER=cmdhub/tmp/rebuild-v4/cmdhub.db` is the variable used throughout.

---

## Step 1 — Pull beta feedback (prod RDS is private → via SSM)

```bash
CMD=$(aws ssm send-command --instance-ids i-037abe4f7659090cd --document-name AWS-RunShellScript --region us-east-1 \
  --parameters 'commands=["export DBURL=$(aws ssm get-parameter --name /cmdhub/prod/DATABASE_URL --with-decryption --region us-east-1 --query Parameter.Value --output text)","psql \"$DBURL\" -t -A -F\"\\t\" -c \"SELECT DISTINCT app_id, cmd_path FROM feedback WHERE app_id IS NOT NULL;\""]' --query Command.CommandId --output text)
sleep 8
aws ssm get-command-invocation --command-id "$CMD" --instance-id i-037abe4f7659090cd --region us-east-1 --query StandardOutputContent --output text > /tmp/feedback.tsv
wc -l /tmp/feedback.tsv
```

## Step 2 — Select the batch (feedback ∪ top-N inferred)

```bash
cd cmdhub
python3 cmdhub-oss/scripts/select_probe_targets.py \
  --offline-db tmp/rebuild-v4/cmdhub.db --top 50 \
  --feedback-tsv /tmp/feedback.tsv --out /tmp/batch_packages.toml
grep -c '\[\[packages\]\]' /tmp/batch_packages.toml
```

Eyeball the list; drop any non-Linux-installable (device-OS) CLIs. The popularity
tier repeats binaries across app_ids (aws's sub-services, docker's plugins) —
de-dupe by `binary` when probing in Step 3.

## Step 3 — Probe the batch (docker-isolated, `--network none`)

```bash
mkdir -p /tmp/probe_out
cd cmdhub-claw/docker_probe
# Package-manager installable tools: batch_probe.py --source aur|pip|npm|cargo|go.
# System CLIs (git/docker/kubectl/aws...) live in the Arch image; probe per-binary:
python3 probe_cli.py --binary <name> --max-depth 3 --max-pages 400 > /tmp/probe_out/cli_<name>.json
```

Honor the depth/page caps. Spot-check 2-3 outputs have real subcommand pages
(not empty / not error pages) before importing.

## Step 4 — Import → master sqlite (now tagged probe)

```bash
cd cmdhub
MASTER=tmp/rebuild-v4/cmdhub.db
cp "$MASTER" "$MASTER.bak.$(date +%s)"                     # always back up first
python3 cmdhub-oss/scripts/import_deep_cli.py --probe-dir /tmp/probe_out --db "$MASTER" --provenance probe
sqlite3 "$MASTER" "SELECT provenance, count(*) FROM arguments GROUP BY provenance;"
```

Expect a non-zero `probe` count = the batch's rows. (Registry-package probes go
through `import_probe_results.py`, which also tags `probe`.)

## Step 5 — Build + GATE (validate + golden, no regression)

```bash
cd cmdhub/cmdhub-oss
python3 scripts/export_sqlite.py --db ../tmp/rebuild-v4/cmdhub.db --out /tmp/cmdhub_export.json
bash scripts/build_db_gpu.sh --input /tmp/cmdhub_export.json --output /tmp/cmdhub.db --compress
python3 scripts/validate_db.py --db /tmp/cmdhub.db 2>&1 | tail -20      # must be ALL PASSED
cp /tmp/cmdhub.db ~/.local/share/cmdhub/cmdhub.db
python3 scripts/eval_golden.py --limit 5 2>&1 | tail -3                 # must be >=96% / MRR >=0.761
```

`build_db` ranks `provenance='probe'` over `inferred` on dedup
(build_db.py:411) and carries provenance into the output.

> **HARD STOP:** if golden regresses below 96% / MRR 0.761, DO NOT publish.
> Restore `$MASTER` from the Step-4 backup and investigate the offending tool's
> probe/parse. This gate is non-negotiable.

## Step 6 — Sign + publish to R2

```bash
cd cmdhub/cmdhub-oss
python3 scripts/sign_db.py --db /tmp/cmdhub.db          # → cmdhub-<sha16>.db.zst + .sig (Ed25519)
bash scripts/publish_r2.sh                              # content-addressed upload + manifest pointer
# verify the live update chain:
curl -s --proxy http://127.0.0.1:1080 "https://cdn.cmdhub.org/db/update" \
  | python3 -c "import sys,json;d=json.load(sys.stdin);print('version',d.get('version'),'db_url',d.get('db_url'))"
# end-to-end CLI proof on a probed tool:
rm -rf /tmp/cmdhverify && XDG_DATA_HOME=/tmp/cmdhverify/data XDG_CACHE_HOME=/tmp/cmdhverify/cache \
  HTTPS_PROXY=http://127.0.0.1:1080 cmdh update
XDG_DATA_HOME=/tmp/cmdhverify/data cmdh search "<probed tool>" | jq '.[0] | {cmd_path, verified}'
```

Expect `verified: true` on the probed tool.

## Step 7 — Re-seed cloud Explore (union, verified)

```bash
cd cmdhub
python3 cmdhub-cloud/cloud-deploy/scripts/seed_explore.py tmp/rebuild-v4/cmdhub.db 2000 > /tmp/seed.sql
# gzip → presigned R2 → SSM psql apply (per project_prod_deployment mechanics), then:
curl -s --proxy http://127.0.0.1:1080 -X POST https://api.cmdhub.org/api/v1/search/public \
  -H 'Content-Type: application/json' -d '{"query":"<probed tool>","limit":1}' \
  | python3 -c "import sys,json;print(json.load(sys.stdin)[0])"
```

Expect the result carries `provenance: "probe"` and `verified: true`.

## Step 8 — Record the batch health line

Append one line to the batch log (date, probe-row count, golden %, MRR,
published db sha16, verified-coverage %):

```
2026-06-__ | batch N | +<rows> probe | golden <pct>% MRR <mrr> | sha <sha16> | verified <cov>%
```

---

## Cadence

Manual-trigger weekly to start. First batch = feedback ∪ top-50. Each subsequent
batch drains new feedback + advances the popularity tier (the selector already
skips apps that have probe rows, so the top-N window naturally moves forward).

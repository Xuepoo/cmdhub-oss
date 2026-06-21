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

## Incremental release (delta updates)

A small batch should ship a delta, not a fresh 247 MB full DB. The full DB is still
built + golden-gated + signed every release (the correctness floor); the delta is
diffed *from* the gated DB, so it inherits its quality. Old CLIs (no `delta=v2`
capability flag) always receive `mode:full` — never a delta — so their vec table is
never corrupted. Keep the **previous published** `cmdhub.db` as the baseline.

```bash
cd cmdhub/cmdhub-oss
PREV=tmp/published/cmdhub-prev.db      # the last release's DB (the delta base)

# 1. Incremental build: reuse unchanged vectors, only re-embed changed/new commands.
#    Refuses reuse + falls back to full embed if the embedding model changed.
bash scripts/build_db_gpu.sh --input /tmp/cmdhub_export.json --output /tmp/cmdhub.db \
  --compress --reuse-vectors "$PREV"

# 2. Same golden gate as a full release (HARD STOP < 96% / MRR 0.761).
python3 scripts/validate_db.py /tmp/cmdhub.db
python3 scripts/eval_golden.py --limit 5 2>&1 | tail -3

# 3. Sign the FULL DB (fallback + first-time/older clients).
python3 scripts/sign_db.py --zst /tmp/cmdhub.db.zst --version <YYYY.MM.DD>

# 4. Generate the signed delta between prev and new.
#    --prev-sync-time = the prev release's last_sync_time (from its manifest);
#    --new-sync-time  = the new full manifest's new_sync_time (must match).
uv run --with sqlite-vec --with zstandard --with cryptography python3 scripts/gen_delta.py \
  --prev "$PREV" --new /tmp/cmdhub.db --version <YYYY.MM.DD> \
  --prev-sync-time <PREV_SYNC> --new-sync-time <NEW_SYNC>

# 5. (optional, pre-publish) prove incremental == full at the data layer:
uv run --with sqlite-vec python3 scripts/verify_delta_equivalence.py --prev "$PREV" --new /tmp/cmdhub.db

# 6. Publish: uploads full + delta + refreshes db/releases.json (the Worker reads it).
bash scripts/publish_r2.sh
```

The Cloudflare Worker at `cdn.cmdhub.org/db/update` reads `db/releases.json` and
returns: `mode:full` (no `delta=v2`, or many versions behind, or index unreadable),
`mode:noop` (already latest), or `mode:incremental` (exactly one version behind +
`delta=v2`). First release / no prev DB: skip steps 1's `--reuse-vectors` and step 4
— publish full only.

> **First-release note:** `publish_r2.sh` only uploads a delta + chains
> `prev_sync_time` when a `delta-entry.json` exists in the release dir; otherwise it
> writes a `releases.json` with `delta: null` (full-only), which is valid.

---

## Cadence

Manual-trigger weekly to start. First batch = feedback ∪ top-50. Each subsequent
batch drains new feedback + advances the popularity tier (the selector already
skips apps that have probe rows, so the top-N window naturally moves forward).

**Release cadence (policy):** ship a release per meaningful batch (real verified-data
gains) plus a **weekly floor** to fold in accumulated small changes — NOT a fixed
daily clock (daily emits noisy near-empty deltas and multiplies the ops surface).
User-uploaded commands land in cloud PostgreSQL (Explore/API visible immediately)
and enter the offline DB on the next rebuild, independent of this cadence.

---

## Batch log

```
2026-06-16 | batch 1 | +29 probe (0→29)   | golden 96%  MRR 0.716 | sha c33fdd5216d33de2 | glyrc/rg/fd/jq/git
2026-06-16 | batch 2 | +483 probe (29→512) | golden 100% MRR 0.761 | sha b9d6980dae2ebcc4 | podman(256)/docker(219)+shellcheck/yt-dlp/micro/wl-copy/wl-paste/pdftotext/rtmpdump/du; consolidated podman 9→1 app + docker; deleted fabricated podman-images
2026-06-19 | batch 3 | +30 probe (512→542) | golden 100% MRR 0.761 | sha c14a4e108d8b239b | tar(19)/grep/curl/wget/gzip/xz/zip/unzip/sed/awk/find/ssh — broken mainstream CLIs; replaced inferred tar.k garbage w/ real --help; fixed wget WGETRC-error probe. Cloud reseed: prod verified 512→542, command_intents gap closed 45→0 (vectors extracted from freshly-built commands_vec). NOTE: tar/zip still buried under podman flood for generic "compress" queries — canonical-tool-burial / model ceiling, separate relevance task (search_overrides concept-aliases), NOT a probe regression.
2026-06-19 | batch 4 | +18 probe (542→560) | golden 100% MRR 0.767 | sha 32e4e702731b4b78 (v2026.06.19.2) | systemctl/journalctl/ip/make/rsync/scp/ssh-keygen/chmod/chown/kill/df/mount/dd/ln/mkdir/cp/mv/ps — system CLIs; deleted 192 fabricated subcommands (systemctl 80/ip 46/mount 15). **GOLDEN GATE CAUGHT A REGRESSION:** first build MRR 0.748<0.761 — chmod probe lost "change file permissions" to uu_chmod/gchmod because import truncated chmod's desc to the "or:" usage-noise line + chmod had no topics. Fixed via search_overrides (chmod/chown got BOTH a clean `description` AND `topics_append`); rebuild → 100%/0.767. Cloud reseed used the FIXED seed_explore.py (reaps stale commands). **PROBE GOTCHA: the prober mis-detects subcommands for flag-only tools** — dd→dd.ascii/dd.ebcdic (conv-option values), ln/rsync→`.or` (the "or:" in usage), systemctl→recursive .list-units.list-units. Mitigation: probe system CLIs at --max-depth 1 and strip any multi-page result to root-only (root --help text already lists subcommands for FTS/embedding). systemctl genuinely has clean subcommands at depth 1 but they share identical global-help text → also kept root-only.
2026-06-20 | batch 5 | +102 probe (560→662) | golden 100% MRR 0.767 | sha b0326bacca9b4576 (v2026.06.20) | kubectl(70)/cargo(17)/npm(11) WITH clean subcommand trees + go/openssl/nmcli/pacman root-only; deleted 380 fabricated rows (cargo 81/kubectl 69/openssl 67/npm 58/nmcli 40/pacman 34/go 31). **Subcommand-rich tools (kubectl/cargo/npm) probe CLEANLY at depth 2 — zero recursion noise, zero duplicate-text clusters** (unlike flag-only tools in batch 4). pacman recursed (pacman.pacman) → root-only; go/openssl/nmcli help format not parsed into subcommands → root-only (fine). Cloud reseed: prod verified=662, kubectl.scale/cargo/openssl now rank #1 for their NL queries. **TWO INFRA GOTCHAS this batch:** (1) background build_db monitor KILLED the build at 68% — the nohup'd process got reaped when the monitoring shell exited. Fix: launch with `setsid ... < /dev/null &` to fully detach the process group, then poll the log for "Compressed:" instead of wrapping the pid in a monitored wait. (2) **psycopg2 `cur.execute()` on a multi-statement file FAILED** ("unterminated quoted string") on the 4500-row intents upsert — its multi-statement parser choked. **Use a postgres:16 pod with `psql -v ON_ERROR_STOP=1 -f file.sql` for large multi-statement applies**, not psycopg2.
2026-06-20 | batch 6 | +161 probe (662→823) | golden 100% MRR 0.767 | sha b2ebe053d3901ee5 (v2026.06.20.2) | helm(58)/terraform(34)/docker-compose(60) clean subcommand trees + gpg/nft/sqlite3/node/psql/ffmpeg/fzf/btop/systemd-analyze root-only; deleted 154 fabricated rows. setsid-detached build + psql -f apply (both batch-5 lessons) worked first try. systemd-analyze recursed (.blame.blame) + ffmpeg foldered lib names (ffmpeg.libavutil) → root-only; docker-compose had 1 recursion page (.top.top) stripped, kept the other 59. Skipped aws(18555)/gcloud(6399) — too huge for now. Cloud reseed: prod verified=823, helm.install/terraform.apply/docker-compose.up all #1 for their NL queries.
2026-06-20 | batch 7 | +468 probe (823→1291) | golden 100% MRR 0.767 | sha 8471466b5630e699 (v2026.06.20.3) | gh(217)/ctr(100)/lvm(60)/flatpak(45)/runc(18)/podman-compose(15)/containerd(7) subcommand trees + btrfs/cryptsetup/dig/host/iptables/nslookup root-only; deleted 382 fabricated rows (gh 198/flatpak 64/btrfs 48/podman-compose 21/runc 11). Largest batch yet (+468). **NEW PROBE GOTCHA: flatpak's `--columns` values get mis-detected as subcommands** (flatpak.list.name, flatpak.history.time — they share the parent's help text → show as dup-text clusters). Fix: strip flatpak to depth<=1 (121→45 pages). gh hit the --max-pages 200 cap → re-probe at 400 (got 217). ctr's depth-2 (ctr.containers.create) ARE real containerd nesting — keep. Cloud reseed: prod verified=1291, gh/flatpak/ctr subcommands all surface verified for their NL queries (minor: gh.search.prs ranks above gh.pr.create — same-tool subcommand ordering, not a data issue). Still skipping aws/gcloud (huge inferred subtrees — need a top-level-only strategy).
2026-06-20 | batch 8 | +266 probe (1291→1557) | golden 100% MRR 0.767 | sha 2caea44a57709a4e (v2026.06.20.4) | aliyun(87)/argocd(111)/tofu(33)/vercel(33) subcommand trees + az/wrangler root-only; deleted 299 fabricated rows. **aws/gcloud ATTEMPTED depth-1 then REVERTED — golden regression.** First tried aws(431 services)/gcloud(133) at depth-1, deleting ~25000 fabricated operations. Build passed validate but **golden CRASHED to 88%/0.671**: depth-1 deletes the deep operations the golden set expects as answers (aws.ec2.create-vpc, aws.ec2.describe-instances, aws.s3.mb, aws.lambda.invoke), AND new service-level entries (aws.eks) polluted "deploy a service to kubernetes" ranking (kubectl.apply dropped out of top-5). LESSON: **huge cloud CLIs can't be naively depth-1'd — that strips golden anchor operations.** They need a HYBRID strategy: depth-1 for all services + depth-2 for the high-value/golden-tested services (ec2, s3, s3api, lambda, eks, gcloud compute). Restored master from the pre-batch backup, re-imported only the 6 clean non-aws/gcloud tools. **aws/gcloud remain DEFERRED to a dedicated hybrid-depth batch.**  Cloud reseed: prod verified=1557.
2026-06-20 | batch 9 | +636 probe (1557→2193) | golden 100% MRR 0.767 | sha 64d23ede5b2d48ae (v2026.06.20.5) | oci(93)/bun(107)/mc(107)/zig(148)/rustup(60)/uv(58)/glab(43)/pnpm(20) subcommand trees; oci depth-1 = 93 real service groups (Click CLI; depth-1 works cleanly UNLIKE aws because oci --help lists ~93 groups not thousands, AND oci isn't golden-tested so no anchor-deletion risk). bun/zig/rustup recursion noise stripped (bun.exec.exec, zig.build.build, rustup.completions.completions). NOTE: `mc` resolved to org.archlinux.mc (Arch's Midnight-Commander package id) but this machine's mc is the MinIO client — content is correct MinIO help, app_id namespace is a cosmetic mismatch only (search works: 'minio object storage' → mc/mc.admin verified). Cloud reseed: prod verified=2193.
2026-06-20 | batch 10 | +1709 probe (2193→3902) | golden 100% MRR 0.767 | sha e5d0527c8878e53f (v2026.06.20.6) | **aws/gcloud HYBRID-DEPTH — the deferred hard one, DONE.** Cleared the ~25000-row aws.*/gcloud.* fabrication (registry's biggest inferred block): aws depth-1 (431 services) + depth-2 on ec2(614)/iam(179)/s3api(109)/lambda(86)/eks(68)/s3(10); gcloud depth-1 (133 groups) + compute(86). aws 18555→1491, gcloud 6399→218. Golden anchors now REAL probe data (aws.ec2.create-vpc/describe-instances, s3.mb, s3api.create-bucket, lambda.invoke all #1-3 verified). **Method: wrote a /tmp/probe_service.py reusing probe_cli's get_help/_extract_subcommand_names to depth-2 a SINGLE service (probe_cli has no sub-path start flag), merged depth-1 ∪ depth-2 by cmd_path.** First build hit golden 96%/0.748 — same kubernetes-query regression as the failed depth-1 attempt (removing 25k rows shifts IDF; kubectl.apply buried under kn.service/kompose). Fixed via search_overrides enrichment of kubectl(root)+helm.install with deploy/kubernetes/service topics → 100%/0.767. Cloud reseed: prod verified=3902. DB shrank 358MB→291MB; registry ~117k args (was ~150k).
2026-06-20 | batch 11 | +2849 probe (3902→6751) | golden 100% MRR 0.787 (best yet) | sha 0a128eafe2f4b015 (v2026.06.20.7) | **MASS-PROBE: 1178 tools in one batch.** Probed every installed CLI that was inferred-in-registry + lang-ecosystem dev tools (black/cookiecutter/edge-tts/huggingface-cli/...). Method: /tmp/mass_probe.py reuses probe_cli.collect_help_pages over a target list, with generalized cleanup (recursion-strip + arg-placeholder-strip + junk-root filter). **CRITICAL GUI LESSON: probing GUI apps with --help/-h/help LAUNCHES THEIR WINDOWS** (Electron/some Qt ignore --help and open) — a probe run popped ~40 GUI windows. Prevention (do NOT skip): exclude any tool that (a) has a .desktop entry OR (b) `ldd`-links a GUI toolkit (libgtk/libQt/libwayland-client/libX11/libEGL). ldd is read-only (safe). After the incident, did ZERO re-probe — filtered the already-collected 1448 files by the GUI-exclusion set (read-only file ops). Dropped 88 GUI + ~180 junk (tracebacks, stdin-read errors, gtk-warnings, 'unrecognized argument --help'). **Golden regressed 92%/0.729 first build**: mass-probe OVERWROTE rm's search_override 'add' entry (probe row now exists → 'add' skipped) losing its delete-topics, AND atuin.search buried our own cmdh for 'offline search cli'. Fixed by converting rm to a 'patch' + adding cmdh patch → 100%/0.787. **LESSON: a probe batch can shadow any search_override 'add' whose tool gets probed — those must become 'patch'.** Cloud reseed: prod verified=6751.
```

Notes carried forward:
- **build_db bottleneck is the sqlite-vec write + zstd-19 compress (~28min on the
  150k corpus), NOT embedding** — do not wrap it in a short `timeout`; run it
  backgrounded and wait on the pid. Use `--device cuda` (single-process GPU path)
  to skip the multiprocess CPU pool.
- **Local `cmdh update` verification needs an isolated `XDG_CONFIG_HOME`** — the
  dev `~/.config/cmdhub/config.toml` pins a dev `public_key` (197f6b23…) that does
  NOT match the production signing key, so it fails signature verify. Real users
  have no such config and use the embedded `OFFICIAL_PUBLIC_KEY` (61e4a25c…), which
  the prod private key matches. Verify with
  `XDG_CONFIG_HOME=/tmp/x XDG_DATA_HOME=/tmp/y CMDH_API_URL=https://cdn.cmdhub.org cmdh update`.
- **Consolidation leaves empty fragment apps** (`import_deep_cli` deletes a tool's
  `arguments` and re-anchors under one app_id, but same-name fragment app rows with
  a *different* name survive empty). `seed_explore.py` now skips 0-command apps and
  reaps seed-owned orphans, so Explore stays clean. The offline DB still carries the
  empties (harmless — CLI search is on `arguments`).
- **Hyphenated fabrications** (`podman-images`, `podman-image`, …) are separate
  app_ids the dotted-path consolidation does NOT catch. They need a dedicated
  fabrication sweep (BACKLOG #13 follow-up b) — and care: `docker-compose` v1,
  `docker-machine`, `podman-compose`, `podman-tui` are REAL tools, not fakes.
- **Prod writes are permission-gated.** The Step-1 feedback read and the Step-7
  destructive re-seed (orphan-reaping DELETEs) can trip the auto-mode classifier;
  stage the artifact + presigned URL, then run the apply via an operator-invoked
  script (`! bash …`).

## Coverage tester (eval_coverage.py)

Discovery tool (NOT the release gate — golden stays the gate). For each probe
command, an LLM generates name-free task queries, runs `cmdh search`, and reports
unfindable tools categorized (not_found / canonical_burial / inferred_attractor /
sibling_misorder / genuine_ambiguity) with suggested search_overrides topics.

    OPENROUTER_API_KEY=… uv run --with requests python3 scripts/eval_coverage.py \
      --db ../tmp/rebuild-v4/cmdhub.db --report /tmp/coverage.md
    # quick iteration: add --sample 20

Query cache at /tmp/coverage_queries.json (re-runs reuse it; --regen to refresh).
Machine-readable fails at /tmp/coverage_fails.json. Triage the report: apply
reviewed topics to data/search_overrides.json, promote high-value fails into
eval_golden.py, then rebuild + golden-gate as usual.

**First sweep finding (2026-06-20):** mass-probe (batch 11) pulled in many niche
same-topic tools that now bury canonical subcommands — e.g. git.log/git.commit/
git.diff fail their own task queries (gview/tig/glv outrank git.log for "view
commit history"). Fix via search_overrides enrichment of the canonical git
subcommands; this is the tester's intended use.

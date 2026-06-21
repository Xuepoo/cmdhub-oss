#!/usr/bin/env bash
# Publish the signed offline database to Cloudflare R2 (served at cdn.cmdhub.org).
#
# Prereqs: release artifacts from sign_db.py — content-addressed cmdhub-<sha16>.db.zst/.sig
# plus manifest.json. The upload backend is auto-detected:
#   - aws cli with an [r2] profile (native S3 multipart, no post-upload hang) — preferred
#   - else wrangler (needs `wrangler login`; stalls after "Upload complete" behind a proxy,
#     so it is timeout-wrapped — the object still lands before the kill)
# Direct egress to Cloudflare is blocked in CN, so HTTPS_PROXY is set by default.
#
#   scripts/publish_r2.sh <bucket-name> [release-dir]
#
# Objects (keys match scripts/sign_db.py + the CLI's update endpoint):
#   db/cmdhub-<sha16>.db.zst  (zstd, immutable long cache)
#   db/cmdhub-<sha16>.db.sig  (octet-stream, immutable long cache)
#   db/update                 (json, SHORT cache — the version pointer)
set -euo pipefail

BUCKET="${1:?usage: publish_r2.sh <bucket-name> [release-dir]}"
DIR="${2:-/tmp/cmdhub_release}"
export HTTPS_PROXY="${HTTPS_PROXY:-http://127.0.0.1:1080}"
R2_PROFILE="${R2_PROFILE:-r2}"
# R2 only accepts auto/wnam/enam/... — override any AWS_REGION the shell exports for real AWS.
R2_REGION="${R2_REGION:-auto}"

# Content-addressed keys + local filenames come from the manifest the signer wrote.
DB_URL=$(python3 -c "import json;print(json.load(open('$DIR/manifest.json'))['db_url'])")
SIG_URL=$(python3 -c "import json;print(json.load(open('$DIR/manifest.json'))['sig_url'])")
DB_FILE="$(basename "$DB_URL")";   DB_KEY="db/$DB_FILE"
SIG_FILE="$(basename "$SIG_URL")"; SIG_KEY="db/$SIG_FILE"
for f in "$DB_FILE" "$SIG_FILE" manifest.json; do
  [ -s "$DIR/$f" ] || { echo "[error] missing $DIR/$f (run sign_db.py first)" >&2; exit 1; }
done

# Pick an upload backend. aws s3 (if the r2 profile has working creds) is preferred: native
# multipart, no post-upload hang. wrangler is the fallback.
BACKEND="wrangler"
if command -v aws >/dev/null 2>&1 && aws --region "$R2_REGION" --profile "$R2_PROFILE" s3 ls "s3://$BUCKET" >/dev/null 2>&1; then
  BACKEND="aws"
fi
echo "[r2] backend: $BACKEND  bucket: $BUCKET"

put() { # key file content-type cache-control
  echo "[r2] put $1"
  if [ "$BACKEND" = "aws" ]; then
    aws --region "$R2_REGION" --profile "$R2_PROFILE" s3 cp "$2" "s3://$BUCKET/$1" \
      --content-type "$3" --cache-control "$4" --only-show-errors
  else
    # wrangler may be killed by timeout after a successful upload; tolerate non-zero exit.
    # -y skips the data-catalog prompt that otherwise hangs the put indefinitely.
    timeout "${WRANGLER_TIMEOUT:-300}" wrangler r2 object put "$BUCKET/$1" \
      --file="$2" --content-type="$3" --cache-control="$4" --remote -y || true
  fi
}

# Immutable payloads first (so the manifest never points at a missing object), pointer last.
put "$DB_KEY"   "$DIR/$DB_FILE"      "application/zstd"          "public, max-age=31536000, immutable"
put "$SIG_KEY"  "$DIR/$SIG_FILE"     "application/octet-stream"  "public, max-age=31536000, immutable"
# Static db/update kept as a pre-Worker fallback: serves the full manifest to any
# direct GET. Once the cmdhub-cdn-update Worker is live, its route shadows this for
# /db/update, but the object staying valid means a safe rollout (no flag-day).
put "db/update" "$DIR/manifest.json" "application/json"          "public, max-age=60"

# --- Incremental delta + release index (Worker reads db/releases.json) ---
# If gen_delta.py produced a delta-entry.json in the release dir, upload the delta
# payload + signature. Then (always) refresh db/releases.json from the full manifest
# and the optional delta entry.
if [ -f "$DIR/delta-entry.json" ]; then
  DELTA_FILE=$(ls "$DIR"/delta-*.json.zst 2>/dev/null | head -1)
  DELTA_SIG=$(ls "$DIR"/delta-*.json.sig 2>/dev/null | head -1)
  if [ -n "$DELTA_FILE" ] && [ -n "$DELTA_SIG" ]; then
    put "db/$(basename "$DELTA_FILE")" "$DELTA_FILE" "application/zstd"         "public, max-age=31536000, immutable"
    put "db/$(basename "$DELTA_SIG")"  "$DELTA_SIG"  "application/octet-stream" "public, max-age=31536000, immutable"
  else
    echo "[error] delta-entry.json present but delta payload/sig missing" >&2; exit 1
  fi
fi

# Build db/releases.json: {latest:{version, sync_time, prev_sync_time, full, delta}}.
# sync_time + full come from the (authoritative) full manifest; prev_sync_time +
# delta come from delta-entry.json when present (else delta=null, full-only release).
python3 - "$DIR" <<'PY'
import json, os, sys
d = sys.argv[1]
man = json.load(open(os.path.join(d, "manifest.json")))
entry = {
    "version": man["version"],
    "sync_time": man["new_sync_time"],
    "prev_sync_time": None,
    "full": {"url": man["db_url"], "sig_url": man["sig_url"], "sha256": man["sha256"]},
    "delta": None,
}
dp = os.path.join(d, "delta-entry.json")
if os.path.exists(dp):
    de = json.load(open(dp))
    entry["prev_sync_time"] = de["prev_sync_time"]
    entry["delta"] = de["delta"]
    # The delta's base + target must line up with the full manifest, or a one-version
    # -behind client would get a delta that doesn't apply. Fail loudly on mismatch.
    if de["sync_time"] != man["new_sync_time"]:
        sys.exit(f"[error] delta sync_time {de['sync_time']} != manifest new_sync_time {man['new_sync_time']}")
json.dump({"latest": entry}, open(os.path.join(d, "releases.json"), "w"), indent=2)
print("[publish] releases.json written (delta=%s)" % ("yes" if entry["delta"] else "no"))
PY
put "db/releases.json" "$DIR/releases.json" "application/json" "public, max-age=60"

echo "[r2] done. db=$DB_KEY"
echo "[r2] verify (static):  curl -s https://cdn.cmdhub.org/db/update      | python3 -m json.tool"
echo "[r2] verify (worker):  curl -s 'https://cdn.cmdhub.org/db/update?last_sync_time=0&delta=v2' | python3 -m json.tool"
echo "[r2] verify (index):   curl -s https://cdn.cmdhub.org/db/releases.json | python3 -m json.tool"

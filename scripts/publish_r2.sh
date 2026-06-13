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
    timeout "${WRANGLER_TIMEOUT:-300}" wrangler r2 object put "$BUCKET/$1" \
      --file="$2" --content-type="$3" --cache-control="$4" --remote || true
  fi
}

# Immutable payloads first (so the manifest never points at a missing object), pointer last.
put "$DB_KEY"   "$DIR/$DB_FILE"      "application/zstd"          "public, max-age=31536000, immutable"
put "$SIG_KEY"  "$DIR/$SIG_FILE"     "application/octet-stream"  "public, max-age=31536000, immutable"
put "db/update" "$DIR/manifest.json" "application/json"          "public, max-age=60"

echo "[r2] done. db=$DB_KEY"
echo "[r2] verify: curl -s https://cdn.cmdhub.org/db/update | python3 -m json.tool"

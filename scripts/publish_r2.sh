#!/usr/bin/env bash
# Publish the signed offline database to Cloudflare R2 (served at cdn.cmdhub.org).
#
# Prereqs: `wrangler login` (or CLOUDFLARE_API_TOKEN with R2 write), and the release
# artifacts produced by scripts/sign_db.py (cmdhub.db.zst, cmdhub.db.sig, manifest.json).
# The CLI's `cmdh update` fetches {cdn}/db/update (manifest), then the .zst and .sig.
#
#   scripts/publish_r2.sh <bucket-name> [release-dir]
#
# Objects written (keys match scripts/sign_db.py and cmdhub-cli's update endpoint):
#   db/cmdhub.db.zst   (zstd)         — long cache, content addressed by manifest etag
#   db/cmdhub.db.sig   (octet-stream) — long cache
#   db/update          (json)         — SHORT cache; this is the version pointer
set -euo pipefail

BUCKET="${1:?usage: publish_r2.sh <bucket-name> [release-dir]}"
DIR="${2:-/tmp/cmdhub_release}"
export HTTPS_PROXY="${HTTPS_PROXY:-http://127.0.0.1:1080}"

for f in cmdhub.db.zst cmdhub.db.sig manifest.json; do
  [ -s "$DIR/$f" ] || { echo "[error] missing $DIR/$f (run sign_db.py first)" >&2; exit 1; }
done

put() { # key file content-type cache-control
  echo "[r2] put $1"
  wrangler r2 object put "$BUCKET/$1" --file="$2" \
    --content-type="$3" --cache-control="$4" --remote
}

# Immutable payloads first (so the manifest never points at a missing object), pointer last.
put "db/cmdhub.db.zst" "$DIR/cmdhub.db.zst" "application/zstd"        "public, max-age=31536000, immutable"
put "db/cmdhub.db.sig" "$DIR/cmdhub.db.sig" "application/octet-stream" "public, max-age=31536000, immutable"
put "db/update"        "$DIR/manifest.json" "application/json"         "public, max-age=300"

echo "[r2] done. verify: curl -s https://cdn.cmdhub.org/db/update"

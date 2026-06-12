#!/usr/bin/env bash
# Sync the generated SEO artifacts to Cloudflare R2 (read by the edge renderer).
# Stable keys + `aws s3 sync` => only changed shards are re-uploaded. The manifest's
# build_id is what busts the edge cache (handled by the renderer), so shards themselves
# are long-cached/immutable.
#
#   scripts/upload_seo_shards.sh <bucket-name> <out-dir>
set -euo pipefail

BUCKET="${1:?usage: upload_seo_shards.sh <bucket-name> <out-dir>}"
DIR="${2:?usage: upload_seo_shards.sh <bucket-name> <out-dir>}"
export HTTPS_PROXY="${HTTPS_PROXY:-http://127.0.0.1:1080}"
R2_PROFILE="${R2_PROFILE:-r2}"
R2_REGION="${R2_REGION:-auto}"
aws() { command aws --region "$R2_REGION" --profile "$R2_PROFILE" "$@"; }

[ -d "$DIR/registry" ] || {
  echo "[error] $DIR/registry missing (run gen_seo_shards.py first)" >&2
  exit 1
}

# Shards: immutable long cache (build_id in manifest drives edge invalidation).
aws s3 sync "$DIR/registry/" "s3://$BUCKET/registry/" \
  --exclude "manifest.json" --exclude "index.json" \
  --content-type "application/json" \
  --cache-control "public, max-age=31536000, immutable" \
  --only-show-errors

# Gzipped sitemaps: immutable; set the gzip content-encoding so browsers/crawlers decode.
aws s3 sync "$DIR/sitemaps/" "s3://$BUCKET/sitemaps/" --exclude "*.xml.gz" \
  --content-type "application/xml" --cache-control "public, max-age=300" --only-show-errors
aws s3 sync "$DIR/sitemaps/" "s3://$BUCKET/sitemaps/" --exclude "*" --include "*.xml.gz" \
  --content-type "application/xml" --content-encoding "gzip" \
  --cache-control "public, max-age=86400" --only-show-errors

# Pointers last: short cache so a new build is picked up within ~60s.
aws s3 cp "$DIR/registry/index.json" "s3://$BUCKET/registry/index.json" \
  --content-type "application/json" --cache-control "public, max-age=60" --only-show-errors
aws s3 cp "$DIR/registry/manifest.json" "s3://$BUCKET/registry/manifest.json" \
  --content-type "application/json" --cache-control "public, max-age=60" --only-show-errors
aws s3 cp "$DIR/robots.txt" "s3://$BUCKET/robots.txt" \
  --content-type "text/plain" --cache-control "public, max-age=300" --only-show-errors

echo "[seo] synced to s3://$BUCKET (registry/ + sitemaps/ + manifest)"

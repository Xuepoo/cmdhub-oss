#!/usr/bin/env python3
"""Sign a compressed database and emit the Ed25519 signature + update manifest.

Trust chain (see docs/adr/003-ed25519-trust-chain.md): the cloud signs SHA-256(db.zst)
with the offline Ed25519 private key; the CLI verifies it against the hardcoded public key
(cmdhub-cli/src/config.rs::OFFICIAL_PUBLIC_KEY). This script produces the two artifacts the
CLI's `cmdh update` downloads from the CDN: `<name>.sig` (64-byte signature) and a
`manifest.json` matching cmdhub_shared::UpdateManifest, with URLs pointing at cdn.cmdhub.org.

    uv run --with cryptography python3 sign_db.py --zst /tmp/cmdhub_final.db.zst \
        --version 2026.06.11 [--priv ~/.config/cmdhub/keys/ed25519_private.bin] \
        [--cdn https://cdn.cmdhub.org] [--out-dir /tmp/cmdhub_release]
"""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import shutil
import sys

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey, Ed25519PublicKey

# Content-addressed object keys. The payload (zst/sig) is served with a long immutable
# cache, so reusing a fixed key across releases lets a stale Cloudflare edge cache shadow
# the new upload forever (immutable means the edge never revalidates — a real outage we hit
# 2026-06-11). Embedding the sha256 in the key means every release writes a NEW path that no
# edge has cached, while old paths stay valid (rollback-friendly). Only the manifest keeps a
# FIXED key + SHORT cache — it is the version pointer the CLI polls.
MANIFEST_KEY = "db/update"  # served at {cdn}/db/update (the CLI's update endpoint)


def _content_keys(sha_hex: str) -> tuple[str, str]:
    """Return (db_key, sig_key) addressed by the payload's sha256."""
    short = sha_hex[:16]
    return f"db/cmdhub-{short}.db.zst", f"db/cmdhub-{short}.db.sig"


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--zst", required=True, help="compressed database (.zst)")
    ap.add_argument("--version", required=True, help="release version, e.g. 2026.06.11")
    ap.add_argument("--priv", default=os.path.expanduser("~/.config/cmdhub/keys/ed25519_private.bin"))
    ap.add_argument("--cdn", default="https://cdn.cmdhub.org")
    ap.add_argument("--out-dir", default="/tmp/cmdhub_release")
    a = ap.parse_args()

    if not os.path.exists(a.zst):
        print(f"[error] no .zst at {a.zst}", file=sys.stderr); sys.exit(1)
    if not os.path.exists(a.priv):
        print(f"[error] no private key at {a.priv} (run keygen first)", file=sys.stderr); sys.exit(1)

    blob = open(a.zst, "rb").read()
    h = hashlib.sha256(blob)
    digest = h.digest()       # 32 raw bytes — the message that gets signed/verified
    sha_hex = h.hexdigest()   # hex string for the manifest

    sk = Ed25519PrivateKey.from_private_bytes(open(a.priv, "rb").read())
    signature = sk.sign(digest)  # sign the 32-byte SHA-256, exactly what the CLI verifies
    # Fail loudly if the local key does not match the public key the CLI ships.
    sk.public_key().verify(signature, digest)

    db_key, sig_key = _content_keys(sha_hex)
    os.makedirs(a.out_dir, exist_ok=True)
    db_out = os.path.join(a.out_dir, os.path.basename(db_key))
    sig_out = os.path.join(a.out_dir, os.path.basename(sig_key))
    man_out = os.path.join(a.out_dir, "manifest.json")
    shutil.copyfile(a.zst, db_out)
    open(sig_out, "wb").write(signature)

    manifest = {
        "version": a.version,
        "etag": sha_hex,
        "db_url": f"{a.cdn}/{db_key}",
        "sig_url": f"{a.cdn}/{sig_key}",
        "sha256": sha_hex,
        "mode": "full",
        "new_sync_time": int(dt.datetime.now(dt.timezone.utc).timestamp()),
    }
    json.dump(manifest, open(man_out, "w"), indent=2)

    print(f"[sign] sha256={sha_hex}")
    print(f"[sign] signature ({len(signature)}B) -> {sig_out}")
    print(f"[sign] db   -> {db_out}  ({len(blob)/1e6:.1f} MB)")
    print(f"[sign] manifest -> {man_out}")
    print(f"[sign] R2 upload keys: {db_key}, {sig_key}, {MANIFEST_KEY}")


if __name__ == "__main__":
    main()

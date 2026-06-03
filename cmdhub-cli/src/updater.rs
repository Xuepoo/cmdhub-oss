use crate::config::{get_cache_dir, Config, OFFICIAL_PUBLIC_KEY};
use crate::db::resolve_db_path;
use anyhow::{Context, Result};
use cmdhub_shared::{CmdHubError, UpdateManifest};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use fs2::FileExt;
use reqwest::Client;
use sha2::{Digest, Sha256};
use std::fs;

pub async fn update_database(config: &Config, force: bool) -> Result<()> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(config.timeout_seconds))
        .build()?;

    let update_url = format!("{}/db/update", config.api_url);

    eprintln!("Checking for updates at {}...", update_url);

    // Fetch manifest
    let manifest_resp = client.get(&update_url).send().await;
    let manifest: UpdateManifest = match manifest_resp {
        Ok(resp) => {
            if resp.status().is_success() {
                resp.json()
                    .await
                    .context("Failed to parse UpdateManifest JSON")?
            } else {
                return Err(anyhow::anyhow!(CmdHubError::UpdateFailed(format!(
                    "Cloud returned status code: {}",
                    resp.status()
                ))));
            }
        }
        Err(e) => {
            return Err(anyhow::anyhow!(CmdHubError::UpdateFailed(format!(
                "Failed to fetch database update manifest: {}",
                e
            ))));
        }
    };

    let cache_dir = get_cache_dir();
    let downloads_dir = cache_dir.join("downloads");
    fs::create_dir_all(&downloads_dir).context("Failed to create downloads cache directory")?;

    let db_zst_path = downloads_dir.join("latest.db.zst");
    let sig_path = downloads_dir.join("latest.db.sig");

    eprintln!(
        "Downloading database update (version: {})...",
        manifest.version
    );

    // Download payload .zst
    let db_resp = client
        .get(&manifest.db_url)
        .send()
        .await
        .context("Failed to download database file")?;
    let db_bytes = db_resp
        .bytes()
        .await
        .context("Failed to read database bytes")?;
    fs::write(&db_zst_path, &db_bytes).context("Failed to write downloaded database payload")?;

    // Download signature
    let sig_resp = client
        .get(&manifest.sig_url)
        .send()
        .await
        .context("Failed to download database signature file")?;
    let sig_bytes = sig_resp
        .bytes()
        .await
        .context("Failed to read database signature bytes")?;
    fs::write(&sig_path, &sig_bytes).context("Failed to write downloaded signature payload")?;

    // 1. Calculate SHA-256 Hash of downloaded .zst
    eprintln!("Verifying database integrity and signature...");
    let mut hasher = Sha256::new();
    hasher.update(&db_bytes);
    let hash_result: [u8; 32] = hasher.finalize().into();

    // Verify SHA-256 match with manifest
    let computed_hex = hash_result
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>();
    if !force && computed_hex != manifest.sha256 {
        return Err(anyhow::anyhow!(CmdHubError::Validation(format!(
            "SHA-256 mismatch: computed {}, manifest {}",
            computed_hex, manifest.sha256
        ))));
    }

    // 2. Decode official public key
    let pub_key_bytes = match hex_decode(&config.public_key) {
        Ok(bytes) => {
            let mut arr = [0u8; 32];
            if bytes.len() == 32 {
                arr.copy_from_slice(&bytes);
                arr
            } else {
                OFFICIAL_PUBLIC_KEY
            }
        }
        Err(_) => OFFICIAL_PUBLIC_KEY,
    };

    let verifying_key = VerifyingKey::from_bytes(&pub_key_bytes).map_err(|e| {
        anyhow::anyhow!(CmdHubError::SignatureVerification(format!(
            "Invalid public key: {}",
            e
        )))
    })?;

    let signature = Signature::from_slice(&sig_bytes).map_err(|e| {
        anyhow::anyhow!(CmdHubError::SignatureVerification(format!(
            "Invalid signature format: {}",
            e
        )))
    })?;

    verifying_key
        .verify(&hash_result, &signature)
        .map_err(|e| {
            anyhow::anyhow!(CmdHubError::SignatureVerification(format!(
                "Ed25519 signature verification failed: {}",
                e
            )))
        })?;

    // 3. Decompress .zst payload
    eprintln!("Decompressing database...");
    let decompressed =
        zstd::decode_all(&db_bytes[..]).context("Failed to decompress zstd payload")?;

    let tmp_dir = cache_dir.join("tmp");
    fs::create_dir_all(&tmp_dir).context("Failed to create temporary staging directory")?;
    let staging_path = tmp_dir.join("latest.db");
    fs::write(&staging_path, &decompressed)
        .context("Failed to write decompressed staging database")?;

    // 4. Lock update.lock and atomically swap
    eprintln!("Applying atomic database replacement...");
    let lock_path = cache_dir.join("update.lock");
    let lock_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&lock_path)
        .context("Failed to open update.lock file")?;

    lock_file
        .lock_exclusive()
        .context("Failed to acquire exclusive lock on update.lock")?;

    let live_db_path = resolve_db_path();
    if let Some(parent) = live_db_path.parent() {
        fs::create_dir_all(parent).context("Failed to create live database directory")?;
    }

    fs::rename(&staging_path, &live_db_path)
        .context("Failed to atomically replace database file via rename")?;

    eprintln!(
        "Database successfully updated to version {}!",
        manifest.version
    );
    Ok(())
}

fn hex_decode(s: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c1) = chars.next() {
        if let Some(c2) = chars.next() {
            let hex = format!("{}{}", c1, c2);
            let b = u8::from_str_radix(&hex, 16)?;
            bytes.push(b);
        }
    }
    Ok(bytes)
}

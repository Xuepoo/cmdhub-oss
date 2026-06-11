use crate::config::{get_cache_dir, Config, OFFICIAL_PUBLIC_KEY};
use crate::db::resolve_db_path;
use anyhow::{Context, Result};
use cmdhub_shared::{CmdHubError, IncrementalSyncPayload, UpdateManifest};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use fs2::FileExt;
use reqwest::Client;
use sha2::{Digest, Sha256};
use std::fs;

pub async fn update_database(config: &Config, force: bool) -> Result<()> {
    // Use connect + read (stall) timeouts rather than one total timeout: the database
    // payload is hundreds of MB, so a total timeout aborts a healthy but slow download.
    // A read timeout still aborts a genuinely stalled connection.
    let to = std::time::Duration::from_secs(config.timeout_seconds);
    let client = Client::builder()
        .connect_timeout(to)
        .read_timeout(to)
        .build()?;

    let mut last_sync_time = 0i64;
    let live_db_path = resolve_db_path();
    if !force && live_db_path.exists() {
        if let Ok(conn) = rusqlite::Connection::open(&live_db_path) {
            if let Ok(val) = conn.query_row::<String, _, _>(
                "SELECT value FROM sync_meta WHERE key = 'last_sync_time' LIMIT 1",
                [],
                |row| row.get(0),
            ) {
                if let Ok(t) = val.parse::<i64>() {
                    last_sync_time = t;
                }
            }
        }
    }

    let update_url = format!(
        "{}/db/update?last_sync_time={}",
        config.api_url, last_sync_time
    );

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

    let mode = manifest.mode.clone().unwrap_or_else(|| "full".to_string());
    if mode == "noop" {
        eprintln!("Database is already up-to-date.");
        return Ok(());
    }

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

    if mode == "incremental" {
        eprintln!("Applying incremental database changes...");
        let payload: IncrementalSyncPayload = serde_json::from_slice(&decompressed)
            .context("Failed to parse IncrementalSyncPayload JSON")?;

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

        let mut conn = rusqlite::Connection::open(&live_db_path)
            .context("Failed to open live database for incremental update")?;
        let _ = conn.execute("PRAGMA foreign_keys = ON;", []);

        unsafe {
            type SqliteVecInitFn = unsafe extern "C" fn();
            let init_fn: SqliteVecInitFn = sqlite_vec::sqlite3_vec_init;
            #[allow(clippy::missing_transmute_annotations)]
            let _ = rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(init_fn)));
        }

        let tx = conn
            .transaction()
            .context("Failed to start SQLite transaction")?;

        crate::db::init_db(&tx)?;

        // Helper helper to delete commands and associated index entries for a given app_id
        let delete_app_commands = |tx_ref: &rusqlite::Transaction,
                                   target_app_id: &str|
         -> Result<()> {
            let mut stmt = tx_ref.prepare("SELECT cmd_path FROM arguments WHERE app_id = ?1")?;
            let mut rows = stmt.query(rusqlite::params![target_app_id])?;
            while let Some(row) = rows.next()? {
                let cmd_path: String = row.get(0)?;
                let _ = tx_ref.execute(
                    "DELETE FROM apps_fts WHERE cmd_path = ?1",
                    rusqlite::params![cmd_path],
                );
                let _ = tx_ref.execute(
                    "DELETE FROM commands_vec WHERE cmd_path = ?1",
                    rusqlite::params![cmd_path],
                );
            }
            tx_ref.execute(
                "DELETE FROM arguments WHERE app_id = ?1",
                rusqlite::params![target_app_id],
            )?;
            Ok(())
        };

        // 1. Process deleted/archived apps
        for app_id in payload.deleted_apps {
            delete_app_commands(&tx, &app_id)?;
            tx.execute(
                "DELETE FROM apps WHERE app_id = ?1",
                rusqlite::params![app_id],
            )?;
        }

        // 2. Process updated/inserted apps
        for app in payload.apps {
            delete_app_commands(&tx, &app.app_id)?;
            tx.execute(
                "INSERT OR REPLACE INTO apps (app_id, name, install_instructions) VALUES (?1, ?2, ?3)",
                rusqlite::params![app.app_id, app.name, app.install_instructions],
            )?;
        }

        for arg in payload.arguments {
            tx.execute(
                "INSERT OR REPLACE INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level, example_template, docker_image, script_url, source_url) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    arg.cmd_path,
                    arg.app_id,
                    arg.node_name,
                    arg.node_type,
                    arg.description,
                    arg.risk_level,
                    arg.example_template,
                    arg.docker_image,
                    arg.script_url,
                    arg.source_url
                ],
            )?;

            let _ = tx.execute(
                "DELETE FROM apps_fts WHERE cmd_path = ?1",
                rusqlite::params![arg.cmd_path],
            );

            let app_name: String = tx
                .query_row(
                    "SELECT name FROM apps WHERE app_id = ?1",
                    rusqlite::params![arg.app_id],
                    |row| row.get(0),
                )
                .unwrap_or_else(|_| "unknown".to_string());

            tx.execute(
                "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES (?1, ?2, ?3)",
                rusqlite::params![arg.cmd_path, app_name, arg.description],
            )?;
        }

        for vec in payload.command_vecs {
            if vec.embedding.len() == 512 {
                let mut vec_bytes = Vec::with_capacity(512 * 4);
                for &val in &vec.embedding {
                    vec_bytes.extend_from_slice(&val.to_ne_bytes());
                }
                let _ = tx.execute(
                    "DELETE FROM commands_vec WHERE cmd_path = ?1",
                    rusqlite::params![vec.cmd_path],
                );
                let _ = tx.execute(
                    "INSERT INTO commands_vec (cmd_path, embedding) VALUES (?1, ?2)",
                    rusqlite::params![vec.cmd_path, vec_bytes],
                );
            }
        }

        let new_time = manifest
            .new_sync_time
            .unwrap_or_else(|| chrono::Utc::now().timestamp());
        tx.execute(
            "INSERT OR REPLACE INTO sync_meta (key, value) VALUES ('last_sync_time', ?1)",
            rusqlite::params![new_time.to_string()],
        )?;

        tx.commit()
            .context("Failed to commit incremental SQLite transaction")?;
        eprintln!(
            "Database successfully incrementally updated (new sync time: {})!",
            new_time
        );
    } else {
        let tmp_dir = cache_dir.join("tmp");
        fs::create_dir_all(&tmp_dir).context("Failed to create temporary staging directory")?;
        let staging_path = tmp_dir.join("latest.db");
        fs::write(&staging_path, &decompressed)
            .context("Failed to write decompressed staging database")?;

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

        eprintln!("Safely applying database changes...");
        let src_conn =
            rusqlite::Connection::open(&staging_path).context("Failed to open staging database")?;
        let mut dst_conn =
            rusqlite::Connection::open(&live_db_path).context("Failed to open live database")?;

        let _ = dst_conn.execute("PRAGMA journal_mode = WAL;", []);
        let _ = dst_conn.execute("PRAGMA synchronous = NORMAL;", []);
        let _ = dst_conn.execute("PRAGMA foreign_keys = ON;", []);

        let backup = rusqlite::backup::Backup::new(&src_conn, &mut dst_conn)
            .context("Failed to initialize SQLite backup")?;

        backup
            .run_to_completion(100, std::time::Duration::from_millis(10), None)
            .context("SQLite backup to live database failed")?;

        drop(backup);

        let _ = fs::remove_file(&staging_path);

        let new_time = manifest
            .new_sync_time
            .unwrap_or_else(|| chrono::Utc::now().timestamp());
        let _ = dst_conn.execute(
            "CREATE TABLE IF NOT EXISTS sync_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
            [],
        );
        let _ = dst_conn.execute(
            "INSERT OR REPLACE INTO sync_meta (key, value) VALUES ('last_sync_time', ?1)",
            rusqlite::params![new_time.to_string()],
        );

        eprintln!(
            "Database successfully updated to version {} (sync time: {})!",
            manifest.version, new_time
        );
    }
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

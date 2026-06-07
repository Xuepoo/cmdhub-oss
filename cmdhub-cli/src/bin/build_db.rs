//! cmdh-build-db — Build a cmdhub.db SQLite database from a JSON export.
//!
//! Reads the JSON produced by `scripts/export_pg.py`, generates real BGE-micro-v2
//! embeddings for every argument's description, writes a fully-populated SQLite
//! database (apps / arguments / apps_fts / commands_vec / sync_meta), and
//! optionally compresses the result to a `.zst` file.
//!
//! Usage:
//!   cmdh-build-db --input cmdhub_export.json --output cmdhub.db [--compress]
//!
//! Environment:
//!   CMDH_MODEL_PATH — override ONNX model path (default: ~/.local/share/cmdhub/models/bge-micro-v2.onnx)

use anyhow::{Context, Result};
use cmdhub_cli::{
    db::init_db,
    inference::EmbeddingModel,
    tokenizer::Tokenizer,
};
use rusqlite::Connection;
use serde::Deserialize;
use std::{
    env,
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Debug, Deserialize)]
struct ExportApp {
    app_id: String,
    name: String,
    os_aliases: Option<String>,
    install_instructions: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExportArgument {
    cmd_path: String,
    app_id: String,
    node_name: String,
    node_type: String,
    description: String,
    risk_level: String,
    example_template: Option<String>,
    docker_image: Option<String>,
    script_url: Option<String>,
    source_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Export {
    apps: Vec<ExportApp>,
    arguments: Vec<ExportArgument>,
}

fn model_path() -> PathBuf {
    if let Ok(p) = env::var("CMDH_MODEL_PATH") {
        return PathBuf::from(p);
    }
    // Use the same XDG logic as cmdhub_cli::config::get_cache_dir()
    cmdhub_cli::config::get_cache_dir().join("models/bge-micro-v2.onnx")
}

fn embed(model: &EmbeddingModel, tokenizer: &Tokenizer, text: &str) -> Result<Vec<u8>> {
    let (ids, mask) = tokenizer.tokenize_passage(text);
    let vec = model.generate_embedding(&ids, &mask)?;
    let mut bytes = Vec::with_capacity(vec.len() * 4);
    for &v in &vec {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    Ok(bytes)
}

fn insert_app(conn: &Connection, app: &ExportApp) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO apps (app_id, name, os_aliases, install_instructions) \
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            &app.app_id,
            &app.name,
            &app.os_aliases,
            &app.install_instructions,
        ],
    )?;
    Ok(())
}

fn insert_argument(
    conn: &Connection,
    arg: &ExportArgument,
    app_name: &str,
    emb_bytes: &[u8],
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO arguments \
         (cmd_path, app_id, node_name, node_type, description, risk_level, \
          example_template, docker_image, script_url, source_url) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        rusqlite::params![
            &arg.cmd_path,
            &arg.app_id,
            &arg.node_name,
            &arg.node_type,
            &arg.description,
            &arg.risk_level,
            &arg.example_template,
            &arg.docker_image,
            &arg.script_url,
            &arg.source_url,
        ],
    )?;

    // FTS5: delete-then-insert to avoid OR REPLACE issues with virtual tables
    let _ = conn.execute(
        "DELETE FROM apps_fts WHERE cmd_path = ?1",
        rusqlite::params![&arg.cmd_path],
    );
    conn.execute(
        "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES (?1, ?2, ?3)",
        rusqlite::params![&arg.cmd_path, app_name, &arg.description],
    )?;

    // commands_vec: delete-then-insert
    let _ = conn.execute(
        "DELETE FROM commands_vec WHERE cmd_path = ?1",
        rusqlite::params![&arg.cmd_path],
    );
    conn.execute(
        "INSERT INTO commands_vec (cmd_path, embedding) VALUES (?1, ?2)",
        rusqlite::params![&arg.cmd_path, emb_bytes],
    )?;

    Ok(())
}

fn main() -> Result<()> {
    // Parse args
    let args: Vec<String> = env::args().collect();
    let mut input_path: Option<String> = None;
    let mut output_path: Option<String> = None;
    let mut compress = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--input" | "-i" => {
                i += 1;
                input_path = args.get(i).cloned();
            }
            "--output" | "-o" => {
                i += 1;
                output_path = args.get(i).cloned();
            }
            "--compress" => compress = true,
            _ => {}
        }
        i += 1;
    }

    let input_path = input_path.context("--input <json_file> is required")?;
    let output_path = output_path.unwrap_or_else(|| "cmdhub.db".to_string());

    eprintln!("[build-db] Loading export from {input_path}...");
    let raw = std::fs::read_to_string(&input_path)
        .with_context(|| format!("Failed to read {input_path}"))?;
    let export: Export =
        serde_json::from_str(&raw).context("Failed to parse export JSON")?;

    eprintln!(
        "[build-db] Loaded {} apps, {} arguments",
        export.apps.len(),
        export.arguments.len()
    );

    // Load ONNX model
    let mp = model_path();
    eprintln!("[build-db] Loading ONNX model from {:?}...", mp);
    if !mp.exists() {
        anyhow::bail!(
            "ONNX model not found at {:?}. Run `cmdh install vector` first.",
            mp
        );
    }
    let model = EmbeddingModel::load(&mp).context("Failed to load ONNX model")?;
    let tokenizer = Tokenizer::new();
    eprintln!("[build-db] Model loaded.");

    // Initialize sqlite-vec extension
    unsafe {
        type SqliteVecInitFn = unsafe extern "C" fn();
        let init_fn: SqliteVecInitFn = sqlite_vec::sqlite3_vec_init;
        #[allow(clippy::missing_transmute_annotations)]
        let _ = rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(init_fn)));
    }

    // Open SQLite at output path
    let output_db = PathBuf::from(&output_path);
    if let Some(parent) = output_db.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(&output_db)
        .with_context(|| format!("Failed to open SQLite at {output_path}"))?;
    let _ = conn.execute("PRAGMA journal_mode = WAL;", []);
    let _ = conn.execute("PRAGMA synchronous = NORMAL;", []);
    let _ = conn.execute("PRAGMA foreign_keys = ON;", []);
    init_db(&conn)?;
    eprintln!("[build-db] SQLite initialized at {output_path}");

    // Build app_id → name lookup for FTS5 name column
    let mut app_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::with_capacity(export.apps.len());

    // Insert apps
    eprintln!("[build-db] Inserting {} apps...", export.apps.len());
    for app in &export.apps {
        insert_app(&conn, app)?;
        app_names.insert(app.app_id.clone(), app.name.clone());
    }

    // Insert arguments with embeddings
    let total = export.arguments.len();
    let mut ok = 0usize;
    let mut failed = 0usize;
    let start = Instant::now();
    let mut last_report = Instant::now();

    eprintln!("[build-db] Generating embeddings and inserting {total} arguments...");

    for (idx, arg) in export.arguments.iter().enumerate() {
        let app_name = app_names
            .get(&arg.app_id)
            .map(|s| s.as_str())
            .unwrap_or(&arg.app_id);

        match embed(&model, &tokenizer, &arg.description) {
            Ok(emb_bytes) => {
                match insert_argument(&conn, arg, app_name, &emb_bytes) {
                    Ok(()) => ok += 1,
                    Err(e) => {
                        eprintln!("[WARN] insert failed for {}: {e}", arg.cmd_path);
                        failed += 1;
                    }
                }
            }
            Err(e) => {
                eprintln!("[WARN] embed failed for {}: {e}", arg.cmd_path);
                failed += 1;
            }
        }

        // Progress report every 5 seconds
        if last_report.elapsed() >= Duration::from_secs(5) || idx + 1 == total {
            let pct = (idx + 1) * 100 / total;
            let elapsed = start.elapsed().as_secs_f64();
            let rate = (idx + 1) as f64 / elapsed;
            let eta = if rate > 0.0 {
                ((total - idx - 1) as f64 / rate) as u64
            } else {
                0
            };
            eprintln!(
                "[build-db] {}/{total} ({pct}%) — {rate:.0}/s — ETA {eta}s",
                idx + 1
            );
            last_report = Instant::now();
        }
    }

    // Write sync_meta timestamp
    conn.execute(
        "INSERT OR REPLACE INTO sync_meta (key, value) VALUES ('last_sync_time', ?1)",
        rusqlite::params![chrono::Utc::now().timestamp().to_string()],
    )?;

    eprintln!(
        "[build-db] Done: {ok} ok, {failed} failed. Elapsed: {:.1}s",
        start.elapsed().as_secs_f64()
    );

    if compress {
        let zst_path = format!("{output_path}.zst");
        eprintln!("[build-db] Compressing → {zst_path}...");
        let db_bytes = std::fs::read(&output_db)?;
        let compressed = zstd::encode_all(&db_bytes[..], 19)
            .context("zstd compression failed")?;
        std::fs::write(&zst_path, &compressed)?;

        let sha256 = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(&compressed);
            format!("{:x}", h.finalize())
        };

        let size_mb = compressed.len() as f64 / 1_048_576.0;
        eprintln!("[build-db] Compressed: {size_mb:.1} MB");
        eprintln!("[build-db] SHA-256: {sha256}");

        // Write manifest sidecar for upload reference
        let manifest = serde_json::json!({
            "sha256": sha256,
            "size_bytes": compressed.len(),
            "app_count": export.apps.len(),
            "command_count": ok,
            "built_at": chrono::Utc::now().to_rfc3339(),
        });
        std::fs::write(
            format!("{output_path}.manifest.json"),
            serde_json::to_string_pretty(&manifest)?,
        )?;
        eprintln!("[build-db] Manifest written to {output_path}.manifest.json");
    }

    Ok(())
}

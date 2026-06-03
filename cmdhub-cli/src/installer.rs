use crate::config::Config;
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::io::{self, Write};
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;

pub async fn ensure_model_installed(config: &Config) -> Result<PathBuf> {
    let default_path = crate::config::get_cache_dir().join("models/bge-micro-v2.onnx");
    let model_path = config
        .vector
        .model_path
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or(default_path);

    if model_path.exists() {
        return Ok(model_path);
    }

    if let Some(parent) = model_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create model parent directory")?;
    }

    let url = config
        .vector
        .model_url
        .as_deref()
        .unwrap_or("https://cdn.cmdhub.xyz/models/bge-micro-v2.onnx");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(
            config.timeout_seconds.max(60),
        ))
        .build()
        .context("Failed to build reqwest client for model download")?;

    eprintln!(
        "ONNX embedding model is missing. Downloading from {}...",
        url
    );

    let mut response = client
        .get(url)
        .send()
        .await
        .context("Failed to send model download request")?;

    if !response.status().is_success() {
        anyhow::bail!(
            "Server returned status code: {} when downloading model",
            response.status()
        );
    }

    let total_size = response.content_length().unwrap_or(0);

    let staging_path = model_path.with_extension("onnx.tmp");
    let mut file = tokio::fs::File::create(&staging_path)
        .await
        .context("Failed to create temporary staging file for model")?;

    let mut downloaded: u64 = 0;
    let mut last_progress_pct = 999; // force print on first chunk

    while let Some(chunk) = response
        .chunk()
        .await
        .context("Error downloading model chunk")?
    {
        file.write_all(&chunk)
            .await
            .context("Failed to write model chunk to file")?;
        downloaded += chunk.len() as u64;

        if let Some(progress_pct) = (downloaded * 100).checked_div(total_size) {
            let progress_pct = progress_pct as usize;
            if progress_pct != last_progress_pct {
                last_progress_pct = progress_pct;
                let bar_width = 30;
                let filled = progress_pct * bar_width / 100;
                let empty = bar_width - filled;
                let bar = format!(
                    "Downloading model: [{}{}] {}% ({:.1} MB / {:.1} MB)\r",
                    "=".repeat(filled),
                    " ".repeat(empty),
                    progress_pct,
                    (downloaded as f64) / 1_048_576.0,
                    (total_size as f64) / 1_048_576.0
                );
                let mut stderr = io::stderr();
                let _ = stderr.write_all(bar.as_bytes());
                let _ = stderr.flush();
            }
        } else {
            let bar = format!(
                "Downloading model: {:.1} MB...\r",
                (downloaded as f64) / 1_048_576.0
            );
            let mut stderr = io::stderr();
            let _ = stderr.write_all(bar.as_bytes());
            let _ = stderr.flush();
        }
    }
    eprintln!(); // newline to clear carriage return

    // Ensure staging file is synced to disk
    file.sync_all()
        .await
        .context("Failed to sync model file to disk")?;
    drop(file);

    // Calculate SHA-256 of downloaded file
    eprintln!("Verifying model integrity...");
    let file_bytes = std::fs::read(&staging_path).context("Failed to read staging model file")?;
    let mut hasher = Sha256::new();
    hasher.update(&file_bytes);
    let hash_str = format!("{:x}", hasher.finalize());
    let target_hash = config
        .vector
        .model_sha256
        .as_deref()
        .unwrap_or("d3b07384d113edec49eaa6238ad5ff00b192e2ad47a8a6cf23bdc1048b292e2a");

    if hash_str != target_hash {
        let _ = std::fs::remove_file(&staging_path);
        anyhow::bail!(
            "SHA-256 verification failed. Expected {}, got {}",
            target_hash,
            hash_str
        );
    }

    std::fs::rename(&staging_path, &model_path)
        .context("Failed to rename staging file to final model path")?;
    eprintln!("Model installed successfully to {:?}", model_path);

    Ok(model_path)
}

pub async fn install_vector(
    config: &Config,
    from_file: Option<PathBuf>,
    force: bool,
) -> Result<()> {
    let default_path = crate::config::get_cache_dir().join("models/bge-micro-v2.onnx");
    let model_path = config
        .vector
        .model_path
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or(default_path);

    if !force && model_path.exists() {
        println!("Model is already installed at {:?}", model_path);
        return Ok(());
    }

    if let Some(src_path) = from_file {
        if let Some(parent) = model_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        println!("Copying model from {:?} to {:?}...", src_path, model_path);
        std::fs::copy(&src_path, &model_path).context("Failed to copy custom model file")?;

        // SHA-256 verification of copied file
        let file_bytes = std::fs::read(&model_path)?;
        let mut hasher = Sha256::new();
        hasher.update(&file_bytes);
        let hash_str = format!("{:x}", hasher.finalize());
        let target_hash = config
            .vector
            .model_sha256
            .as_deref()
            .unwrap_or("d3b07384d113edec49eaa6238ad5ff00b192e2ad47a8a6cf23bdc1048b292e2a");
        if hash_str != target_hash {
            std::fs::remove_file(&model_path)?;
            anyhow::bail!(
                "SHA-256 verification failed. Expected {}, got {}",
                target_hash,
                hash_str
            );
        }
        println!("Model installed successfully to {:?}", model_path);
    } else {
        // Force re-download by deleting existing first
        if model_path.exists() {
            let _ = std::fs::remove_file(&model_path);
        }
        ensure_model_installed(config).await?;
    }
    Ok(())
}

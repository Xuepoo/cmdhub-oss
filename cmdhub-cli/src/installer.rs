use crate::config::Config;
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub async fn install_vector(
    config: &Config,
    from_file: Option<PathBuf>,
    force: bool,
) -> Result<()> {
    let default_path = crate::config::get_data_dir().join("models/bge-micro-v2.onnx");
    let model_path = config
        .vector
        .model_path
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or(default_path);

    if let Some(parent) = model_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if let Some(src_path) = from_file {
        println!("Copying model from {:?} to {:?}...", src_path, model_path);
        std::fs::copy(&src_path, &model_path).context("Failed to copy custom model file")?;
    } else {
        let url = config
            .vector
            .model_url
            .as_deref()
            .unwrap_or("https://cdn.cmdhub.xyz/models/bge-micro-v2.onnx");
        println!("Downloading model from {}...", url);
        let response = reqwest::get(url).await?.bytes().await?;
        std::fs::write(&model_path, response)?;
    }

    // Check SHA-256
    if !force {
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
    }
    println!("Model installed successfully to {:?}", model_path);
    Ok(())
}

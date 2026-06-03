use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct VectorConfig {
    pub model_url: Option<String>,
    pub model_path: Option<String>,
    pub model_sha256: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    pub api_url: String,
    pub public_key: String,
    pub timeout_seconds: u64,
    #[serde(default)]
    pub vector: VectorConfig,
}

pub const OFFICIAL_PUBLIC_KEY: [u8; 32] = [
    25, 127, 107, 35, 225, 108, 133, 50, 198, 171, 200, 56, 250, 205, 94, 167, 137, 190, 12, 118,
    178, 146, 3, 52, 3, 155, 250, 139, 61, 54, 141, 97,
];

impl Default for Config {
    fn default() -> Self {
        Self {
            api_url: "https://api.cmdhub.io/v1".to_string(),
            public_key: OFFICIAL_PUBLIC_KEY
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect(),
            timeout_seconds: 30,
            vector: VectorConfig::default(),
        }
    }
}

pub fn get_config_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("cmdhub");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/fuyu".to_string());
    PathBuf::from(home).join(".config").join("cmdhub")
}

pub fn get_data_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("cmdhub");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/fuyu".to_string());
    PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("cmdhub")
}

pub fn get_cache_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("cmdhub");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/fuyu".to_string());
    PathBuf::from(home).join(".cache").join("cmdhub")
}

pub fn resolve_config_path(custom_path: Option<PathBuf>) -> PathBuf {
    if let Some(path) = custom_path {
        path
    } else if let Ok(env_path) = std::env::var("CMDH_CONFIG") {
        if !env_path.is_empty() {
            PathBuf::from(env_path)
        } else {
            get_config_dir().join("config.toml")
        }
    } else {
        get_config_dir().join("config.toml")
    }
}

pub fn load_or_create_config(custom_path: Option<PathBuf>) -> Result<Config> {
    let config_path = resolve_config_path(custom_path);
    let default_xdg_path = get_config_dir().join("config.toml");

    if !config_path.exists() {
        if config_path != default_xdg_path {
            anyhow::bail!(
                "Custom configuration file does not exist at {:?}",
                config_path
            );
        }
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent).context("Failed to create config directory")?;
        }
        let default_config = Config::default();
        let toml_str = toml::to_string_pretty(&default_config)
            .context("Failed to serialize default config")?;
        fs::write(&config_path, toml_str).context("Failed to write default config file")?;
        eprintln!(
            "[INFO] Created default configuration file at: {}",
            config_path.display()
        );
        Ok(default_config)
    } else {
        let toml_str = fs::read_to_string(&config_path).context("Failed to read config file")?;
        let config: Config = toml::from_str(&toml_str).context("Failed to parse config TOML")?;
        Ok(config)
    }
}

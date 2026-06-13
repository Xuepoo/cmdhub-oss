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
    #[serde(default = "default_risk_guard_level")]
    pub risk_guard_level: String,
    #[serde(default)]
    pub vector: VectorConfig,
    #[serde(default)]
    pub output: OutputConfig,
    #[serde(default)]
    pub install: InstallConfig,
}

// Ed25519 public key for verifying the official offline database. The matching private
// key lives outside the repo (~/.config/cmdhub/keys/ed25519_private.bin) and signs each
// release's SHA-256(db.zst). Generated 2026-06-11; rotating it requires a client release.
pub const OFFICIAL_PUBLIC_KEY: [u8; 32] = [
    97, 228, 162, 92, 153, 11, 201, 252, 71, 48, 104, 125, 199, 128, 20, 60, 250, 189, 150, 94,
    170, 212, 223, 133, 120, 182, 137, 88, 220, 130, 171, 194,
];

impl Default for Config {
    fn default() -> Self {
        Self {
            api_url: "https://cdn.cmdhub.org".to_string(),
            public_key: OFFICIAL_PUBLIC_KEY
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect(),
            timeout_seconds: 30,
            risk_guard_level: default_risk_guard_level(),
            vector: VectorConfig::default(),
            output: OutputConfig::default(),
            install: InstallConfig::default(),
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
        let mut config: Config =
            toml::from_str(&toml_str).context("Failed to parse config TOML")?;

        // Cloud-native overrides via Environment Variables
        if let Ok(api_url) = std::env::var("CMDH_API_URL") {
            if !api_url.is_empty() {
                config.api_url = api_url;
            }
        }
        if let Ok(model_url) = std::env::var("CMDH_MODEL_URL") {
            if !model_url.is_empty() {
                config.vector.model_url = Some(model_url);
            }
        }

        Ok(config)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OutputConfig {
    #[serde(default = "default_output_mode")]
    pub mode: String, // "full", "usage", "minimal"
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            mode: default_output_mode(),
        }
    }
}

fn default_output_mode() -> String {
    "full".to_string()
}

fn default_risk_guard_level() -> String {
    "ask".to_string()
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct InstallConfig {
    pub os: Option<String>,
    #[serde(default = "default_package_managers")]
    pub package_managers: Vec<String>,
}

impl Default for InstallConfig {
    fn default() -> Self {
        Self {
            os: None,
            package_managers: default_package_managers(),
        }
    }
}

fn default_package_managers() -> Vec<String> {
    vec![
        "uv".to_string(),
        "npm".to_string(),
        "cargo".to_string(),
        "go".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_parsing_defaults() {
        let toml_str = r#"
            api_url = "https://cdn.cmdhub.org"
            public_key = "01020304"
            timeout_seconds = 30
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.risk_guard_level, "ask");
        assert_eq!(config.output.mode, "full");
        assert_eq!(config.install.os, None);
        assert_eq!(
            config.install.package_managers,
            vec![
                "uv".to_string(),
                "npm".to_string(),
                "cargo".to_string(),
                "go".to_string()
            ]
        );
    }
}

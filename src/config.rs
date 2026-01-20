use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub api_token: String,
    pub account_id: String,
    pub default_zone_id: String,
    pub default_zone_name: String,
    pub zones: Vec<ZoneConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoneConfig {
    pub id: String,
    pub name: String,
}

pub fn config_dir() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("Could not determine config directory")?
        .join("ytunnel");
    Ok(dir)
}

pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

pub fn load_config() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        anyhow::bail!(
            "ytunnel is not configured. Run `ytunnel init` first."
        );
    }
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config from {}", path.display()))?;
    let config: Config = toml::from_str(&contents)
        .with_context(|| "Failed to parse config file")?;
    Ok(config)
}

pub fn save_config(config: &Config) -> Result<()> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create config directory: {}", dir.display()))?;

    let path = config_path()?;
    let contents = toml::to_string_pretty(config)
        .context("Failed to serialize config")?;
    fs::write(&path, contents)
        .with_context(|| format!("Failed to write config to {}", path.display()))?;

    Ok(())
}

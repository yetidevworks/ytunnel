use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// A single Cloudflare account configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub name: String,
    pub api_token: String,
    pub account_id: String,
    pub default_zone_id: String,
    pub default_zone_name: String,
    pub zones: Vec<ZoneConfig>,
}

/// The main configuration with multi-account support
#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub selected_account: String,
    pub accounts: Vec<Account>,
}

impl Config {
    /// Get an account by name, or the selected account if name is None
    pub fn get_account(&self, name: Option<&str>) -> Result<&Account> {
        let account_name = name.unwrap_or(&self.selected_account);
        self.accounts
            .iter()
            .find(|a| a.name == account_name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Account '{}' not found. Run `ytunnel account list` to see available accounts.",
                    account_name
                )
            })
    }

    /// Get a mutable account by name, or the selected account if name is None
    pub fn get_account_mut(&mut self, name: Option<&str>) -> Result<&mut Account> {
        let account_name = name.unwrap_or(&self.selected_account).to_string();
        self.accounts
            .iter_mut()
            .find(|a| a.name == account_name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Account '{}' not found. Run `ytunnel account list` to see available accounts.",
                    account_name
                )
            })
    }

    /// Add a new account
    pub fn add_account(&mut self, account: Account) -> Result<()> {
        if self.accounts.iter().any(|a| a.name == account.name) {
            bail!("Account '{}' already exists", account.name);
        }
        self.accounts.push(account);
        Ok(())
    }

    /// Remove an account by name
    pub fn remove_account(&mut self, name: &str) -> Result<Account> {
        let pos = self
            .accounts
            .iter()
            .position(|a| a.name == name)
            .ok_or_else(|| anyhow::anyhow!("Account '{}' not found", name))?;

        let account = self.accounts.remove(pos);

        // If we removed the selected account, select another one
        if self.selected_account == name && !self.accounts.is_empty() {
            self.selected_account = self.accounts[0].name.clone();
        }

        Ok(account)
    }

    /// Set the selected account
    pub fn select_account(&mut self, name: &str) -> Result<()> {
        if !self.accounts.iter().any(|a| a.name == name) {
            bail!(
                "Account '{}' not found. Run `ytunnel account list` to see available accounts.",
                name
            );
        }
        self.selected_account = name.to_string();
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoneConfig {
    pub id: String,
    pub name: String,
}

/// Legacy config format for migration
#[derive(Debug, Deserialize)]
struct LegacyConfig {
    api_token: String,
    account_id: String,
    default_zone_id: String,
    default_zone_name: String,
    zones: Vec<ZoneConfig>,
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
        anyhow::bail!("ytunnel is not configured. Run `ytunnel init` first.");
    }
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config from {}", path.display()))?;

    // Try new format first
    if let Ok(config) = toml::from_str::<Config>(&contents) {
        return Ok(config);
    }

    // Try legacy format and migrate
    if let Ok(legacy) = toml::from_str::<LegacyConfig>(&contents) {
        let account_name = "default".to_string();
        eprintln!(
            "Migrating config to multi-account format (account: '{}')...",
            account_name
        );
        let config = Config {
            selected_account: account_name.clone(),
            accounts: vec![Account {
                name: account_name,
                api_token: legacy.api_token,
                account_id: legacy.account_id,
                default_zone_id: legacy.default_zone_id,
                default_zone_name: legacy.default_zone_name,
                zones: legacy.zones,
            }],
        };
        save_config(&config)?;
        return Ok(config);
    }

    bail!("Invalid config format")
}

pub fn save_config(config: &Config) -> Result<()> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create config directory: {}", dir.display()))?;

    let path = config_path()?;
    let contents = toml::to_string_pretty(config).context("Failed to serialize config")?;
    fs::write(&path, contents)
        .with_context(|| format!("Failed to write config to {}", path.display()))?;

    Ok(())
}

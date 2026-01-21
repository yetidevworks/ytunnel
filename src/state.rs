use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::config;

// Represents the current runtime status of a tunnel
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelStatus {
    Running,
    Stopped,
    Error,
}

impl TunnelStatus {
    pub fn symbol(&self) -> &'static str {
        match self {
            TunnelStatus::Running => "●",
            TunnelStatus::Stopped => "○",
            TunnelStatus::Error => "✗",
        }
    }
}

// A persistent tunnel configuration stored in tunnels.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistentTunnel {
    pub name: String,
    // Which account owns this tunnel (defaults to selected account for migration)
    #[serde(default)]
    pub account_name: String,
    pub target: String,
    pub zone_id: String,
    pub zone_name: String,
    pub hostname: String,
    pub tunnel_id: String,
    pub enabled: bool,
    // Whether to auto-start on login (RunAtLoad in launchd)
    #[serde(default)]
    pub auto_start: bool,
    // Port for cloudflared metrics endpoint (optional, calculated if not set)
    #[serde(default)]
    pub metrics_port: Option<u16>,
}

impl PersistentTunnel {
    // Get the path to the credentials file for this tunnel
    pub fn credentials_path(&self) -> Result<PathBuf> {
        let config_dir = config::config_dir()?;
        Ok(config_dir.join(format!("{}.json", self.tunnel_id)))
    }

    // Get the path to the tunnel config file
    pub fn config_path(&self) -> Result<PathBuf> {
        let config_dir = config::config_dir()?;
        let configs_dir = config_dir.join("tunnel-configs");
        Ok(configs_dir.join(format!("{}.yml", self.name)))
    }

    // Get the path to the log file for this tunnel
    pub fn log_path(&self) -> Result<PathBuf> {
        let config_dir = config::config_dir()?;
        let logs_dir = config_dir.join("logs");
        Ok(logs_dir.join(format!("{}.log", self.name)))
    }

    // Get the metrics port for this tunnel (calculates from name hash if not set)
    pub fn get_metrics_port(&self) -> u16 {
        self.metrics_port.unwrap_or_else(|| {
            // Calculate a port based on the tunnel name hash
            // Range: 21000-21999 to avoid conflicts with cloudflared defaults (20241-20245)
            let hash: u32 = self
                .name
                .bytes()
                .fold(0u32, |acc, b| acc.wrapping_add(b as u32).wrapping_mul(31));
            21000 + (hash % 1000) as u16
        })
    }

    // Get the metrics URL for this tunnel
    pub fn metrics_url(&self) -> String {
        format!("http://localhost:{}/metrics", self.get_metrics_port())
    }
}

// The collection of all persistent tunnels
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TunnelState {
    #[serde(default)]
    pub tunnels: Vec<PersistentTunnel>,
}

impl TunnelState {
    // Load the tunnel state from disk
    pub fn load() -> Result<Self> {
        let path = tunnels_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read tunnels from {}", path.display()))?;

        let state: TunnelState =
            toml::from_str(&contents).with_context(|| "Failed to parse tunnels.toml")?;

        Ok(state)
    }

    // Load tunnel state and migrate any tunnels with empty account_name
    // to the specified default account
    pub fn load_and_migrate(default_account: &str) -> Result<Self> {
        let mut state = Self::load()?;

        // Check if any tunnels need migration
        let needs_migration = state.tunnels.iter().any(|t| t.account_name.is_empty());

        if needs_migration {
            for tunnel in &mut state.tunnels {
                if tunnel.account_name.is_empty() {
                    tunnel.account_name = default_account.to_string();
                }
            }
            // Save the migrated state
            state.save()?;
        }

        Ok(state)
    }

    // Save the tunnel state to disk
    pub fn save(&self) -> Result<()> {
        let dir = config::config_dir()?;
        fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create config directory: {}", dir.display()))?;

        let path = tunnels_path()?;
        let contents = toml::to_string_pretty(self).context("Failed to serialize tunnels")?;
        fs::write(&path, contents)
            .with_context(|| format!("Failed to write tunnels to {}", path.display()))?;

        Ok(())
    }

    // Find a tunnel by name (searches all accounts)
    pub fn find(&self, name: &str) -> Option<&PersistentTunnel> {
        self.tunnels.iter().find(|t| t.name == name)
    }

    // Find a tunnel by name (mutable, searches all accounts)
    pub fn find_mut(&mut self, name: &str) -> Option<&mut PersistentTunnel> {
        self.tunnels.iter_mut().find(|t| t.name == name)
    }

    // Find a tunnel by name for a specific account
    pub fn find_for_account(&self, name: &str, account: &str) -> Option<&PersistentTunnel> {
        self.tunnels
            .iter()
            .find(|t| t.name == name && t.account_name == account)
    }

    // Find a tunnel by name for a specific account (mutable)
    pub fn find_for_account_mut(
        &mut self,
        name: &str,
        account: &str,
    ) -> Option<&mut PersistentTunnel> {
        self.tunnels
            .iter_mut()
            .find(|t| t.name == name && t.account_name == account)
    }

    // Get all tunnels for a specific account
    pub fn tunnels_for_account(&self, account: &str) -> Vec<&PersistentTunnel> {
        self.tunnels
            .iter()
            .filter(|t| t.account_name == account)
            .collect()
    }

    // Add a new tunnel
    pub fn add(&mut self, tunnel: PersistentTunnel) {
        self.tunnels.push(tunnel);
    }

    // Remove a tunnel by name (from any account)
    pub fn remove(&mut self, name: &str) -> Option<PersistentTunnel> {
        if let Some(pos) = self.tunnels.iter().position(|t| t.name == name) {
            Some(self.tunnels.remove(pos))
        } else {
            None
        }
    }

    // Remove a tunnel by name for a specific account
    pub fn remove_for_account(&mut self, name: &str, account: &str) -> Option<PersistentTunnel> {
        if let Some(pos) = self
            .tunnels
            .iter()
            .position(|t| t.name == name && t.account_name == account)
        {
            Some(self.tunnels.remove(pos))
        } else {
            None
        }
    }
}

// Get the path to the tunnels.toml file
pub fn tunnels_path() -> Result<PathBuf> {
    Ok(config::config_dir()?.join("tunnels.toml"))
}

// Ensure the tunnel-configs directory exists
pub fn ensure_configs_dir() -> Result<PathBuf> {
    let config_dir = config::config_dir()?;
    let configs_dir = config_dir.join("tunnel-configs");
    fs::create_dir_all(&configs_dir).with_context(|| {
        format!(
            "Failed to create configs directory: {}",
            configs_dir.display()
        )
    })?;
    Ok(configs_dir)
}

// Ensure the logs directory exists
pub fn ensure_logs_dir() -> Result<PathBuf> {
    let config_dir = config::config_dir()?;
    let logs_dir = config_dir.join("logs");
    fs::create_dir_all(&logs_dir)
        .with_context(|| format!("Failed to create logs directory: {}", logs_dir.display()))?;
    Ok(logs_dir)
}

// Generate the cloudflared config YAML content for a tunnel
pub fn generate_tunnel_config(tunnel: &PersistentTunnel) -> Result<String> {
    let credentials_path = tunnel.credentials_path()?;

    // Normalize target URL
    let target_url =
        if tunnel.target.starts_with("http://") || tunnel.target.starts_with("https://") {
            tunnel.target.clone()
        } else {
            format!("http://{}", tunnel.target)
        };

    let config = format!(
        r#"tunnel: {tunnel_id}
credentials-file: {credentials_path}
ingress:
  - hostname: {hostname}
    service: {target_url}
  - service: http_status:404
"#,
        tunnel_id = tunnel.tunnel_id,
        credentials_path = credentials_path.display(),
        hostname = tunnel.hostname,
        target_url = target_url
    );

    Ok(config)
}

// Write the cloudflared config file for a tunnel
pub fn write_tunnel_config(tunnel: &PersistentTunnel) -> Result<PathBuf> {
    ensure_configs_dir()?;
    let config_path = tunnel.config_path()?;
    let config_content = generate_tunnel_config(tunnel)?;
    fs::write(&config_path, &config_content)
        .with_context(|| format!("Failed to write tunnel config to {}", config_path.display()))?;
    Ok(config_path)
}

use anyhow::Result;
use crossterm::{
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::time::Duration;

use crate::cloudflare;
use crate::config;
use crate::config::Account;
use crate::daemon;
use crate::metrics::TunnelMetrics;
use crate::state::{write_tunnel_config, PersistentTunnel, TunnelState, TunnelStatus};

use super::ui;

// Parse an ephemeral tunnel's config file to extract hostname and target
fn parse_ephemeral_config(tunnel_id: &str) -> Option<(String, String)> {
    let config_dir = crate::config::config_dir().ok()?;
    let config_path = config_dir.join(format!("tunnel-{}.yml", tunnel_id));

    if !config_path.exists() {
        return None;
    }

    let content = std::fs::read_to_string(&config_path).ok()?;

    // Parse simple YAML - look for hostname and service lines
    // Format is:
    //   ingress:
    //     - hostname: example.com
    //       service: http://localhost:8080
    //     - service: http_status:404
    let mut hostname = None;
    let mut service = None;

    for line in content.lines() {
        // Strip leading whitespace and list marker
        let line = line.trim().trim_start_matches('-').trim();

        if line.starts_with("hostname:") {
            hostname = line.strip_prefix("hostname:").map(|s| s.trim().to_string());
        } else if line.starts_with("service:") {
            let svc = line.strip_prefix("service:").map(|s| s.trim().to_string());
            // Skip the fallback http_status:404 service, take first real service
            if let Some(ref s) = svc {
                if !s.contains("http_status") && service.is_none() {
                    service = svc;
                }
            }
        }
    }

    match (hostname, service) {
        (Some(h), Some(s)) => Some((h, s)),
        _ => None,
    }
}

// Input mode for the TUI
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    AddName,
    AddTarget,
    AddZone,
    Confirm,
    Help,
}

// Whether a tunnel is managed (persistent) or ephemeral
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelKind {
    // Managed tunnel with launchd daemon
    Managed,
    // Ephemeral tunnel (created with `ytunnel run`, not in state)
    Ephemeral,
}

// Health check status for a tunnel
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HealthStatus {
    #[default]
    Unknown,
    Healthy,
    Unhealthy,
    Checking,
}

// Historical metrics for sparkline display
#[derive(Debug, Clone, Default)]
pub struct MetricsHistory {
    // Request counts over time (last 30 samples)
    pub request_samples: Vec<u64>,
    // Last known total requests (to calculate delta)
    pub last_total: u64,
}

impl MetricsHistory {
    const MAX_SAMPLES: usize = 30;

    // Record a new sample
    pub fn record(&mut self, total_requests: u64) {
        // Calculate delta since last sample
        let delta = if total_requests >= self.last_total {
            total_requests - self.last_total
        } else {
            // Counter reset, just use total
            total_requests
        };

        self.request_samples.push(delta);

        // Keep only last N samples
        if self.request_samples.len() > Self::MAX_SAMPLES {
            self.request_samples.remove(0);
        }

        self.last_total = total_requests;
    }

    // Generate sparkline string using Unicode blocks
    pub fn sparkline(&self) -> String {
        if self.request_samples.is_empty() {
            return String::new();
        }

        let blocks = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let max = *self.request_samples.iter().max().unwrap_or(&1).max(&1);

        self.request_samples
            .iter()
            .map(|&val| {
                let idx = if max > 0 {
                    ((val as f64 / max as f64) * 7.0).round() as usize
                } else {
                    0
                };
                blocks[idx.min(7)]
            })
            .collect()
    }
}

// A tunnel entry with its runtime status
#[derive(Debug, Clone)]
pub struct TunnelEntry {
    pub tunnel: PersistentTunnel,
    pub status: TunnelStatus,
    pub kind: TunnelKind,
    pub metrics: Option<TunnelMetrics>,
    pub metrics_history: MetricsHistory,
    pub health: HealthStatus,
}

// Application state
pub struct App {
    // Current input mode
    pub input_mode: InputMode,
    // List of tunnels with status
    pub tunnels: Vec<TunnelEntry>,
    // Currently selected tunnel index
    pub selected: usize,
    // Log lines for the selected tunnel
    pub logs: Vec<String>,
    // Input buffer for add dialog
    pub input: String,
    // Temporary storage for new tunnel name during add flow
    pub new_tunnel_name: Option<String>,
    // Temporary storage for new tunnel target during add flow
    pub new_tunnel_target: Option<String>,
    // Available zones for selection
    pub zones: Vec<config::ZoneConfig>,
    // Selected zone index during add flow
    pub zone_selected: usize,
    // Confirmation message
    pub confirm_message: Option<String>,
    // Action to perform on confirmation
    pub pending_action: Option<PendingAction>,
    // Status message to display
    pub status_message: Option<String>,
    // Should quit
    pub should_quit: bool,
    // Config loaded
    pub config: Option<config::Config>,
    // Whether we're importing (vs adding) a tunnel
    pub is_importing: bool,
    // Available accounts
    pub accounts: Vec<Account>,
    // Selected account index
    pub selected_account_idx: usize,
}

// Actions that require confirmation
#[derive(Debug, Clone)]
pub enum PendingAction {
    Delete(String),
}

impl App {
    pub fn new(initial_account: Option<&str>) -> Self {
        // Try to load config and determine initial account index
        let (config, accounts, selected_account_idx) = if let Ok(cfg) = config::load_config() {
            let accounts = cfg.accounts.clone();
            let idx = if let Some(name) = initial_account {
                accounts.iter().position(|a| a.name == name).unwrap_or(0)
            } else {
                accounts
                    .iter()
                    .position(|a| a.name == cfg.selected_account)
                    .unwrap_or(0)
            };
            (Some(cfg), accounts, idx)
        } else {
            (None, Vec::new(), 0)
        };

        Self {
            input_mode: InputMode::Normal,
            tunnels: Vec::new(),
            selected: 0,
            logs: vec!["Select a tunnel to view logs".to_string()],
            input: String::new(),
            new_tunnel_name: None,
            new_tunnel_target: None,
            zones: Vec::new(),
            zone_selected: 0,
            confirm_message: None,
            pending_action: None,
            status_message: None,
            should_quit: false,
            config,
            is_importing: false,
            accounts,
            selected_account_idx,
        }
    }

    // Get the current account name
    pub fn current_account_name(&self) -> &str {
        self.accounts
            .get(self.selected_account_idx)
            .map(|a| a.name.as_str())
            .unwrap_or("default")
    }

    // Get the current account
    pub fn current_account(&self) -> Option<&Account> {
        self.accounts.get(self.selected_account_idx)
    }

    // Switch to the next account
    pub fn next_account(&mut self) {
        if !self.accounts.is_empty() {
            self.selected_account_idx = (self.selected_account_idx + 1) % self.accounts.len();
            self.status_message = Some(format!(
                "Switched to account: {}",
                self.current_account_name()
            ));
        }
    }

    // Switch to the previous account
    // Load tunnels and their statuses
    pub async fn load_tunnels(&mut self) -> Result<()> {
        // Load config
        self.config = config::load_config().ok();
        if let Some(ref cfg) = self.config {
            self.accounts = cfg.accounts.clone();
            // Update zones from current account
            if let Some(acct) = self.current_account() {
                self.zones = acct.zones.clone();
            }
        }

        // Get current account name for filtering
        let current_account_name = self.current_account_name().to_string();

        // Load tunnel state with migration (assigns empty account_name to first account,
        // since that's typically the original account before multi-account was added)
        let first_account = self
            .accounts
            .first()
            .map(|a| a.name.as_str())
            .unwrap_or(&current_account_name);
        let state = TunnelState::load_and_migrate(first_account)?;
        // Only get tunnels for the current account
        let managed_tunnels: Vec<_> = state.tunnels_for_account(&current_account_name);
        let managed_names: std::collections::HashSet<String> =
            managed_tunnels.iter().map(|t| t.name.clone()).collect();

        // Get status for each managed tunnel
        let mut entries = Vec::new();
        for tunnel in managed_tunnels.into_iter().cloned() {
            let status = daemon::get_daemon_status(&tunnel).await;
            // Fetch metrics for running tunnels
            let (metrics, mut history) = if status == TunnelStatus::Running {
                let m = TunnelMetrics::fetch(&tunnel.metrics_url()).await;
                if m.available {
                    let mut h = MetricsHistory::default();
                    h.record(m.total_requests);
                    (Some(m), h)
                } else {
                    (None, MetricsHistory::default())
                }
            } else {
                (None, MetricsHistory::default())
            };

            // Preserve existing history and health if we have it
            let mut health = HealthStatus::Unknown;
            if let Some(existing) = self.tunnels.iter().find(|e| e.tunnel.name == tunnel.name) {
                history = existing.metrics_history.clone();
                health = existing.health;
                if let Some(ref m) = metrics {
                    history.record(m.total_requests);
                }
            }

            entries.push(TunnelEntry {
                tunnel,
                status,
                kind: TunnelKind::Managed,
                metrics,
                metrics_history: history,
                health,
            });
        }

        // Query Cloudflare for ephemeral tunnels (ytunnel-* not in state)
        if let Some(acct) = self.current_account() {
            let client = cloudflare::Client::new(&acct.api_token);
            if let Ok(cf_tunnels) = client.list_tunnels(&acct.account_id).await {
                for cf_tunnel in cf_tunnels {
                    // Skip deleted tunnels
                    if cf_tunnel.deleted_at.is_some() {
                        continue;
                    }

                    // Only consider ytunnel-* tunnels
                    if !cf_tunnel.name.starts_with("ytunnel-") {
                        continue;
                    }

                    // Extract the short name (without ytunnel- prefix)
                    let short_name = cf_tunnel
                        .name
                        .strip_prefix("ytunnel-")
                        .unwrap_or(&cf_tunnel.name);

                    // Skip if already managed
                    if managed_names.contains(short_name) {
                        continue;
                    }

                    // This is an ephemeral tunnel - try to read its config file
                    let (hostname, target) = parse_ephemeral_config(&cf_tunnel.id)
                        .unwrap_or_else(|| (short_name.to_string(), "unknown".to_string()));

                    // Try to determine the zone from the hostname
                    let (zone_id, zone_name) = if hostname.contains('.') {
                        // Try to match hostname to a known zone
                        acct.zones
                            .iter()
                            .find(|z| hostname.ends_with(&z.name))
                            .map(|z| (z.id.clone(), z.name.clone()))
                            .unwrap_or_default()
                    } else {
                        (String::new(), String::new())
                    };

                    let ephemeral = PersistentTunnel {
                        name: short_name.to_string(),
                        account_name: current_account_name.clone(),
                        target,
                        zone_id,
                        zone_name,
                        hostname,
                        tunnel_id: cf_tunnel.id.clone(),
                        enabled: false,
                        auto_start: false,
                        metrics_port: None,
                    };

                    // Check if config file exists (means tunnel is actively running)
                    let config_dir = crate::config::config_dir().ok();
                    let config_exists = config_dir
                        .map(|d| d.join(format!("tunnel-{}.yml", cf_tunnel.id)).exists())
                        .unwrap_or(false);

                    let status = if config_exists {
                        TunnelStatus::Running
                    } else {
                        TunnelStatus::Stopped
                    };

                    entries.push(TunnelEntry {
                        tunnel: ephemeral,
                        status,
                        kind: TunnelKind::Ephemeral,
                        metrics: None,
                        metrics_history: MetricsHistory::default(),
                        health: HealthStatus::Unknown,
                    });
                }
            }
        }

        self.tunnels = entries;

        // Ensure selected index is valid
        if self.selected >= self.tunnels.len() && !self.tunnels.is_empty() {
            self.selected = self.tunnels.len() - 1;
        }

        // Load logs for selected tunnel
        self.refresh_logs();

        Ok(())
    }

    // Refresh logs for the selected tunnel
    pub fn refresh_logs(&mut self) {
        if let Some(entry) = self.tunnels.get(self.selected) {
            match entry.kind {
                TunnelKind::Managed => match daemon::read_log_tail(&entry.tunnel, 100) {
                    Ok(lines) => self.logs = lines,
                    Err(e) => self.logs = vec![format!("Error reading logs: {}", e)],
                },
                TunnelKind::Ephemeral => {
                    let has_config =
                        entry.tunnel.target != "unknown" && !entry.tunnel.target.is_empty();
                    self.logs = if has_config {
                        vec![
                            "Ephemeral tunnel (created with `ytunnel run`)".to_string(),
                            String::new(),
                            format!("Hostname: {}", entry.tunnel.hostname),
                            format!("Target:   {}", entry.tunnel.target),
                            if !entry.tunnel.zone_name.is_empty() {
                                format!("Zone:     {}", entry.tunnel.zone_name)
                            } else {
                                "Zone:     (will prompt)".to_string()
                            },
                            String::new(),
                            "Press [m] to import as managed tunnel".to_string(),
                            "Press [d] to delete from Cloudflare".to_string(),
                        ]
                    } else {
                        vec![
                            "Ephemeral tunnel (created with `ytunnel run`)".to_string(),
                            String::new(),
                            "Config not found - tunnel may not be running.".to_string(),
                            String::new(),
                            "Press [m] to import (will prompt for target)".to_string(),
                            "Press [d] to delete from Cloudflare".to_string(),
                        ]
                    };
                }
            }
        } else {
            self.logs = vec!["No tunnel selected".to_string()];
        }
    }

    // Refresh metrics for the selected tunnel
    pub async fn refresh_metrics(&mut self) {
        if let Some(entry) = self.tunnels.get_mut(self.selected) {
            if entry.kind == TunnelKind::Managed && entry.status == TunnelStatus::Running {
                let metrics = TunnelMetrics::fetch(&entry.tunnel.metrics_url()).await;
                if metrics.available {
                    entry.metrics_history.record(metrics.total_requests);
                    entry.metrics = Some(metrics);
                } else {
                    entry.metrics = None;
                }
            }
        }
    }

    // Check health of the selected tunnel by making an HTTP request
    pub async fn check_health(&mut self) {
        self.check_health_for_index(self.selected).await;
    }

    // Check health of all running tunnels
    pub async fn check_all_health(&mut self) {
        for i in 0..self.tunnels.len() {
            if self.tunnels[i].status == TunnelStatus::Running {
                self.check_health_for_index(i).await;
            }
        }
    }

    // Check health for a specific tunnel by index
    async fn check_health_for_index(&mut self, index: usize) {
        if let Some(entry) = self.tunnels.get_mut(index) {
            if entry.status != TunnelStatus::Running {
                entry.health = HealthStatus::Unknown;
                if index == self.selected {
                    self.status_message = Some("Tunnel not running".to_string());
                }
                return;
            }

            let previous_health = entry.health;
            let tunnel_name = entry.tunnel.name.clone();
            let hostname = entry.tunnel.hostname.clone();
            let is_selected = index == self.selected;

            entry.health = HealthStatus::Checking;
            if is_selected {
                self.status_message = Some(format!("Checking health of {}...", tunnel_name));
            }

            let url = format!("https://{}", hostname);

            // Simple HTTP HEAD request with short timeout
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .danger_accept_invalid_certs(true) // In case of self-signed certs
                .build();

            let result = match client {
                Ok(c) => c.head(&url).send().await,
                Err(_) => {
                    if let Some(entry) = self.tunnels.get_mut(index) {
                        entry.health = HealthStatus::Unhealthy;
                    }
                    self.show_health_result(&tunnel_name, previous_health, HealthStatus::Unhealthy);
                    return;
                }
            };

            let new_health = match result {
                Ok(resp) if resp.status().is_success() || resp.status().is_redirection() => {
                    HealthStatus::Healthy
                }
                Ok(resp) if resp.status().is_server_error() => HealthStatus::Unhealthy,
                Ok(_) => HealthStatus::Healthy, // 4xx is still "reachable"
                Err(_) => HealthStatus::Unhealthy,
            };

            if let Some(entry) = self.tunnels.get_mut(index) {
                entry.health = new_health;
            }
            self.show_health_result(&tunnel_name, previous_health, new_health);
        }
    }

    // Show health check result and send notifications for state changes
    fn show_health_result(&mut self, tunnel_name: &str, old: HealthStatus, new: HealthStatus) {
        // Always show the result in status bar
        match new {
            HealthStatus::Healthy => {
                self.status_message = Some(format!("✓ {} is healthy", tunnel_name));
            }
            HealthStatus::Unhealthy => {
                self.status_message = Some(format!("✗ {} is unreachable", tunnel_name));
            }
            _ => {}
        }

        // Send system notification only on meaningful transitions
        match (old, new) {
            (HealthStatus::Healthy, HealthStatus::Unhealthy) => {
                self.status_message = Some(format!("⚠️  Tunnel '{}' is DOWN!", tunnel_name));
                self.send_system_notification(
                    &format!("Tunnel Down: {}", tunnel_name),
                    "The tunnel is no longer reachable",
                );
            }
            (HealthStatus::Unhealthy, HealthStatus::Healthy) => {
                self.status_message = Some(format!("✓ Tunnel '{}' is back UP", tunnel_name));
                self.send_system_notification(
                    &format!("Tunnel Up: {}", tunnel_name),
                    "The tunnel is now reachable",
                );
            }
            _ => {}
        }
    }

    // Send a system notification
    fn send_system_notification(&self, title: &str, message: &str) {
        use std::process::Command;

        #[cfg(target_os = "macos")]
        {
            // Try terminal-notifier first, fall back to osascript
            let result = Command::new("terminal-notifier")
                .args(["-title", title, "-message", message, "-sound", "default"])
                .spawn();

            if result.is_err() {
                // Fall back to osascript
                let script = format!(
                    r#"display notification "{}" with title "{}""#,
                    message.replace('"', r#"\""#),
                    title.replace('"', r#"\""#)
                );
                Command::new("osascript").args(["-e", &script]).spawn().ok();
            }
        }

        #[cfg(target_os = "linux")]
        {
            // Use notify-send on Linux
            Command::new("notify-send")
                .args([title, message])
                .spawn()
                .ok();
        }
    }

    // Get health status for the selected tunnel
    pub fn selected_health(&self) -> HealthStatus {
        self.tunnels
            .get(self.selected)
            .map(|e| e.health)
            .unwrap_or(HealthStatus::Unknown)
    }

    // Get metrics for the selected tunnel
    pub fn selected_metrics(&self) -> Option<&TunnelMetrics> {
        self.tunnels
            .get(self.selected)
            .and_then(|e| e.metrics.as_ref())
    }

    // Get sparkline for the selected tunnel
    pub fn selected_sparkline(&self) -> String {
        self.tunnels
            .get(self.selected)
            .map(|e| e.metrics_history.sparkline())
            .unwrap_or_default()
    }

    // Move selection up
    pub fn select_previous(&mut self) -> bool {
        if !self.tunnels.is_empty() && self.selected > 0 {
            self.selected -= 1;
            self.refresh_logs();
            return true; // Selection changed
        }
        false
    }

    // Move selection down
    pub fn select_next(&mut self) -> bool {
        if !self.tunnels.is_empty() && self.selected < self.tunnels.len() - 1 {
            self.selected += 1;
            self.refresh_logs();
            return true; // Selection changed
        }
        false
    }

    // Check if selected tunnel needs a health check (unknown or stale)
    pub fn selected_needs_health_check(&self) -> bool {
        self.tunnels
            .get(self.selected)
            .map(|e| e.status == TunnelStatus::Running && e.health == HealthStatus::Unknown)
            .unwrap_or(false)
    }

    // Start the add tunnel flow
    pub fn start_add(&mut self) {
        if self.config.is_none() {
            self.status_message = Some("Run 'ytunnel init' first".to_string());
            return;
        }
        self.input_mode = InputMode::AddName;
        self.input.clear();
        self.new_tunnel_name = None;
        self.new_tunnel_target = None;
        self.zone_selected = 0;
        self.is_importing = false;
    }

    // Cancel current input
    pub fn cancel_input(&mut self) {
        self.input_mode = InputMode::Normal;
        self.input.clear();
        self.new_tunnel_name = None;
        self.new_tunnel_target = None;
        self.confirm_message = None;
        self.pending_action = None;
    }

    // Move to next step in add flow
    pub fn next_add_step(&mut self) {
        match self.input_mode {
            InputMode::AddName => {
                if !self.input.is_empty() {
                    // Check if name already exists
                    if self.tunnels.iter().any(|t| t.tunnel.name == self.input) {
                        self.status_message =
                            Some(format!("Tunnel '{}' already exists", self.input));
                        return;
                    }
                    self.new_tunnel_name = Some(self.input.clone());
                    self.input.clear();
                    self.input_mode = InputMode::AddTarget;
                }
            }
            InputMode::AddTarget => {
                if !self.input.is_empty() {
                    self.new_tunnel_target = Some(self.input.clone());
                    self.input.clear();
                    self.input_mode = InputMode::AddZone;
                }
            }
            _ => {}
        }
    }

    // Select zone in add flow (moves selection or confirms)
    pub fn select_zone_next(&mut self) {
        if !self.zones.is_empty() && self.zone_selected < self.zones.len() - 1 {
            self.zone_selected += 1;
        }
    }

    pub fn select_zone_prev(&mut self) {
        if self.zone_selected > 0 {
            self.zone_selected -= 1;
        }
    }

    // Complete the add tunnel flow
    pub async fn complete_add(&mut self) -> Result<()> {
        let name = self.new_tunnel_name.take().unwrap();
        let target = self.new_tunnel_target.take().unwrap();
        let zone = self.zones.get(self.zone_selected).unwrap().clone();

        let acct = self
            .current_account()
            .ok_or_else(|| anyhow::anyhow!("No account selected"))?
            .clone();
        let client = cloudflare::Client::new(&acct.api_token);

        let tunnel_name = format!("ytunnel-{}", name);
        let hostname = format!("{}.{}", name, zone.name);

        self.status_message = Some(format!("Creating tunnel {}...", name));

        // Check if tunnel exists, create if not
        let (tunnel, _credentials_path) = match client
            .get_tunnel_by_name(&acct.account_id, &tunnel_name)
            .await?
        {
            Some(t) => {
                let creds_path = t.credentials_path()?;
                if !creds_path.exists() {
                    anyhow::bail!("Credentials missing for existing tunnel");
                }
                (t, creds_path)
            }
            None => {
                let result = client.create_tunnel(&acct.account_id, &tunnel_name).await?;
                (result.tunnel, result.credentials_path)
            }
        };

        // Ensure DNS record exists
        client
            .ensure_dns_record(&zone.id, &hostname, &tunnel.id)
            .await?;

        // Create persistent tunnel
        let persistent = PersistentTunnel {
            name: name.clone(),
            account_name: acct.name.clone(),
            target,
            zone_id: zone.id,
            zone_name: zone.name,
            hostname,
            tunnel_id: tunnel.id,
            enabled: true,
            auto_start: false,
            metrics_port: None,
        };

        // Write tunnel config
        write_tunnel_config(&persistent)?;

        // Install daemon
        daemon::install_daemon(&persistent).await?;

        // Save to state
        let mut state = TunnelState::load()?;
        state.add(persistent);
        state.save()?;

        // Start the daemon
        daemon::start_daemon(&name, &acct.name).await?;

        self.input_mode = InputMode::Normal;
        self.status_message = Some(format!("Tunnel '{}' created and started", name));

        // Reload tunnels
        self.load_tunnels().await?;

        // Select the new tunnel
        if let Some(pos) = self.tunnels.iter().position(|t| t.tunnel.name == name) {
            self.selected = pos;
            self.refresh_logs();
        }

        Ok(())
    }

    // Start the selected tunnel
    pub async fn start_selected(&mut self) -> Result<()> {
        if let Some(entry) = self.tunnels.get(self.selected) {
            if entry.kind == TunnelKind::Ephemeral {
                self.status_message =
                    Some("Cannot start ephemeral tunnel. Import it first with 'm'.".to_string());
                return Ok(());
            }

            let name = entry.tunnel.name.clone();
            let account_name = entry.tunnel.account_name.clone();
            self.status_message = Some(format!("Starting {}...", name));

            // Ensure config file exists
            write_tunnel_config(&entry.tunnel)?;

            // Ensure daemon is installed
            daemon::install_daemon(&entry.tunnel).await?;

            // Start daemon
            daemon::start_daemon(&name, &account_name).await?;

            // Update state
            let mut state = TunnelState::load()?;
            if let Some(t) = state.find_mut(&name) {
                t.enabled = true;
            }
            state.save()?;

            self.status_message = Some(format!("Started {}", name));
            self.load_tunnels().await?;
        }
        Ok(())
    }

    // Stop the selected tunnel
    pub async fn stop_selected(&mut self) -> Result<()> {
        if let Some(entry) = self.tunnels.get(self.selected) {
            if entry.kind == TunnelKind::Ephemeral {
                self.status_message = Some(
                    "Cannot stop ephemeral tunnel from TUI. Use Ctrl+C in its terminal."
                        .to_string(),
                );
                return Ok(());
            }

            let name = entry.tunnel.name.clone();
            let account_name = entry.tunnel.account_name.clone();
            self.status_message = Some(format!("Stopping {}...", name));

            daemon::stop_daemon(&name, &account_name).await?;

            // Update state
            let mut state = TunnelState::load()?;
            if let Some(t) = state.find_mut(&name) {
                t.enabled = false;
            }
            state.save()?;

            self.status_message = Some(format!("Stopped {}", name));
            self.load_tunnels().await?;
        }
        Ok(())
    }

    // Restart the selected tunnel (stop then start, reinstalls daemon config)
    pub async fn restart_selected(&mut self) -> Result<()> {
        if let Some(entry) = self.tunnels.get(self.selected) {
            if entry.kind == TunnelKind::Ephemeral {
                self.status_message =
                    Some("Cannot restart ephemeral tunnel. Import it first with 'm'.".to_string());
                return Ok(());
            }

            let name = entry.tunnel.name.clone();
            let account_name = entry.tunnel.account_name.clone();
            let tunnel = entry.tunnel.clone();
            self.status_message = Some(format!("Restarting {}...", name));

            // Stop the daemon
            daemon::stop_daemon(&name, &account_name).await.ok();

            // Reinstall daemon (regenerates plist with latest config, including metrics)
            daemon::install_daemon(&tunnel).await?;

            // Start the daemon
            daemon::start_daemon(&name, &account_name).await?;

            // Update state
            let mut state = TunnelState::load()?;
            if let Some(t) = state.find_mut(&name) {
                t.enabled = true;
            }
            state.save()?;

            self.status_message = Some(format!("Restarted {}", name));
            self.load_tunnels().await?;
        }
        Ok(())
    }

    // Check if selected tunnel is ephemeral
    pub fn is_selected_ephemeral(&self) -> bool {
        self.tunnels
            .get(self.selected)
            .map(|e| e.kind == TunnelKind::Ephemeral)
            .unwrap_or(false)
    }

    // Copy the selected tunnel's URL to clipboard
    pub fn copy_url_to_clipboard(&mut self) {
        if let Some(entry) = self.tunnels.get(self.selected) {
            let url = format!("https://{}", entry.tunnel.hostname);
            self.status_message = Some(format!("Copying {}...", url));

            // Use pbcopy on macOS
            use std::io::Write;
            use std::process::{Command, Stdio};

            let result = Command::new("pbcopy")
                .stdin(Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    if let Some(mut stdin) = child.stdin.take() {
                        stdin.write_all(url.as_bytes())?;
                    }
                    child.wait()
                });

            match result {
                Ok(status) if status.success() => {
                    self.status_message = Some(format!("Copied: {}", url));
                }
                _ => {
                    self.status_message = Some("Failed to copy to clipboard".to_string());
                }
            }
        } else {
            self.status_message = Some("No tunnel selected".to_string());
        }
    }

    // Open the selected tunnel's URL in browser
    pub fn open_in_browser(&mut self) {
        if let Some(entry) = self.tunnels.get(self.selected) {
            let url = format!("https://{}", entry.tunnel.hostname);
            self.status_message = Some(format!("Opening {}...", url));

            // Use open command on macOS
            use std::process::Command;

            let result = Command::new("open").arg(&url).spawn();

            match result {
                Ok(_) => {
                    self.status_message = Some(format!("Opened: {}", url));
                }
                Err(_) => {
                    self.status_message = Some("Failed to open browser".to_string());
                }
            }
        } else {
            self.status_message = Some("No tunnel selected".to_string());
        }
    }

    // Toggle auto-start on login for the selected tunnel
    pub async fn toggle_auto_start(&mut self) -> Result<()> {
        if let Some(entry) = self.tunnels.get(self.selected) {
            if entry.kind == TunnelKind::Ephemeral {
                self.status_message = Some(
                    "Cannot set auto-start for ephemeral tunnel. Import it first.".to_string(),
                );
                return Ok(());
            }

            let name = entry.tunnel.name.clone();
            let new_auto_start = !entry.tunnel.auto_start;

            // Update state
            let mut state = TunnelState::load()?;
            if let Some(t) = state.find_mut(&name) {
                t.auto_start = new_auto_start;
            }
            state.save()?;

            // Reinstall daemon with new config
            let tunnel = state.find(&name).unwrap().clone();
            daemon::install_daemon(&tunnel).await?;

            let status = if new_auto_start { "ON" } else { "OFF" };
            self.status_message = Some(format!("Auto-start {}: {}", status, name));

            // Reload tunnels to reflect change
            self.load_tunnels().await?;
        }
        Ok(())
    }

    // Start import flow for ephemeral tunnel
    // Returns true if import was started (either directly or via dialog)
    pub async fn start_import(&mut self) -> Result<()> {
        if !self.is_selected_ephemeral() {
            self.status_message = Some("Only ephemeral tunnels can be imported".to_string());
            return Ok(());
        }
        if self.config.is_none() {
            self.status_message = Some("Run 'ytunnel init' first".to_string());
            return Ok(());
        }

        let entry = match self.tunnels.get(self.selected) {
            Some(e) => e.clone(),
            None => return Ok(()),
        };

        // Check if we have all the info needed for direct import
        let has_target = !entry.tunnel.target.is_empty() && entry.tunnel.target != "unknown";
        let has_zone = !entry.tunnel.zone_id.is_empty();

        if has_target && has_zone {
            // We have everything - import directly
            self.status_message = Some(format!("Importing {}...", entry.tunnel.name));
            self.direct_import(&entry.tunnel).await?;
        } else if has_target {
            // Have target but need zone - go to zone selection
            self.new_tunnel_name = Some(entry.tunnel.name.clone());
            self.new_tunnel_target = Some(entry.tunnel.target.clone());
            self.zone_selected = 0;
            self.is_importing = true;
            self.input_mode = InputMode::AddZone;
        } else {
            // Need target - go to target input
            self.new_tunnel_name = Some(entry.tunnel.name.clone());
            self.new_tunnel_target = None;
            self.zone_selected = 0;
            self.is_importing = true;
            self.input_mode = InputMode::AddTarget;
        }

        Ok(())
    }

    // Directly import an ephemeral tunnel without prompts
    async fn direct_import(&mut self, ephemeral: &PersistentTunnel) -> Result<()> {
        let acct = self
            .current_account()
            .ok_or_else(|| anyhow::anyhow!("No account selected"))?
            .clone();
        let client = cloudflare::Client::new(&acct.api_token);

        // Ensure DNS record exists
        client
            .ensure_dns_record(
                &ephemeral.zone_id,
                &ephemeral.hostname,
                &ephemeral.tunnel_id,
            )
            .await?;

        // Create the managed tunnel entry
        let persistent = PersistentTunnel {
            name: ephemeral.name.clone(),
            account_name: acct.name.clone(),
            target: ephemeral.target.clone(),
            zone_id: ephemeral.zone_id.clone(),
            zone_name: ephemeral.zone_name.clone(),
            hostname: ephemeral.hostname.clone(),
            tunnel_id: ephemeral.tunnel_id.clone(),
            enabled: true,
            auto_start: false,
            metrics_port: None,
        };

        // Write tunnel config for daemon
        write_tunnel_config(&persistent)?;

        // Install daemon
        daemon::install_daemon(&persistent).await?;

        // Save to state
        let mut state = TunnelState::load()?;
        state.add(persistent);
        state.save()?;

        // Note: Don't start daemon yet - the ephemeral tunnel is still running
        // User should stop the ephemeral one first (Ctrl+C) then start via TUI

        self.status_message = Some(format!(
            "Imported '{}'. Stop the running tunnel (Ctrl+C) then start here.",
            ephemeral.name
        ));

        // Reload tunnels
        self.load_tunnels().await?;

        // Select the imported tunnel
        if let Some(pos) = self
            .tunnels
            .iter()
            .position(|t| t.tunnel.name == ephemeral.name)
        {
            self.selected = pos;
            self.refresh_logs();
        }

        Ok(())
    }

    // Complete importing an ephemeral tunnel as managed
    pub async fn complete_import(&mut self) -> Result<()> {
        let name = self.new_tunnel_name.take().unwrap();
        let target = self.new_tunnel_target.take().unwrap();
        let zone = self.zones.get(self.zone_selected).unwrap().clone();

        // Find the ephemeral tunnel to get its tunnel_id
        let tunnel_id = self
            .tunnels
            .iter()
            .find(|e| e.tunnel.name == name && e.kind == TunnelKind::Ephemeral)
            .map(|e| e.tunnel.tunnel_id.clone())
            .ok_or_else(|| anyhow::anyhow!("Ephemeral tunnel not found"))?;

        let acct = self
            .current_account()
            .ok_or_else(|| anyhow::anyhow!("No account selected"))?
            .clone();
        let client = cloudflare::Client::new(&acct.api_token);

        let hostname = format!("{}.{}", name, zone.name);

        self.status_message = Some(format!("Importing tunnel {}...", name));

        // Ensure DNS record exists
        client
            .ensure_dns_record(&zone.id, &hostname, &tunnel_id)
            .await?;

        // Create persistent tunnel
        let persistent = PersistentTunnel {
            name: name.clone(),
            account_name: acct.name.clone(),
            target,
            zone_id: zone.id,
            zone_name: zone.name,
            hostname,
            tunnel_id,
            enabled: true,
            auto_start: false,
            metrics_port: None,
        };

        // Write tunnel config
        write_tunnel_config(&persistent)?;

        // Install daemon
        daemon::install_daemon(&persistent).await?;

        // Save to state
        let mut state = TunnelState::load()?;
        state.add(persistent);
        state.save()?;

        // Start the daemon
        daemon::start_daemon(&name, &acct.name).await?;

        self.input_mode = InputMode::Normal;
        self.status_message = Some(format!("Imported '{}' as managed tunnel", name));

        // Reload tunnels
        self.load_tunnels().await?;

        // Select the imported tunnel
        if let Some(pos) = self.tunnels.iter().position(|t| t.tunnel.name == name) {
            self.selected = pos;
            self.refresh_logs();
        }

        Ok(())
    }

    // Request deletion of selected tunnel
    pub fn request_delete(&mut self) {
        if let Some(entry) = self.tunnels.get(self.selected) {
            let msg = if entry.kind == TunnelKind::Ephemeral {
                format!(
                    "Delete ephemeral tunnel '{}'? This will remove it from Cloudflare. (y/n)",
                    entry.tunnel.name
                )
            } else {
                format!(
                    "Delete tunnel '{}'? This will remove the DNS record and tunnel. (y/n)",
                    entry.tunnel.name
                )
            };
            self.confirm_message = Some(msg);
            self.pending_action = Some(PendingAction::Delete(entry.tunnel.name.clone()));
            self.input_mode = InputMode::Confirm;
        }
    }

    // Execute confirmed delete
    pub async fn execute_delete(&mut self, name: String) -> Result<()> {
        self.status_message = Some(format!("Deleting {}...", name));

        // Find the entry to check if it's ephemeral
        let entry = self.tunnels.iter().find(|e| e.tunnel.name == name);
        let is_ephemeral = entry
            .map(|e| e.kind == TunnelKind::Ephemeral)
            .unwrap_or(false);
        let tunnel_id = entry.map(|e| e.tunnel.tunnel_id.clone());
        let account_name = entry
            .map(|e| e.tunnel.account_name.clone())
            .unwrap_or_else(|| self.current_account_name().to_string());

        if is_ephemeral {
            // Ephemeral tunnel: just delete from Cloudflare
            if let (Some(acct), Some(tid)) = (self.current_account(), tunnel_id) {
                let client = cloudflare::Client::new(&acct.api_token);
                client.delete_tunnel(&acct.account_id, &tid).await.ok();

                // Remove credentials file if it exists
                let config_dir = crate::config::config_dir()?;
                let creds_path = config_dir.join(format!("{}.json", tid));
                std::fs::remove_file(&creds_path).ok();
            }
        } else {
            // Managed tunnel: full cleanup
            // Stop daemon
            daemon::stop_daemon(&name, &account_name).await?;

            // Uninstall daemon
            daemon::uninstall_daemon(&name, &account_name).await?;

            // Remove from state and get tunnel info
            let mut state = TunnelState::load()?;
            if let Some(tunnel) = state.remove(&name) {
                // Delete from Cloudflare
                if let Some(acct) = self.current_account() {
                    let client = cloudflare::Client::new(&acct.api_token);
                    client
                        .delete_tunnel(&acct.account_id, &tunnel.tunnel_id)
                        .await
                        .ok();
                }

                // Remove credentials file
                if let Ok(creds_path) = tunnel.credentials_path() {
                    std::fs::remove_file(&creds_path).ok();
                }

                // Remove config file
                if let Ok(config_path) = tunnel.config_path() {
                    std::fs::remove_file(&config_path).ok();
                }

                // Remove log file
                if let Ok(log_path) = tunnel.log_path() {
                    std::fs::remove_file(&log_path).ok();
                }
            }
            state.save()?;
        }

        self.status_message = Some(format!("Deleted {}", name));
        self.load_tunnels().await?;

        Ok(())
    }
}

// Run the TUI application
pub async fn run_tui(initial_account: Option<&str>) -> Result<()> {
    // Check if ytunnel is initialized
    if !crate::config::config_path()?.exists() {
        anyhow::bail!(
            "ytunnel is not initialized.\n\n\
             Run `ytunnel init` to set up your Cloudflare API credentials."
        );
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app and load data
    let mut app = App::new(initial_account);
    if let Err(e) = app.load_tunnels().await {
        // Still show TUI even if load fails
        app.status_message = Some(format!("Error loading tunnels: {}", e));
    }

    // Initial health check for all running tunnels
    app.check_all_health().await;

    // Main loop
    let result = run_app(&mut terminal, &mut app).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()?;

    result
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    let mut last_metrics_refresh = std::time::Instant::now();
    let mut last_health_check = std::time::Instant::now();
    let metrics_refresh_interval = Duration::from_secs(5);
    let health_check_interval = Duration::from_secs(30);

    loop {
        terminal.draw(|f| ui::render(f, app))?;

        // Refresh metrics periodically
        if last_metrics_refresh.elapsed() >= metrics_refresh_interval {
            app.refresh_metrics().await;
            last_metrics_refresh = std::time::Instant::now();
        }

        // Check health of all running tunnels less frequently
        if last_health_check.elapsed() >= health_check_interval {
            app.check_all_health().await;
            last_health_check = std::time::Instant::now();
        }

        // Poll for events with timeout for async refresh
        if event::poll(Duration::from_millis(250))? {
            let event = event::read()?;

            // Handle paste events (some remote desktop software sends text as paste)
            if let Event::Paste(text) = &event {
                if matches!(app.input_mode, InputMode::AddName | InputMode::AddTarget) {
                    app.input.push_str(text);
                }
                continue;
            }

            if let Event::Key(key) = event {
                // Handle key press and repeat events (repeat needed for remote desktop)
                // Skip release events
                if key.kind == KeyEventKind::Release {
                    continue;
                }

                match app.input_mode {
                    InputMode::Normal => match key.code {
                        KeyCode::Char('q') => {
                            app.should_quit = true;
                        }
                        KeyCode::Char('a') => {
                            app.start_add();
                        }
                        KeyCode::Char('s') => {
                            if let Err(e) = app.start_selected().await {
                                app.status_message = Some(format!("Error: {}", e));
                            }
                        }
                        KeyCode::Char('S') => {
                            if let Err(e) = app.stop_selected().await {
                                app.status_message = Some(format!("Error: {}", e));
                            }
                        }
                        KeyCode::Char('d') => {
                            app.request_delete();
                        }
                        KeyCode::Char('m') => {
                            if let Err(e) = app.start_import().await {
                                app.status_message = Some(format!("Error: {}", e));
                            }
                        }
                        KeyCode::Char('r') => {
                            if let Err(e) = app.load_tunnels().await {
                                app.status_message = Some(format!("Error: {}", e));
                            } else {
                                app.status_message = Some("Refreshed".to_string());
                                // Check health of selected tunnel after refresh
                                if app.selected_needs_health_check() {
                                    app.check_health().await;
                                }
                            }
                        }
                        KeyCode::Char('R') => {
                            if let Err(e) = app.restart_selected().await {
                                app.status_message = Some(format!("Error: {}", e));
                            }
                        }
                        KeyCode::Char('c') => {
                            app.copy_url_to_clipboard();
                        }
                        KeyCode::Char('o') => {
                            app.open_in_browser();
                        }
                        KeyCode::Char('h') => {
                            app.check_health().await;
                        }
                        KeyCode::Char('A') => {
                            if let Err(e) = app.toggle_auto_start().await {
                                app.status_message = Some(format!("Error: {}", e));
                            }
                        }
                        KeyCode::Char('?') => {
                            app.input_mode = InputMode::Help;
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            if app.select_previous() && app.selected_needs_health_check() {
                                app.check_health().await;
                            }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if app.select_next() && app.selected_needs_health_check() {
                                app.check_health().await;
                            }
                        }
                        KeyCode::Char(';') => {
                            // Cycle to next account
                            if app.accounts.len() > 1 {
                                app.next_account();
                                if let Err(e) = app.load_tunnels().await {
                                    app.status_message = Some(format!("Error: {}", e));
                                }
                            }
                        }
                        _ => {}
                    },
                    InputMode::Help => match key.code {
                        KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Enter => {
                            app.input_mode = InputMode::Normal;
                        }
                        _ => {}
                    },
                    InputMode::AddName | InputMode::AddTarget => match key.code {
                        KeyCode::Esc => {
                            app.cancel_input();
                        }
                        KeyCode::Enter => {
                            app.next_add_step();
                        }
                        KeyCode::Backspace => {
                            app.input.pop();
                        }
                        KeyCode::Char(c) => {
                            app.input.push(c);
                        }
                        _ => {}
                    },
                    InputMode::AddZone => match key.code {
                        KeyCode::Esc => {
                            app.cancel_input();
                        }
                        KeyCode::Enter => {
                            let result = if app.is_importing {
                                app.complete_import().await
                            } else {
                                app.complete_add().await
                            };
                            if let Err(e) = result {
                                app.status_message = Some(format!("Error: {}", e));
                                app.input_mode = InputMode::Normal;
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            app.select_zone_prev();
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            app.select_zone_next();
                        }
                        _ => {}
                    },
                    InputMode::Confirm => match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => {
                            if let Some(PendingAction::Delete(name)) = app.pending_action.take() {
                                app.confirm_message = None;
                                app.input_mode = InputMode::Normal;
                                if let Err(e) = app.execute_delete(name).await {
                                    app.status_message = Some(format!("Error: {}", e));
                                }
                            }
                        }
                        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                            app.cancel_input();
                        }
                        _ => {}
                    },
                }
            }
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use rand::Rng;
use std::collections::{HashMap, HashSet};

use crate::cloudflare;
use crate::config;
use crate::config::Account;
use crate::daemon;
use crate::metrics::TunnelMetrics;
use crate::state::{write_tunnel_config, PersistentTunnel, TunnelState, TunnelStatus};

use super::ui;

// How long a Cloudflare tunnel listing stays good before we re-query. Tunnels
// created elsewhere don't appear often, and `r` forces a refresh anyway.
const DISCOVERY_TTL: Duration = Duration::from_secs(60);

// Turn an HTTP probe result into a health verdict
fn classify_health(result: reqwest::Result<reqwest::Response>) -> HealthStatus {
    match result {
        Ok(resp) if resp.status().is_success() || resp.status().is_redirection() => {
            HealthStatus::Healthy
        }
        Ok(resp) if resp.status().is_server_error() => HealthStatus::Unhealthy,
        Ok(_) => HealthStatus::Healthy, // 4xx is still "reachable"
        Err(_) => HealthStatus::Unhealthy,
    }
}

// Check if a key event is a cancel key (Esc or Ctrl+C)
fn is_cancel_key(key: &crossterm::event::KeyEvent) -> bool {
    if key.kind == KeyEventKind::Release {
        return false;
    }
    key.code == KeyCode::Esc
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}

// Standalone async operation: start a tunnel (doesn't borrow App)
async fn start_tunnel_op(
    name: String,
    account_name: String,
    tunnel: PersistentTunnel,
) -> Result<String> {
    // Ensure DNS record exists (recreates if manually deleted)
    let cfg = config::load_config()?;
    if let Some(acct) = cfg.accounts.iter().find(|a| a.name == account_name) {
        let client = cloudflare::Client::new(&acct.api_token);
        client
            .ensure_dns_record(&tunnel.zone_id, &tunnel.hostname, &tunnel.tunnel_id)
            .await?;
    }

    write_tunnel_config(&tunnel)?;
    daemon::install_daemon(&tunnel).await?;
    daemon::start_daemon(&name, &account_name).await?;

    let mut state = TunnelState::load()?;
    if let Some(t) = state.find_mut(&name) {
        t.enabled = true;
    }
    state.save()?;

    Ok(name)
}

// Standalone async operation: stop a tunnel
async fn stop_tunnel_op(name: String, account_name: String) -> Result<String> {
    daemon::stop_daemon(&name, &account_name).await?;

    let mut state = TunnelState::load()?;
    if let Some(t) = state.find_mut(&name) {
        t.enabled = false;
    }
    state.save()?;

    Ok(name)
}

// Standalone async operation: restart a tunnel
async fn restart_tunnel_op(
    name: String,
    account_name: String,
    tunnel: PersistentTunnel,
) -> Result<String> {
    daemon::stop_daemon(&name, &account_name).await.ok();

    // Ensure DNS record exists (recreates if manually deleted)
    let cfg = config::load_config()?;
    if let Some(acct) = cfg.accounts.iter().find(|a| a.name == account_name) {
        let client = cloudflare::Client::new(&acct.api_token);
        client
            .ensure_dns_record(&tunnel.zone_id, &tunnel.hostname, &tunnel.tunnel_id)
            .await?;
    }

    daemon::install_daemon(&tunnel).await?;
    daemon::start_daemon(&name, &account_name).await?;

    let mut state = TunnelState::load()?;
    if let Some(t) = state.find_mut(&name) {
        t.enabled = true;
    }
    state.save()?;

    Ok(name)
}

// Standalone async operation: create a new tunnel
async fn create_tunnel_op(
    name: String,
    target: String,
    zone: config::ZoneConfig,
    account: Account,
) -> Result<(String, PersistentTunnel)> {
    let client = cloudflare::Client::new(&account.api_token);

    let tunnel_name = format!("ytunnel-{}", name);
    let hostname = format!("{}.{}", name, zone.name);

    // Check if tunnel exists, create if not
    let (tunnel, _credentials_path) = match client
        .get_tunnel_by_name(&account.account_id, &tunnel_name)
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
            let result = client
                .create_tunnel(&account.account_id, &tunnel_name)
                .await?;
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
        account_name: account.name.clone(),
        target,
        zone_id: zone.id,
        zone_name: zone.name,
        hostname,
        tunnel_id: tunnel.id,
        enabled: true,
        auto_start: false,
        metrics_port: None,
        tunnel_options: std::collections::BTreeMap::new(),
        origin_request: std::collections::BTreeMap::new(),
        log_mode: crate::state::LogMode::Default,
    };

    // Write tunnel config
    write_tunnel_config(&persistent)?;

    // Install daemon
    daemon::install_daemon(&persistent).await?;

    // Save to state
    let mut state = TunnelState::load()?;
    state.add(persistent.clone());
    state.save()?;

    // Start the daemon
    daemon::start_daemon(&name, &account.name).await?;

    Ok((name, persistent))
}

// Standalone async operation: import an ephemeral tunnel
async fn import_tunnel_op(
    name: String,
    target: String,
    zone: config::ZoneConfig,
    tunnel_id: String,
    account: Account,
) -> Result<String> {
    let client = cloudflare::Client::new(&account.api_token);
    let hostname = format!("{}.{}", name, zone.name);

    // Ensure DNS record exists
    client
        .ensure_dns_record(&zone.id, &hostname, &tunnel_id)
        .await?;

    // Create persistent tunnel
    let persistent = PersistentTunnel {
        name: name.clone(),
        account_name: account.name.clone(),
        target,
        zone_id: zone.id,
        zone_name: zone.name,
        hostname,
        tunnel_id,
        enabled: true,
        auto_start: false,
        metrics_port: None,
        tunnel_options: std::collections::BTreeMap::new(),
        origin_request: std::collections::BTreeMap::new(),
        log_mode: crate::state::LogMode::Default,
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
    daemon::start_daemon(&name, &account.name).await?;

    Ok(name)
}

// Standalone async operation: delete a tunnel
async fn delete_tunnel_op(
    name: String,
    account_name: String,
    is_ephemeral: bool,
    tunnel_id: Option<String>,
    account: Option<Account>,
) -> Result<String> {
    if is_ephemeral {
        // Ephemeral tunnel: remove it from Cloudflare along with the CNAME that
        // `ytunnel run` created for it
        if let (Some(acct), Some(tid)) = (account, tunnel_id) {
            let client = cloudflare::Client::new(&acct.api_token);
            let cleanup = client.delete_tunnel_dns_records(&acct.zones, &tid).await;
            if !cleanup.errors.is_empty() {
                anyhow::bail!("{}", cleanup.errors.join("; "));
            }
            client.delete_tunnel(&acct.account_id, &tid).await.ok();

            // Remove credentials file if it exists
            let config_dir = crate::config::config_dir()?;
            let creds_path = config_dir.join(format!("{}.json", tid));
            std::fs::remove_file(&creds_path).ok();
        }
    } else {
        // Managed tunnel: full cleanup
        daemon::stop_daemon(&name, &account_name).await?;
        daemon::uninstall_daemon(&name, &account_name).await?;

        // Remove from state and get tunnel info
        let mut state = TunnelState::load()?;
        if let Some(tunnel) = state.remove(&name) {
            // Delete from Cloudflare
            if let Some(acct) = account {
                let client = cloudflare::Client::new(&acct.api_token);

                // Delete the DNS CNAME records, matched by what they point at
                // rather than a stored hostname that may be stale
                let cleanup = client
                    .delete_tunnel_dns_records(&acct.zones, &tunnel.tunnel_id)
                    .await;
                if !cleanup.errors.is_empty() {
                    anyhow::bail!("{}", cleanup.errors.join("; "));
                }

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

    Ok(name)
}

// Save the edit sheet state: apply to tunnel, persist to disk, reload daemon if running
// Route a mouse event to the log pane. Mouse capture is only enabled while
// focus is on Logs (see Tab handler), so any mouse event we receive here is
// implicitly for the log pane. Scroll wheel scrolls; clicks are ignored so
// they don't interfere with anything.
fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::ScrollUp => app.scroll_logs_up(3),
        MouseEventKind::ScrollDown => app.scroll_logs_down(3),
        _ => {}
    }
}

async fn save_edit_sheet(app: &mut App) {
    let Some(sheet) = app.edit_sheet.clone() else {
        return;
    };

    if !sheet.dirty {
        app.edit_sheet = None;
        app.input_mode = InputMode::Normal;
        return;
    }

    let tunnel_name = sheet.tunnel_name.clone();

    // Zone change: reconcile DNS records BEFORE writing new state to disk
    // (so if DNS fails we haven't persisted a broken hostname).
    if let Some(new_zone) = &sheet.pending_zone {
        let account = match app.current_account() {
            Some(a) => a.clone(),
            None => {
                app.status_message = Some("No active account for DNS reconciliation.".into());
                return;
            }
        };
        let client = crate::cloudflare::Client::new(&account.api_token);
        let new_hostname = format!("{}.{}", sheet.tunnel_name, new_zone.name);
        // Need tunnel_id — fetch from state.
        let state = match crate::state::TunnelState::load() {
            Ok(s) => s,
            Err(e) => {
                app.status_message = Some(format!("Failed to load tunnel state: {e}"));
                return;
            }
        };
        let tunnel_id = match state.find(&sheet.tunnel_name).map(|t| t.tunnel_id.clone()) {
            Some(id) => id,
            None => {
                app.status_message = Some("Tunnel not found in state.".into());
                return;
            }
        };
        // Clear every record pointing at this tunnel before pointing the new
        // hostname at it, so moving zones can't strand the old CNAME
        let cleanup = client
            .delete_tunnel_dns_records(&account.zones, &tunnel_id)
            .await;
        if !cleanup.errors.is_empty() {
            app.status_message = Some(format!(
                "Could not remove the old DNS record for {}: {}. Config not saved.",
                sheet.original_hostname,
                cleanup.errors.join("; ")
            ));
            return;
        }
        if let Err(e) = client
            .ensure_dns_record(&new_zone.id, &new_hostname, &tunnel_id)
            .await
        {
            app.status_message = Some(format!("Zone change failed (DNS): {e}. Config not saved."));
            return;
        }
    }

    // Load state, apply sheet, save
    let mut state = match crate::state::TunnelState::load() {
        Ok(s) => s,
        Err(e) => {
            app.status_message = Some(format!("Error: Failed to load tunnels.toml: {}", e));
            return;
        }
    };

    let cloned = {
        let Some(tunnel) = state.find_mut(&tunnel_name) else {
            app.status_message = Some(format!("Error: Tunnel '{}' not found", tunnel_name));
            return;
        };
        sheet.apply(tunnel);
        tunnel.clone()
    };

    if let Err(e) = state.save() {
        app.status_message = Some(format!("Error: Failed to save tunnels.toml: {}", e));
        return;
    }
    if let Err(e) = crate::state::write_tunnel_config(&cloned) {
        app.status_message = Some(format!("Error: Failed to write YAML: {}", e));
        return;
    }
    if let Err(e) = crate::daemon::reload_if_installed(&cloned).await {
        app.status_message = Some(format!("Config saved; daemon reload failed: {}", e));
    } else {
        app.status_message = Some(format!("Tunnel '{}' updated", tunnel_name));
    }
    app.edit_sheet = None;
    app.input_mode = InputMode::Normal;
}

fn commit_input_edit(app: &mut App) {
    let InputMode::EditSheetInput { yaml_key, buffer } = &app.input_mode else {
        return;
    };
    let yaml_key = yaml_key.clone();
    let buffer = buffer.clone();
    let Some(spec) = crate::cloudflared_options::find(&yaml_key) else {
        return;
    };
    use crate::cloudflared_options::OptionKind;
    use crate::state::TunnelOptionValue;

    let parsed = match &spec.kind {
        OptionKind::Int { .. } => match buffer.trim().parse::<i64>() {
            Ok(n) => Some(TunnelOptionValue::Int(n)),
            Err(_) => {
                app.status_message = Some(format!("Error: Invalid integer: {}", buffer));
                return;
            }
        },
        OptionKind::String { .. } | OptionKind::Duration { .. } => {
            if buffer.trim().is_empty() {
                None
            } else {
                Some(TunnelOptionValue::String(buffer.trim().to_string()))
            }
        }
        OptionKind::List { .. } => {
            let items: Vec<String> = buffer
                .lines()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if items.is_empty() {
                None
            } else {
                Some(TunnelOptionValue::List(items))
            }
        }
        _ => None,
    };

    if let Some(v) = &parsed {
        if let Err(e) = crate::cloudflared_options::validate_value(spec, v) {
            app.status_message = Some(format!("Error: Validation failed: {}", e));
            return;
        }
    }

    if let Some(sheet) = app.edit_sheet.as_mut() {
        if let Some(row) = sheet
            .advanced_rows
            .iter_mut()
            .find(|r| r.spec.yaml_key == yaml_key)
        {
            row.value = parsed;
            sheet.dirty = true;
        }
    }
}

fn commit_picker_edit(app: &mut App) {
    let InputMode::EditSheetPicker { yaml_key, cursor } = &app.input_mode else {
        return;
    };
    let yaml_key = yaml_key.clone();
    let cursor = *cursor;
    let Some(spec) = crate::cloudflared_options::find(&yaml_key) else {
        return;
    };
    let crate::cloudflared_options::OptionKind::Enum { choices, .. } = &spec.kind else {
        return;
    };
    let choice = choices[cursor].to_string();

    if let Some(sheet) = app.edit_sheet.as_mut() {
        if let Some(row) = sheet
            .advanced_rows
            .iter_mut()
            .find(|r| r.spec.yaml_key == yaml_key)
        {
            row.value = if choice.is_empty() {
                None
            } else {
                Some(crate::state::TunnelOptionValue::String(choice))
            };
            sheet.dirty = true;
        }
    }
}

// Write the Basic-tab input buffer back to the correct field on the sheet.
// On success, sets sheet.dirty = true and transitions to InputMode::EditSheet.
// On MetricsPort parse failure, sets app.status_message and leaves input_mode
// unchanged so the user can correct the value.
fn commit_basic_input_edit(app: &mut App) {
    let InputMode::EditSheetBasicInput { field, buffer } = &app.input_mode else {
        return;
    };
    let field = *field;
    let buffer = buffer.trim().to_string();

    use crate::tui::edit_sheet::BasicField;

    if field == BasicField::MetricsPort && !buffer.is_empty() {
        match buffer.parse::<u16>() {
            Ok(port) => {
                if let Some(sheet) = app.edit_sheet.as_mut() {
                    sheet.metrics_port = Some(port);
                    sheet.dirty = true;
                }
                app.input_mode = InputMode::EditSheet;
            }
            Err(_) => {
                app.status_message = Some(format!("Invalid metrics port: {}", buffer));
                // Leave input_mode unchanged so the user can fix the value.
            }
        }
        return;
    }

    if let Some(sheet) = app.edit_sheet.as_mut() {
        match field {
            BasicField::Target => {
                sheet.target = buffer;
                sheet.dirty = true;
            }
            BasicField::MetricsPort => {
                // Empty buffer → clear the port.
                sheet.metrics_port = None;
                sheet.dirty = true;
            }
        }
    }
    app.input_mode = InputMode::EditSheet;
}

fn log_mode_to_index(m: crate::state::LogMode) -> usize {
    match m {
        crate::state::LogMode::Default => 0,
        crate::state::LogMode::Debug => 1,
        crate::state::LogMode::NgrokDev => 2,
    }
}

fn log_mode_from_index(i: usize) -> crate::state::LogMode {
    match i {
        1 => crate::state::LogMode::Debug,
        2 => crate::state::LogMode::NgrokDev,
        _ => crate::state::LogMode::Default,
    }
}

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

// Message for an action attempted on a tunnel ytunnel doesn't own. `action` is
// a bare verb, e.g. "start" or "edit".
fn unmanaged_message(entry: &TunnelEntry, action: &str) -> String {
    match entry.kind {
        TunnelKind::Ephemeral => format!(
            "Cannot {} an ephemeral tunnel. Import it first with [m].",
            action
        ),
        TunnelKind::External => format!(
            "Cannot {} '{}' - it isn't managed by ytunnel.",
            action, entry.tunnel.name
        ),
        TunnelKind::Managed => String::new(),
    }
}

// Build the detail pane text for a tunnel ytunnel didn't create. There are no
// logs to read because there's no local cloudflared process behind it.
fn external_detail(entry: &TunnelEntry) -> Vec<String> {
    vec![
        "Not managed by ytunnel".to_string(),
        String::new(),
        format!("Name:      {}", entry.tunnel.name),
        format!("Tunnel ID: {}", entry.tunnel.tunnel_id),
        format!(
            "Status:    {} (reported by Cloudflare)",
            entry.cf_status.as_deref().unwrap_or("unknown")
        ),
        String::new(),
        "This tunnel exists on your Cloudflare account but wasn't created by".to_string(),
        "ytunnel, so there's no local config, credentials, or daemon for it.".to_string(),
        "Its cloudflared may be running on a different machine.".to_string(),
        String::new(),
        "ytunnel can't start, stop, edit, or delete it. Manage it from the".to_string(),
        "Cloudflare dashboard or with cloudflared on the machine running it.".to_string(),
    ]
}

// Which pane currently has keyboard focus in Normal mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tunnels,
    Logs,
}

// Input mode for the TUI
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    AddName,
    AddTarget,
    AddZone,
    EditSheet,
    EditSheetInput {
        yaml_key: String,
        buffer: String,
    },
    EditSheetBasicInput {
        field: crate::tui::edit_sheet::BasicField,
        buffer: String,
    },
    EditSheetPicker {
        yaml_key: String,
        cursor: usize,
    },
    EditSheetZonePicker {
        cursor: usize,
    },
    EditSheetLogModePicker {
        cursor: usize,
    },
    EditSheetConfirmDiscard,
    Confirm,
    Help,
}

// Whether a tunnel is managed (persistent), ephemeral, or someone else's
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelKind {
    // Managed tunnel with launchd daemon
    Managed,
    // Ephemeral tunnel (created with `ytunnel run`, not in state)
    Ephemeral,
    // Exists on the Cloudflare account but wasn't created by ytunnel. Its
    // cloudflared may be running on a different machine entirely, so the only
    // thing we can do is show it.
    External,
}

impl TunnelKind {
    // Whether ytunnel owns this tunnel's lifecycle (daemon, config, DNS)
    pub fn is_managed(&self) -> bool {
        *self == TunnelKind::Managed
    }
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

// Animated spinner for async operations
#[derive(Debug, Clone, Default)]
pub struct Spinner {
    // Braille dots animation frames
    frames: Vec<char>,
    // Current frame index
    frame_idx: usize,
    // Message to display with spinner
    pub message: Option<String>,
}

impl Spinner {
    pub fn new() -> Self {
        Self {
            // Classic braille dots spinner
            frames: vec!['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'],
            frame_idx: 0,
            message: None,
        }
    }

    // Start the spinner with a message
    pub fn start(&mut self, message: &str) {
        self.message = Some(message.to_string());
        self.frame_idx = 0;
    }

    // Stop the spinner
    pub fn stop(&mut self) {
        self.message = None;
    }

    // Check if spinner is active
    pub fn is_active(&self) -> bool {
        self.message.is_some()
    }

    // Advance to next frame
    pub fn tick(&mut self) {
        if self.is_active() {
            self.frame_idx = (self.frame_idx + 1) % self.frames.len();
        }
    }

    // Get current display string (spinner char + message)
    pub fn display(&self) -> Option<String> {
        self.message
            .as_ref()
            .map(|msg| format!("{} {}", self.frames[self.frame_idx], msg))
    }
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
    // Cloudflare's reported status, only set for External tunnels where it's
    // the only thing we know about them
    pub cf_status: Option<String>,
}

// Application state
pub struct App {
    // Current input mode
    pub input_mode: InputMode,
    // List of tunnels with status
    pub tunnels: Vec<TunnelEntry>,
    // Currently selected tunnel index
    pub selected: usize,
    // Per-tunnel byte-offset log tails (keyed by tunnel name)
    pub log_tails: HashMap<String, crate::tui::log_tail::LogTail>,
    // Scroll offset from bottom of log buffer. 0 = follow bottom.
    pub log_scroll: u16,
    // True while log_scroll == 0 (auto-scroll to bottom on new lines).
    pub log_follow: bool,
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
    // Name of tunnel being edited (for edit flow)
    pub editing_tunnel_name: Option<String>,
    // Original zone ID (for DNS cleanup if zone changes during edit)
    pub original_zone_id: Option<String>,
    // Original hostname (for DNS cleanup if zone changes during edit)
    pub original_hostname: Option<String>,
    // Spinner for async operations
    pub spinner: Spinner,
    // Demo mode flag (synthetic data, no real API calls)
    pub demo: bool,
    // What Cloudflare last told us was on each account, keyed by account name.
    // Cached so a reload never waits on the network to draw the local tunnels.
    discovered: HashMap<String, Vec<cloudflare::Tunnel>>,
    // When each account's discovery last completed
    discovery_at: HashMap<String, Instant>,
    // Accounts with a discovery request currently running
    discovery_in_flight: HashSet<String>,
    // Background discovery results arrive here
    discovery_tx: mpsc::UnboundedSender<DiscoveryResult>,
    discovery_rx: mpsc::UnboundedReceiver<DiscoveryResult>,
    // State for the two-tab edit sheet overlay
    pub edit_sheet: Option<crate::tui::edit_sheet::EditSheetState>,
    // Which pane has keyboard focus in Normal mode.
    pub focus: Focus,
}

// Outcome of one background query to Cloudflare for an account's tunnels
struct DiscoveryResult {
    account_name: String,
    tunnels: Result<Vec<cloudflare::Tunnel>>,
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

        let (discovery_tx, discovery_rx) = mpsc::unbounded_channel();

        Self {
            input_mode: InputMode::Normal,
            tunnels: Vec::new(),
            selected: 0,
            log_tails: HashMap::new(),
            log_scroll: 0,
            log_follow: true,
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
            editing_tunnel_name: None,
            original_zone_id: None,
            original_hostname: None,
            spinner: Spinner::new(),
            demo: false,
            discovered: HashMap::new(),
            discovery_at: HashMap::new(),
            discovery_in_flight: HashSet::new(),
            discovery_tx,
            discovery_rx,
            edit_sheet: None,
            focus: Focus::Tunnels,
        }
    }

    // Create an App pre-loaded with synthetic demo data
    pub fn new_demo() -> Self {
        let demo_account = Account {
            name: "demo".to_string(),
            api_token: String::new(),
            account_id: String::new(),
            default_zone_id: "zone-1".to_string(),
            default_zone_name: "example.com".to_string(),
            zones: vec![
                config::ZoneConfig {
                    id: "zone-1".to_string(),
                    name: "example.com".to_string(),
                },
                config::ZoneConfig {
                    id: "zone-2".to_string(),
                    name: "dev.example.com".to_string(),
                },
            ],
        };

        let (discovery_tx, discovery_rx) = mpsc::unbounded_channel();

        Self {
            input_mode: InputMode::Normal,
            tunnels: Vec::new(),
            selected: 0,
            log_tails: HashMap::new(),
            log_scroll: 0,
            log_follow: true,
            input: String::new(),
            new_tunnel_name: None,
            new_tunnel_target: None,
            zones: demo_account.zones.clone(),
            zone_selected: 0,
            confirm_message: None,
            pending_action: None,
            status_message: None,
            should_quit: false,
            config: None,
            is_importing: false,
            accounts: vec![demo_account],
            selected_account_idx: 0,
            editing_tunnel_name: None,
            original_zone_id: None,
            original_hostname: None,
            spinner: Spinner::new(),
            demo: true,
            discovered: HashMap::new(),
            discovery_at: HashMap::new(),
            discovery_in_flight: HashSet::new(),
            discovery_tx,
            discovery_rx,
            edit_sheet: None,
            focus: Focus::Tunnels,
        }
    }

    // Load synthetic tunnels for demo mode
    pub fn load_demo_tunnels(&mut self) {
        let tunnels_spec: Vec<(
            &str,
            TunnelStatus,
            TunnelKind,
            HealthStatus,
            bool,
            &str,
            &str,
        )> = vec![
            (
                "webapp",
                TunnelStatus::Running,
                TunnelKind::Managed,
                HealthStatus::Healthy,
                true,
                "localhost:3000",
                "example.com",
            ),
            (
                "api",
                TunnelStatus::Running,
                TunnelKind::Managed,
                HealthStatus::Healthy,
                true,
                "localhost:8080",
                "example.com",
            ),
            (
                "staging",
                TunnelStatus::Running,
                TunnelKind::Managed,
                HealthStatus::Unhealthy,
                false,
                "localhost:3001",
                "dev.example.com",
            ),
            (
                "docs",
                TunnelStatus::Running,
                TunnelKind::Managed,
                HealthStatus::Healthy,
                false,
                "localhost:4000",
                "example.com",
            ),
            (
                "backend",
                TunnelStatus::Stopped,
                TunnelKind::Managed,
                HealthStatus::Unknown,
                false,
                "localhost:9000",
                "example.com",
            ),
            (
                "preview-k8f2x",
                TunnelStatus::Running,
                TunnelKind::Ephemeral,
                HealthStatus::Unknown,
                false,
                "localhost:5173",
                "dev.example.com",
            ),
            (
                "homeserver",
                TunnelStatus::Running,
                TunnelKind::External,
                HealthStatus::Unknown,
                false,
                "",
                "example.com",
            ),
        ];

        let mut rng = rand::rng();

        for (name, status, kind, health, auto_start, target, zone_name) in tunnels_spec {
            // We don't know an external tunnel's hostname, it lives in a config
            // file on whatever machine is actually running cloudflared
            let hostname = if kind == TunnelKind::External {
                String::new()
            } else {
                format!("{}.{}", name, zone_name)
            };
            let tunnel = PersistentTunnel {
                name: name.to_string(),
                account_name: "demo".to_string(),
                target: target.to_string(),
                zone_id: if zone_name == "example.com" {
                    "zone-1"
                } else {
                    "zone-2"
                }
                .to_string(),
                zone_name: zone_name.to_string(),
                hostname,
                tunnel_id: format!("demo-{}", name),
                enabled: status == TunnelStatus::Running,
                auto_start,
                metrics_port: None,
                tunnel_options: std::collections::BTreeMap::new(),
                origin_request: std::collections::BTreeMap::new(),
                log_mode: crate::state::LogMode::Default,
            };

            // Build pre-seeded metrics for running managed tunnels
            let (metrics, metrics_history) =
                if status == TunnelStatus::Running && kind == TunnelKind::Managed {
                    let (total, errors, concurrent, ha, codes, locations, samples) = match name {
                        "webapp" => (
                            12847u64,
                            23u64,
                            8u64,
                            4u64,
                            vec![(200u16, 11500u64), (301, 420), (404, 85), (500, 23)],
                            vec!["dfw08", "den01", "iad02", "lax01"],
                            vec![
                                45, 52, 38, 61, 55, 48, 72, 65, 43, 58, 51, 67, 44, 53, 60, 47, 56,
                                42, 63, 50,
                            ],
                        ),
                        "api" => (
                            5432,
                            7,
                            3,
                            4,
                            vec![(200, 4800), (201, 320), (400, 52), (404, 30), (500, 7)],
                            vec!["dfw08", "den01", "iad02", "lax01"],
                            vec![
                                20, 18, 25, 22, 15, 28, 19, 24, 17, 21, 26, 14, 23, 20, 27, 16, 22,
                                25, 18, 24,
                            ],
                        ),
                        "staging" => (
                            342,
                            41,
                            1,
                            2,
                            vec![(200, 280), (404, 15), (502, 35), (503, 6)],
                            vec!["dfw08", "den01"],
                            vec![3, 5, 2, 4, 1, 6, 2, 3, 5, 1, 4, 2, 3, 5, 2, 4, 1, 6, 3, 2],
                        ),
                        "docs" => (
                            89,
                            0,
                            0,
                            4,
                            vec![(200, 82), (304, 7)],
                            vec!["dfw08", "den01", "iad02", "lax01"],
                            vec![1, 0, 2, 1, 0, 1, 0, 0, 1, 2, 0, 1, 0, 1, 0, 0, 1, 0, 2, 1],
                        ),
                        _ => (0, 0, 0, 0, vec![], vec![], vec![]),
                    };

                    let mut response_codes = HashMap::new();
                    for (code, count) in codes {
                        response_codes.insert(code, count);
                    }

                    let m = TunnelMetrics {
                        total_requests: total,
                        request_errors: errors,
                        concurrent_requests: concurrent,
                        ha_connections: ha,
                        response_codes,
                        edge_locations: locations.into_iter().map(|s| s.to_string()).collect(),
                        available: true,
                    };

                    let mut history = MetricsHistory {
                        last_total: total.saturating_sub(samples.iter().sum::<u64>()),
                        ..Default::default()
                    };
                    for &s in &samples {
                        // Manually push samples (not using record() to avoid delta math on seeded data)
                        history.request_samples.push(s + rng.random_range(0u64..3));
                    }

                    (Some(m), history)
                } else {
                    (None, MetricsHistory::default())
                };

            self.tunnels.push(TunnelEntry {
                tunnel,
                status,
                kind,
                metrics,
                metrics_history,
                health,
                cf_status: if kind == TunnelKind::External {
                    Some("healthy".to_string())
                } else {
                    None
                },
            });
        }

        self.refresh_demo_logs();
    }

    // Generate cloudflared-style log lines for demo tunnels (writes into log_tails)
    fn refresh_demo_logs(&mut self) {
        let lines: Vec<String> = if let Some(entry) = self.tunnels.get(self.selected) {
            match entry.kind {
                TunnelKind::Ephemeral => vec![
                    "Ephemeral tunnel (created with `ytunnel run`)".to_string(),
                    String::new(),
                    format!("Hostname: {}", entry.tunnel.hostname),
                    format!("Target:   {}", entry.tunnel.target),
                    format!("Zone:     {}", entry.tunnel.zone_name),
                    String::new(),
                    "[demo mode] Press [m] to import as managed tunnel".to_string(),
                    "[demo mode] Press [d] to delete from Cloudflare".to_string(),
                ],
                TunnelKind::External => external_detail(entry),
                TunnelKind::Managed => match entry.status {
                    TunnelStatus::Running => {
                        let name = entry.tunnel.name.clone();
                        let hostname = entry.tunnel.hostname.clone();
                        let target = entry.tunnel.target.clone();
                        vec![
                            format!("INF Starting tunnel tunnelID=demo-{}", name),
                            "INF Version 2024.12.2".to_string(),
                            format!("INF ICMP proxy will use {}:0 as source for both IPv4 and IPv6", target.split(':').next().unwrap_or("localhost")),
                            "INF Starting metrics server on 127.0.0.1:20241".to_string(),
                            "INF Registered tunnel connection connIndex=0 connection=demo-conn-0 event=0 ip=198.41.192.77 location=dfw08 protocol=quic".to_string(),
                            "INF Registered tunnel connection connIndex=1 connection=demo-conn-1 event=0 ip=198.41.200.33 location=den01 protocol=quic".to_string(),
                            "INF Registered tunnel connection connIndex=2 connection=demo-conn-2 event=0 ip=198.41.200.13 location=iad02 protocol=quic".to_string(),
                            "INF Registered tunnel connection connIndex=3 connection=demo-conn-3 event=0 ip=198.41.192.47 location=lax01 protocol=quic".to_string(),
                            format!("INF Updated tunnel route dns={} tunnelID=demo-{}", hostname, name),
                            "INF Connection established connIndex=0 connection=demo-conn-0 location=dfw08".to_string(),
                            String::new(),
                            format!("INF GET {} 200 12ms", hostname),
                            format!("INF GET {} 200 8ms", hostname),
                            format!("INF POST {}/api/data 201 45ms", hostname),
                            format!("INF GET {} 200 6ms", hostname),
                        ]
                    }
                    TunnelStatus::Stopped => {
                        let name = entry.tunnel.name.clone();
                        vec![
                            format!("INF Starting tunnel tunnelID=demo-{}", name),
                            "INF Registered tunnel connection connIndex=0 location=dfw08"
                                .to_string(),
                            "INF Registered tunnel connection connIndex=1 location=den01"
                                .to_string(),
                            String::new(),
                            "INF Initiating graceful shutdown due to signal".to_string(),
                            "INF Quitting...".to_string(),
                            "INF Unregistered tunnel connection connIndex=1".to_string(),
                            "INF Unregistered tunnel connection connIndex=0".to_string(),
                        ]
                    }
                    TunnelStatus::Error => vec![
                        "ERR Unable to establish connection".to_string(),
                        "ERR Retrying in 5s...".to_string(),
                    ],
                },
            }
        } else {
            return;
        };

        // Inject demo lines into the log_tails map so render_logs can read them.
        if let Some(entry) = self.tunnels.get(self.selected) {
            let name = entry.tunnel.name.clone();
            // Use a dummy path; seed() won't be called for demo tails.
            let tail = self.log_tails.entry(name).or_insert_with(|| {
                crate::tui::log_tail::LogTail::new(std::path::PathBuf::new(), 5000)
            });
            tail.inject_lines(lines);
        }
    }

    // Randomly increment demo metrics for sparkline animation
    fn refresh_demo_metrics(&mut self) {
        let mut rng = rand::rng();

        for entry in &mut self.tunnels {
            if entry.kind != TunnelKind::Managed || entry.status != TunnelStatus::Running {
                continue;
            }

            if let Some(ref mut m) = entry.metrics {
                // Increment based on tunnel traffic profile
                let (req_delta, err_chance) = match entry.tunnel.name.as_str() {
                    "webapp" => (rng.random_range(30u64..80), 0.02),
                    "api" => (rng.random_range(10u64..35), 0.01),
                    "staging" => (rng.random_range(1u64..8), 0.15),
                    "docs" => (rng.random_range(0u64..3), 0.0),
                    _ => (0, 0.0),
                };

                m.total_requests += req_delta;
                if rng.random_range(0.0f64..1.0) < err_chance {
                    m.request_errors += 1;
                }
                m.concurrent_requests = rng.random_range(0u64..=req_delta.min(12));

                // Record to history for sparkline
                entry.metrics_history.request_samples.push(req_delta);
                if entry.metrics_history.request_samples.len() > MetricsHistory::MAX_SAMPLES {
                    entry.metrics_history.request_samples.remove(0);
                }
                entry.metrics_history.last_total = m.total_requests;
            }
        }
    }

    // Guard for demo mode: returns true (and sets status) if in demo mode
    fn demo_guard(&mut self) -> bool {
        if self.demo {
            self.status_message = Some("Demo mode — actions disabled".to_string());
            true
        } else {
            false
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

    // Load tunnels and their statuses from local state. Cloudflare discovery
    // runs in the background, so this never blocks on the network.
    pub async fn load_tunnels(&mut self) -> Result<()> {
        self.load_tunnels_inner(false).await
    }

    // Same, but always re-query Cloudflare even if the cache is still fresh
    pub async fn reload_tunnels(&mut self) -> Result<()> {
        self.load_tunnels_inner(true).await
    }

    async fn load_tunnels_inner(&mut self, force: bool) -> Result<()> {
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
        let managed_ids: std::collections::HashSet<String> = managed_tunnels
            .iter()
            .map(|t| t.tunnel_id.clone())
            .collect();

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
                cf_status: None,
            });
        }

        // Append whatever Cloudflare last told us about, from cache. The API
        // call itself runs in the background so it never delays this render.
        entries.extend(self.derive_discovered(&current_account_name, &managed_names, &managed_ids));

        self.tunnels = entries;

        // Ensure selected index is valid
        if self.selected >= self.tunnels.len() && !self.tunnels.is_empty() {
            self.selected = self.tunnels.len() - 1;
        }

        // Load logs for selected tunnel
        self.refresh_logs();

        self.spawn_discovery(force);

        Ok(())
    }

    // Turn cached Cloudflare results into entries for tunnels we aren't already
    // showing: ephemeral ones (ytunnel-* with no state) and everything else,
    // which ytunnel didn't create at all
    fn derive_discovered(
        &self,
        account_name: &str,
        managed_names: &HashSet<String>,
        managed_ids: &HashSet<String>,
    ) -> Vec<TunnelEntry> {
        let Some(cf_tunnels) = self.discovered.get(account_name) else {
            return Vec::new();
        };
        let zones = self
            .current_account()
            .map(|a| a.zones.clone())
            .unwrap_or_default();

        let mut entries = Vec::new();
        for cf_tunnel in cf_tunnels {
            // Skip deleted tunnels and anything already shown as managed
            if cf_tunnel.deleted_at.is_some() || managed_ids.contains(&cf_tunnel.id) {
                continue;
            }

            // Anything without the ytunnel- prefix was created outside of
            // ytunnel, so we have no local config, credentials, or daemon for
            // it. Show it read-only rather than pretending it isn't there.
            let Some(short_name) = cf_tunnel.name.strip_prefix("ytunnel-") else {
                entries.push(TunnelEntry {
                    tunnel: PersistentTunnel {
                        name: cf_tunnel.name.clone(),
                        account_name: account_name.to_string(),
                        target: String::new(),
                        zone_id: String::new(),
                        zone_name: String::new(),
                        hostname: String::new(),
                        tunnel_id: cf_tunnel.id.clone(),
                        enabled: false,
                        auto_start: false,
                        metrics_port: None,
                        tunnel_options: std::collections::BTreeMap::new(),
                        origin_request: std::collections::BTreeMap::new(),
                        log_mode: crate::state::LogMode::Default,
                    },
                    // Cloudflare's own view is the only status we have
                    status: if cf_tunnel.is_connected() {
                        TunnelStatus::Running
                    } else {
                        TunnelStatus::Stopped
                    },
                    kind: TunnelKind::External,
                    metrics: None,
                    metrics_history: MetricsHistory::default(),
                    health: HealthStatus::Unknown,
                    cf_status: Some(cf_tunnel.status_label().to_string()),
                });
                continue;
            };

            // Skip if already managed under its short name
            if managed_names.contains(short_name) {
                continue;
            }

            // This is an ephemeral tunnel - try to read its config file
            let (hostname, target) = parse_ephemeral_config(&cf_tunnel.id)
                .unwrap_or_else(|| (short_name.to_string(), "unknown".to_string()));

            // Try to determine the zone from the hostname
            let (zone_id, zone_name) = if hostname.contains('.') {
                zones
                    .iter()
                    .find(|z| hostname.ends_with(&z.name))
                    .map(|z| (z.id.clone(), z.name.clone()))
                    .unwrap_or_default()
            } else {
                (String::new(), String::new())
            };

            // Check if config file exists (means tunnel is actively running)
            let config_exists = crate::config::config_dir()
                .ok()
                .map(|d| d.join(format!("tunnel-{}.yml", cf_tunnel.id)).exists())
                .unwrap_or(false);

            entries.push(TunnelEntry {
                tunnel: PersistentTunnel {
                    name: short_name.to_string(),
                    account_name: account_name.to_string(),
                    target,
                    zone_id,
                    zone_name,
                    hostname,
                    tunnel_id: cf_tunnel.id.clone(),
                    enabled: false,
                    auto_start: false,
                    metrics_port: None,
                    tunnel_options: std::collections::BTreeMap::new(),
                    origin_request: std::collections::BTreeMap::new(),
                    log_mode: crate::state::LogMode::Default,
                },
                status: if config_exists {
                    TunnelStatus::Running
                } else {
                    TunnelStatus::Stopped
                },
                kind: TunnelKind::Ephemeral,
                metrics: None,
                metrics_history: MetricsHistory::default(),
                health: HealthStatus::Unknown,
                cf_status: None,
            });
        }

        entries
    }

    // Kick off a background query to Cloudflare for the current account unless
    // one is already running or the cached result is still fresh
    fn spawn_discovery(&mut self, force: bool) {
        if self.demo {
            return;
        }

        let Some(acct) = self.current_account().cloned() else {
            return;
        };

        if self.discovery_in_flight.contains(&acct.name) {
            return;
        }

        if !force {
            let fresh = self
                .discovery_at
                .get(&acct.name)
                .map(|t| t.elapsed() < DISCOVERY_TTL)
                .unwrap_or(false);
            if fresh {
                return;
            }
        }

        self.discovery_in_flight.insert(acct.name.clone());
        let tx = self.discovery_tx.clone();

        tokio::spawn(async move {
            let client = cloudflare::Client::new(&acct.api_token);
            let tunnels = client.list_tunnels(&acct.account_id).await;
            // A send failure just means the TUI exited first
            tx.send(DiscoveryResult {
                account_name: acct.name,
                tunnels,
            })
            .ok();
        });
    }

    // Drain any finished background discoveries. Returns true if the tunnel
    // list changed and the UI should redraw.
    pub fn poll_discovery(&mut self) -> bool {
        let mut changed = false;

        while let Ok(result) = self.discovery_rx.try_recv() {
            self.discovery_in_flight.remove(&result.account_name);

            match result.tunnels {
                Ok(tunnels) => {
                    self.discovery_at
                        .insert(result.account_name.clone(), Instant::now());
                    self.discovered.insert(result.account_name, tunnels);
                    changed = true;
                }
                Err(e) => {
                    // Keep any previous result rather than blanking the list
                    self.status_message =
                        Some(format!("Could not reach Cloudflare to list tunnels: {}", e));
                }
            }
        }

        if changed {
            self.rebuild_discovered_entries();
        }

        changed
    }

    // Swap the non-managed entries for freshly derived ones, leaving managed
    // tunnels (and their metrics history) untouched
    fn rebuild_discovered_entries(&mut self) {
        let account_name = self.current_account_name().to_string();
        let selected_name = self
            .tunnels
            .get(self.selected)
            .map(|e| e.tunnel.name.clone());

        self.tunnels.retain(|e| e.kind.is_managed());

        let managed_names: HashSet<String> =
            self.tunnels.iter().map(|e| e.tunnel.name.clone()).collect();
        let managed_ids: HashSet<String> = self
            .tunnels
            .iter()
            .map(|e| e.tunnel.tunnel_id.clone())
            .collect();

        let discovered = self.derive_discovered(&account_name, &managed_names, &managed_ids);
        self.tunnels.extend(discovered);

        // Keep the cursor on whatever the user had selected
        if let Some(name) = selected_name {
            if let Some(pos) = self.tunnels.iter().position(|e| e.tunnel.name == name) {
                self.selected = pos;
            }
        }
        if self.selected >= self.tunnels.len() && !self.tunnels.is_empty() {
            self.selected = self.tunnels.len() - 1;
        }

        self.refresh_logs();
    }

    // Refresh logs for the selected tunnel
    pub fn refresh_logs(&mut self) {
        if self.demo {
            self.refresh_demo_logs();
            return;
        }
        let Some(entry) = self.tunnels.get(self.selected) else {
            return;
        };
        let name = entry.tunnel.name.clone();
        // Ephemeral and external tunnels have no log file; show their details
        // in the panel instead, through a placeholder tail like demo mode does
        let info = match entry.kind {
            TunnelKind::Managed => None,
            TunnelKind::Ephemeral => {
                let has_config =
                    entry.tunnel.target != "unknown" && !entry.tunnel.target.is_empty();
                Some(if has_config {
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
                })
            }
            TunnelKind::External => Some(external_detail(entry)),
        };
        if let Some(lines) = info {
            let tail = self.log_tails.entry(name).or_insert_with(|| {
                crate::tui::log_tail::LogTail::new(std::path::PathBuf::new(), 5000)
            });
            tail.inject_lines(lines);
            return;
        }
        let path = match entry.tunnel.log_path() {
            Ok(p) => p,
            Err(_) => return,
        };
        match self.log_tails.entry(name) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                let _ = e.get_mut().poll();
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                let mut tail = crate::tui::log_tail::LogTail::new(path, 5000);
                let _ = tail.seed();
                e.insert(tail);
            }
        }
    }

    // Refresh metrics for the selected tunnel
    pub async fn refresh_metrics(&mut self) {
        if self.demo {
            self.refresh_demo_metrics();
            return;
        }
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
        if self.demo {
            return;
        }
        self.check_health_for_index(self.selected).await;
    }

    // Check health of all running tunnels. Probes run concurrently: done one at
    // a time this cost a round trip per tunnel and froze the UI for the total.
    pub async fn check_all_health(&mut self) {
        if self.demo {
            return;
        }

        // (index, hostname, health before the check)
        let targets: Vec<(usize, String, HealthStatus)> = self
            .tunnels
            .iter()
            .enumerate()
            .filter(|(_, e)| e.status == TunnelStatus::Running && !e.tunnel.hostname.is_empty())
            .map(|(i, e)| (i, e.tunnel.hostname.clone(), e.health))
            .collect();

        if targets.is_empty() {
            return;
        }

        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .danger_accept_invalid_certs(true) // In case of self-signed certs
            .build()
        {
            Ok(c) => c,
            Err(_) => return,
        };

        let mut handles = Vec::new();
        for (index, hostname, _) in &targets {
            let client = client.clone();
            let url = format!("https://{}", hostname);
            let index = *index;
            handles.push(tokio::spawn(async move {
                (index, classify_health(client.head(&url).send().await))
            }));
        }

        for (handle, (_, _, previous)) in handles.into_iter().zip(targets.iter()) {
            let Ok((index, health)) = handle.await else {
                continue;
            };
            let Some(entry) = self.tunnels.get_mut(index) else {
                continue;
            };
            entry.health = health;
            let name = entry.tunnel.name.clone();
            self.show_health_result(&name, *previous, health);
        }
    }

    // Check health for a specific tunnel by index
    async fn check_health_for_index(&mut self, index: usize) {
        if let Some(entry) = self.tunnels.get_mut(index) {
            // No hostname means nothing to probe (external tunnels)
            if entry.tunnel.hostname.is_empty() {
                entry.health = HealthStatus::Unknown;
                if index == self.selected {
                    self.status_message = Some(unmanaged_message(entry, "health check"));
                }
                return;
            }

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

            let new_health = classify_health(result);

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

    // Get the selected tunnel's details (target and hostname)
    pub fn selected_tunnel_details(&self) -> Option<(&str, &str)> {
        self.tunnels
            .get(self.selected)
            .map(|e| (e.tunnel.target.as_str(), e.tunnel.hostname.as_str()))
    }

    // Move selection up
    pub fn select_previous(&mut self) -> bool {
        if !self.tunnels.is_empty() && self.selected > 0 {
            self.selected -= 1;
            self.log_scroll = 0;
            self.log_follow = true;
            self.refresh_logs();
            return true; // Selection changed
        }
        false
    }

    // Move selection down
    pub fn select_next(&mut self) -> bool {
        if !self.tunnels.is_empty() && self.selected < self.tunnels.len() - 1 {
            self.selected += 1;
            self.log_scroll = 0;
            self.log_follow = true;
            self.refresh_logs();
            return true; // Selection changed
        }
        false
    }

    // Scroll log view up (toward older lines)
    pub fn scroll_logs_up(&mut self, n: u16) {
        self.log_scroll = self.log_scroll.saturating_add(n);
        self.log_follow = false;
    }

    // Scroll log view down (toward newer lines)
    pub fn scroll_logs_down(&mut self, n: u16) {
        self.log_scroll = self.log_scroll.saturating_sub(n);
        if self.log_scroll == 0 {
            self.log_follow = true;
        }
    }

    // Jump to the bottom (newest) line and re-enable auto-follow
    pub fn scroll_logs_bottom(&mut self) {
        self.log_scroll = 0;
        self.log_follow = true;
    }

    // Jump to the top (oldest) line; render clamps to actual buffer size
    pub fn scroll_logs_top(&mut self) {
        self.log_scroll = u16::MAX;
        self.log_follow = false;
    }

    // Check if selected tunnel needs a health check (unknown or stale)
    pub fn selected_needs_health_check(&self) -> bool {
        self.tunnels
            .get(self.selected)
            .map(|e| {
                e.status == TunnelStatus::Running
                    && e.health == HealthStatus::Unknown
                    && !e.tunnel.hostname.is_empty()
            })
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

    // Start the edit tunnel flow — opens the two-tab edit sheet overlay
    pub fn start_edit(&mut self) {
        self.status_message = None;
        if self.config.is_none() {
            self.status_message = Some("Run 'ytunnel init' first".to_string());
            return;
        }

        let entry = match self.tunnels.get(self.selected) {
            Some(e) => e,
            None => {
                self.status_message = Some("No tunnel selected".to_string());
                return;
            }
        };

        // Only managed tunnels can be edited
        if !entry.kind.is_managed() {
            self.status_message = Some(unmanaged_message(entry, "edit"));
            return;
        }

        let tunnel = entry.tunnel.clone();
        self.edit_sheet = Some(crate::tui::edit_sheet::EditSheetState::from_tunnel(&tunnel));
        self.input_mode = InputMode::EditSheet;
    }

    // Cancel current input
    pub fn cancel_input(&mut self) {
        self.input_mode = InputMode::Normal;
        self.input.clear();
        self.new_tunnel_name = None;
        self.new_tunnel_target = None;
        self.confirm_message = None;
        self.pending_action = None;
        self.editing_tunnel_name = None;
        self.original_zone_id = None;
        self.original_hostname = None;
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
            InputMode::AddTarget if !self.input.is_empty() => {
                self.new_tunnel_target = Some(self.input.clone());
                self.input.clear();
                self.input_mode = InputMode::AddZone;
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

    // Check if selected tunnel is ephemeral
    pub fn is_selected_ephemeral(&self) -> bool {
        self.selected_kind() == Some(TunnelKind::Ephemeral)
    }

    // Kind of the currently selected tunnel, if there is one
    pub fn selected_kind(&self) -> Option<TunnelKind> {
        self.tunnels.get(self.selected).map(|e| e.kind)
    }

    // Copy the selected tunnel's URL to clipboard
    pub fn copy_url_to_clipboard(&mut self) {
        if let Some(entry) = self.tunnels.get(self.selected) {
            if entry.tunnel.hostname.is_empty() {
                self.status_message = Some(unmanaged_message(entry, "copy a URL for"));
                return;
            }
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
            if entry.tunnel.hostname.is_empty() {
                self.status_message = Some(unmanaged_message(entry, "open"));
                return;
            }
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
            if !entry.kind.is_managed() {
                self.status_message = Some(unmanaged_message(entry, "set auto-start for"));
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
            // External tunnels can't be imported: there are no local credentials
            // for them, so ytunnel could never run one itself
            self.status_message = match self.selected_kind() {
                Some(TunnelKind::External) => self
                    .tunnels
                    .get(self.selected)
                    .map(|e| unmanaged_message(e, "import")),
                _ => Some("Only ephemeral tunnels can be imported".to_string()),
            };
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
            tunnel_options: std::collections::BTreeMap::new(),
            origin_request: std::collections::BTreeMap::new(),
            log_mode: crate::state::LogMode::Default,
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

    // Request deletion of selected tunnel
    pub fn request_delete(&mut self) {
        if let Some(entry) = self.tunnels.get(self.selected) {
            // Refuse to delete tunnels we didn't create. We don't know their DNS
            // records, and the cloudflared serving them may be live on another
            // machine, so deleting would break traffic and orphan CNAMEs.
            if entry.kind == TunnelKind::External {
                self.status_message = Some(unmanaged_message(entry, "delete"));
                return;
            }

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
    // Mouse capture starts OFF — text selection works with native drag.
    // It's toggled ON only when Tab focuses the log pane (see Tab handler).
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app and load data
    let mut app = App::new(initial_account);
    if let Err(e) = app.load_tunnels().await {
        // Still show TUI even if load fails
        app.status_message = Some(format!("Error loading tunnels: {}", e));
    }

    // The initial health check runs on the loop's first tick instead of here,
    // so the tunnel list paints before we start probing hostnames

    // Main loop
    let result = run_app(&mut terminal, &mut app).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableBracketedPaste,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}

// Run the TUI in demo mode with synthetic data (no config required)
pub async fn run_demo_tui() -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // Mouse capture starts OFF — text selection works with native drag.
    // It's toggled ON only when Tab focuses the log pane (see Tab handler).
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create demo app with fake tunnels
    let mut app = App::new_demo();
    app.load_demo_tunnels();

    // Main loop
    let result = run_app(&mut terminal, &mut app).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableBracketedPaste,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    let mut last_metrics_refresh = std::time::Instant::now();
    let mut last_logs_refresh = std::time::Instant::now();
    let metrics_refresh_interval = Duration::from_secs(5);
    let health_check_interval = Duration::from_secs(30);
    let logs_refresh_interval = Duration::from_secs(1);
    // Backdated so the first health check fires immediately after the first draw
    let mut last_health_check = std::time::Instant::now()
        .checked_sub(health_check_interval)
        .unwrap_or_else(std::time::Instant::now);

    loop {
        terminal.draw(|f| ui::render(f, app))?;

        // Fold in any background Cloudflare discovery that finished
        app.poll_discovery();

        // Tick spinner animation
        app.spinner.tick();

        // Refresh metrics periodically (skip if spinner is active to avoid blocking)
        if !app.spinner.is_active() && last_metrics_refresh.elapsed() >= metrics_refresh_interval {
            app.refresh_metrics().await;
            last_metrics_refresh = std::time::Instant::now();
        }

        // Check health of all running tunnels less frequently (skip if spinner is active)
        if !app.spinner.is_active() && last_health_check.elapsed() >= health_check_interval {
            app.check_all_health().await;
            last_health_check = std::time::Instant::now();
        }

        // Tail the log file for the selected tunnel every second so new lines
        // appear live instead of only on start/stop/import.
        if !app.spinner.is_active() && last_logs_refresh.elapsed() >= logs_refresh_interval {
            app.refresh_logs();
            last_logs_refresh = std::time::Instant::now();
        }

        // Poll for events - use shorter timeout when spinner is active for smooth animation
        let poll_timeout = if app.spinner.is_active() {
            Duration::from_millis(80)
        } else {
            Duration::from_millis(250)
        };

        // Poll for events with timeout for async refresh
        if event::poll(poll_timeout)? {
            let event = event::read()?;

            // Handle paste events (some remote desktop software sends text as paste)
            if let Event::Paste(text) = &event {
                if matches!(app.input_mode, InputMode::AddName | InputMode::AddTarget) {
                    app.input.push_str(text);
                } else if let InputMode::EditSheetInput { buffer, .. } = &mut app.input_mode {
                    buffer.push_str(text);
                } else if let InputMode::EditSheetBasicInput { buffer, .. } = &mut app.input_mode {
                    buffer.push_str(text);
                }
                continue;
            }

            // Handle mouse events: click sets focus based on which pane was hit;
            // scroll wheel routes to the pane the cursor is currently over.
            // Left panel = tunnels list (left 40% of terminal width); right = logs.
            if let Event::Mouse(mouse) = &event {
                if app.input_mode == InputMode::Normal {
                    handle_mouse(app, *mouse);
                }
                continue;
            }

            if let Event::Key(key) = event {
                // Handle key press and repeat events (repeat needed for remote desktop)
                // Skip release events
                if key.kind == KeyEventKind::Release {
                    continue;
                }

                // Handle Ctrl+C to quit (before other handlers)
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    app.should_quit = true;
                    continue;
                }

                // Handle Ctrl+Z to suspend
                if key.code == KeyCode::Char('z') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    // Restore terminal before suspending
                    disable_raw_mode()?;
                    execute!(
                        terminal.backend_mut(),
                        LeaveAlternateScreen,
                        DisableBracketedPaste,
                        DisableMouseCapture
                    )?;
                    terminal.show_cursor()?;

                    // Send SIGTSTP to suspend
                    #[cfg(unix)]
                    {
                        unsafe {
                            libc::raise(libc::SIGTSTP);
                        }
                    }

                    // When resumed, restore terminal. Mouse capture is only
                    // re-enabled if focus is still on Logs.
                    enable_raw_mode()?;
                    execute!(
                        terminal.backend_mut(),
                        EnterAlternateScreen,
                        EnableBracketedPaste
                    )?;
                    if app.focus == Focus::Logs {
                        execute!(terminal.backend_mut(), EnableMouseCapture)?;
                    }
                    // Force full redraw
                    terminal.clear()?;
                    continue;
                }

                match app.input_mode {
                    InputMode::Normal => match key.code {
                        KeyCode::Char('q') => {
                            app.should_quit = true;
                        }
                        KeyCode::Char('a') => {
                            if !app.demo_guard() {
                                app.start_add();
                            }
                        }
                        KeyCode::Char('e') => {
                            if !app.demo_guard() {
                                app.start_edit();
                            }
                        }
                        KeyCode::Char('s') => {
                            if !app.demo_guard() {
                                if let Some(entry) = app.tunnels.get(app.selected) {
                                    if !entry.kind.is_managed() {
                                        app.status_message =
                                            Some(unmanaged_message(entry, "start"));
                                    } else {
                                        let name = entry.tunnel.name.clone();
                                        let account_name = entry.tunnel.account_name.clone();
                                        let tunnel = entry.tunnel.clone();

                                        app.spinner.start(&format!("Starting {}...", name));

                                        // Create and pin future ONCE
                                        let fut =
                                            start_tunnel_op(name.clone(), account_name, tunnel);
                                        tokio::pin!(fut);

                                        let result: Result<String> = loop {
                                            terminal.draw(|f| ui::render(f, app))?;

                                            // Check for cancel (Ctrl+C or Esc)
                                            if event::poll(Duration::from_millis(10))? {
                                                if let Event::Key(k) = event::read()? {
                                                    if is_cancel_key(&k) {
                                                        break Err(anyhow::anyhow!("Cancelled"));
                                                    }
                                                }
                                            }

                                            tokio::select! {
                                                biased;
                                                res = &mut fut => break res,
                                                _ = tokio::time::sleep(Duration::from_millis(70)) => {
                                                    app.spinner.tick();
                                                }
                                            }
                                        };

                                        app.spinner.stop();
                                        match result {
                                            Ok(name) => {
                                                app.status_message =
                                                    Some(format!("Started {}", name));
                                                app.load_tunnels().await?;
                                            }
                                            Err(e) if e.to_string() == "Cancelled" => {
                                                app.status_message = Some("Cancelled".to_string());
                                            }
                                            Err(e) => {
                                                app.status_message = Some(format!("Error: {}", e));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        KeyCode::Char('S') => {
                            if !app.demo_guard() {
                                if let Some(entry) = app.tunnels.get(app.selected) {
                                    if entry.kind == TunnelKind::Ephemeral {
                                        app.status_message = Some(
                                        "Cannot stop ephemeral tunnel from TUI. Use Ctrl+C in its terminal."
                                            .to_string(),
                                    );
                                    } else if entry.kind == TunnelKind::External {
                                        app.status_message = Some(unmanaged_message(entry, "stop"));
                                    } else {
                                        let name = entry.tunnel.name.clone();
                                        let account_name = entry.tunnel.account_name.clone();

                                        app.spinner.start(&format!("Stopping {}...", name));

                                        let fut = stop_tunnel_op(name.clone(), account_name);
                                        tokio::pin!(fut);

                                        let result: Result<String> = loop {
                                            terminal.draw(|f| ui::render(f, app))?;

                                            if event::poll(Duration::from_millis(10))? {
                                                if let Event::Key(k) = event::read()? {
                                                    if is_cancel_key(&k) {
                                                        break Err(anyhow::anyhow!("Cancelled"));
                                                    }
                                                }
                                            }

                                            tokio::select! {
                                                biased;
                                                res = &mut fut => break res,
                                                _ = tokio::time::sleep(Duration::from_millis(70)) => {
                                                    app.spinner.tick();
                                                }
                                            }
                                        };

                                        app.spinner.stop();
                                        match result {
                                            Ok(name) => {
                                                app.status_message =
                                                    Some(format!("Stopped {}", name));
                                                app.load_tunnels().await?;
                                            }
                                            Err(e) if e.to_string() == "Cancelled" => {
                                                app.status_message = Some("Cancelled".to_string());
                                            }
                                            Err(e) => {
                                                app.status_message = Some(format!("Error: {}", e));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        KeyCode::Char('d') => {
                            if key.modifiers.contains(KeyModifiers::CONTROL) {
                                app.scroll_logs_down(5);
                            } else if !app.demo_guard() {
                                app.request_delete();
                            }
                        }
                        KeyCode::Char('m') => {
                            if !app.demo_guard() {
                                if let Err(e) = app.start_import().await {
                                    app.status_message = Some(format!("Error: {}", e));
                                }
                            }
                        }
                        KeyCode::Char('r') => {
                            if app.demo {
                                app.refresh_demo_metrics();
                                app.status_message = Some("Refreshed (demo)".to_string());
                            } else {
                                // Refresh is typically quick, just show status
                                app.status_message = Some("Refreshing...".to_string());
                                terminal.draw(|f| ui::render(f, app))?;

                                // Explicit refresh re-queries Cloudflare too
                                if let Err(e) = app.reload_tunnels().await {
                                    app.status_message = Some(format!("Error: {}", e));
                                } else {
                                    app.status_message = Some("Refreshed".to_string());
                                    if app.selected_needs_health_check() {
                                        app.check_health().await;
                                    }
                                }
                            }
                        }
                        KeyCode::Char('R') => {
                            if !app.demo_guard() {
                                if let Some(entry) = app.tunnels.get(app.selected) {
                                    if !entry.kind.is_managed() {
                                        app.status_message =
                                            Some(unmanaged_message(entry, "restart"));
                                    } else {
                                        let name = entry.tunnel.name.clone();
                                        let account_name = entry.tunnel.account_name.clone();
                                        let tunnel = entry.tunnel.clone();

                                        app.spinner.start(&format!("Restarting {}...", name));

                                        let fut =
                                            restart_tunnel_op(name.clone(), account_name, tunnel);
                                        tokio::pin!(fut);

                                        let result: Result<String> = loop {
                                            terminal.draw(|f| ui::render(f, app))?;

                                            if event::poll(Duration::from_millis(10))? {
                                                if let Event::Key(k) = event::read()? {
                                                    if is_cancel_key(&k) {
                                                        break Err(anyhow::anyhow!("Cancelled"));
                                                    }
                                                }
                                            }

                                            tokio::select! {
                                                biased;
                                                res = &mut fut => break res,
                                                _ = tokio::time::sleep(Duration::from_millis(70)) => {
                                                    app.spinner.tick();
                                                }
                                            }
                                        };

                                        app.spinner.stop();
                                        match result {
                                            Ok(name) => {
                                                app.status_message =
                                                    Some(format!("Restarted {}", name));
                                                app.load_tunnels().await?;
                                            }
                                            Err(e) if e.to_string() == "Cancelled" => {
                                                app.status_message = Some("Cancelled".to_string());
                                            }
                                            Err(e) => {
                                                app.status_message = Some(format!("Error: {}", e));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        KeyCode::Char('c') => {
                            app.copy_url_to_clipboard();
                        }
                        KeyCode::Char('o') => {
                            if !app.demo_guard() {
                                app.open_in_browser();
                            }
                        }
                        KeyCode::Char('h') => {
                            if !app.demo_guard() {
                                app.check_health().await;
                            }
                        }
                        KeyCode::Char('A') => {
                            if !app.demo_guard() {
                                if let Err(e) = app.toggle_auto_start().await {
                                    app.status_message = Some(format!("Error: {}", e));
                                }
                            }
                        }
                        KeyCode::Char('?') => {
                            app.input_mode = InputMode::Help;
                        }
                        KeyCode::Tab => {
                            // Toggle focus AND terminal mouse capture: capture is
                            // on only when Logs are focused, so users can drag to
                            // select text in the terminal whenever focus is on
                            // the tunnel list.
                            app.focus = match app.focus {
                                Focus::Tunnels => {
                                    execute!(terminal.backend_mut(), EnableMouseCapture)?;
                                    Focus::Logs
                                }
                                Focus::Logs => {
                                    execute!(terminal.backend_mut(), DisableMouseCapture)?;
                                    Focus::Tunnels
                                }
                            };
                        }
                        KeyCode::Up | KeyCode::Char('k') => match app.focus {
                            Focus::Tunnels => {
                                if app.select_previous()
                                    && !app.demo
                                    && app.selected_needs_health_check()
                                {
                                    app.check_health().await;
                                }
                            }
                            Focus::Logs => {
                                app.scroll_logs_up(1);
                            }
                        },
                        KeyCode::Down | KeyCode::Char('j') => match app.focus {
                            Focus::Tunnels => {
                                if app.select_next()
                                    && !app.demo
                                    && app.selected_needs_health_check()
                                {
                                    app.check_health().await;
                                }
                            }
                            Focus::Logs => {
                                app.scroll_logs_down(1);
                            }
                        },
                        // Cycle to the next account. The guard is deliberately
                        // side-effecting: demo_guard sets the "actions
                        // disabled" status message when it returns true, and
                        // short-circuiting keeps it evaluated first. Either way
                        // a non-match falls through to `_ => {}`, so the
                        // message still shows and nothing else runs.
                        KeyCode::Char(';') if !app.demo_guard() && app.accounts.len() > 1 => {
                            app.next_account();
                            if let Err(e) = app.load_tunnels().await {
                                app.status_message = Some(format!("Error: {}", e));
                            }
                        }
                        KeyCode::PageUp => app.scroll_logs_up(10),
                        KeyCode::PageDown => app.scroll_logs_down(10),
                        KeyCode::Home => app.scroll_logs_top(),
                        KeyCode::End => app.scroll_logs_bottom(),
                        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.scroll_logs_up(5);
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
                            // Extract all data before creating future
                            let name = match app.new_tunnel_name.clone() {
                                Some(n) => n,
                                None => {
                                    app.status_message = Some("No tunnel name".to_string());
                                    app.input_mode = InputMode::Normal;
                                    continue;
                                }
                            };
                            let target = match app.new_tunnel_target.clone() {
                                Some(t) => t,
                                None => {
                                    app.status_message = Some("No target URL".to_string());
                                    app.input_mode = InputMode::Normal;
                                    continue;
                                }
                            };
                            let zone: config::ZoneConfig = match app.zones.get(app.zone_selected) {
                                Some(z) => z.clone(),
                                None => {
                                    app.status_message = Some("No zone selected".to_string());
                                    app.input_mode = InputMode::Normal;
                                    continue;
                                }
                            };
                            let account: Account = match app.current_account() {
                                Some(a) => a.clone(),
                                None => {
                                    app.status_message = Some("No account selected".to_string());
                                    app.input_mode = InputMode::Normal;
                                    continue;
                                }
                            };

                            let is_importing = app.is_importing;
                            let msg = if is_importing {
                                format!("Importing {}...", name)
                            } else {
                                format!("Creating {}...", name)
                            };
                            app.spinner.start(&msg);

                            // Get tunnel_id for import (from ephemeral tunnel)
                            let tunnel_id = if is_importing {
                                app.tunnels
                                    .iter()
                                    .find(|e| {
                                        e.tunnel.name == name && e.kind == TunnelKind::Ephemeral
                                    })
                                    .map(|e| e.tunnel.tunnel_id.clone())
                            } else {
                                None
                            };

                            // Create and pin the appropriate future
                            let result: Result<String> = if is_importing {
                                let tid = match tunnel_id {
                                    Some(t) => t,
                                    None => {
                                        app.spinner.stop();
                                        app.status_message =
                                            Some("Ephemeral tunnel not found".to_string());
                                        app.input_mode = InputMode::Normal;
                                        continue;
                                    }
                                };
                                let fut =
                                    import_tunnel_op(name.clone(), target, zone, tid, account);
                                tokio::pin!(fut);

                                loop {
                                    terminal.draw(|f| ui::render(f, app))?;

                                    if event::poll(Duration::from_millis(10))? {
                                        if let Event::Key(k) = event::read()? {
                                            if is_cancel_key(&k) {
                                                break Err(anyhow::anyhow!("Cancelled"));
                                            }
                                        }
                                    }

                                    tokio::select! {
                                        biased;
                                        res = &mut fut => break res,
                                        _ = tokio::time::sleep(Duration::from_millis(70)) => {
                                            app.spinner.tick();
                                        }
                                    }
                                }
                            } else {
                                let fut = create_tunnel_op(name.clone(), target, zone, account);
                                tokio::pin!(fut);

                                loop {
                                    terminal.draw(|f| ui::render(f, app))?;

                                    if event::poll(Duration::from_millis(10))? {
                                        if let Event::Key(k) = event::read()? {
                                            if is_cancel_key(&k) {
                                                break Err(anyhow::anyhow!("Cancelled"));
                                            }
                                        }
                                    }

                                    tokio::select! {
                                        biased;
                                        res = &mut fut => break res.map(|(n, _)| n),
                                        _ = tokio::time::sleep(Duration::from_millis(70)) => {
                                            app.spinner.tick();
                                        }
                                    }
                                }
                            };

                            app.spinner.stop();
                            app.new_tunnel_name = None;
                            app.new_tunnel_target = None;
                            app.input_mode = InputMode::Normal;

                            match result {
                                Ok(name) => {
                                    let action = if is_importing { "Imported" } else { "Created" };
                                    app.status_message =
                                        Some(format!("{} tunnel '{}'", action, name));
                                    app.load_tunnels().await?;
                                    // Select the new tunnel
                                    if let Some(pos) =
                                        app.tunnels.iter().position(|t| t.tunnel.name == name)
                                    {
                                        app.selected = pos;
                                        app.refresh_logs();
                                    }
                                }
                                Err(e) if e.to_string() == "Cancelled" => {
                                    app.status_message = Some("Cancelled".to_string());
                                }
                                Err(e) => {
                                    app.status_message = Some(format!("Error: {}", e));
                                }
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
                    InputMode::EditSheet => match key.code {
                        KeyCode::Esc => {
                            if app.edit_sheet.as_ref().is_some_and(|s| s.dirty) {
                                app.input_mode = InputMode::EditSheetConfirmDiscard;
                            } else {
                                app.edit_sheet = None;
                                app.input_mode = InputMode::Normal;
                                app.status_message = None;
                            }
                        }
                        KeyCode::Tab | KeyCode::BackTab => {
                            if let Some(s) = app.edit_sheet.as_mut() {
                                s.toggle_tab();
                            }
                        }
                        KeyCode::Up => {
                            if let Some(s) = app.edit_sheet.as_mut() {
                                match s.active_tab {
                                    crate::tui::edit_sheet::SheetTab::Advanced => s.select_prev(),
                                    crate::tui::edit_sheet::SheetTab::Basic => {
                                        s.select_basic_prev()
                                    }
                                }
                            }
                        }
                        KeyCode::Down => {
                            if let Some(s) = app.edit_sheet.as_mut() {
                                match s.active_tab {
                                    crate::tui::edit_sheet::SheetTab::Advanced => s.select_next(),
                                    crate::tui::edit_sheet::SheetTab::Basic => {
                                        s.select_basic_next()
                                    }
                                }
                            }
                        }
                        KeyCode::Char('d')
                            if app.edit_sheet.as_ref().is_some_and(|s| {
                                s.active_tab == crate::tui::edit_sheet::SheetTab::Advanced
                            }) =>
                        {
                            if let Some(s) = app.edit_sheet.as_mut() {
                                s.clear_selected();
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(s) = app.edit_sheet.as_ref() {
                                match s.active_tab {
                                    crate::tui::edit_sheet::SheetTab::Basic => {
                                        use crate::tui::edit_sheet::BasicField;
                                        match s.basic_selected {
                                            0 => {
                                                let buffer = s.target.clone();
                                                app.input_mode = InputMode::EditSheetBasicInput {
                                                    field: BasicField::Target,
                                                    buffer,
                                                };
                                            }
                                            1 => {
                                                // Zone: open picker over app.zones.
                                                if app.zones.is_empty() {
                                                    app.status_message = Some(
                                                        "No zones available for this account."
                                                            .to_string(),
                                                    );
                                                } else {
                                                    let cursor = app
                                                        .edit_sheet
                                                        .as_ref()
                                                        .and_then(|s| {
                                                            // Preselect current zone if present in list.
                                                            let cur_id = s
                                                                .pending_zone
                                                                .as_ref()
                                                                .map(|z| z.id.as_str())
                                                                .unwrap_or(
                                                                    s.original_zone_id.as_str(),
                                                                );
                                                            app.zones
                                                                .iter()
                                                                .position(|z| z.id == cur_id)
                                                        })
                                                        .unwrap_or(0);
                                                    app.input_mode =
                                                        InputMode::EditSheetZonePicker { cursor };
                                                }
                                            }
                                            2 => {
                                                let cursor = app
                                                    .edit_sheet
                                                    .as_ref()
                                                    .map(|s| log_mode_to_index(s.log_mode))
                                                    .unwrap_or(0);
                                                app.input_mode =
                                                    InputMode::EditSheetLogModePicker { cursor };
                                            }
                                            _ => {}
                                        }
                                    }
                                    crate::tui::edit_sheet::SheetTab::Advanced => {
                                        match s.selected_row {
                                            0 => {
                                                // Toggle auto_start in-place; no sub-modal.
                                                if let Some(sm) = app.edit_sheet.as_mut() {
                                                    sm.auto_start = !sm.auto_start;
                                                    sm.dirty = true;
                                                }
                                            }
                                            1 => {
                                                // Open MetricsPort input modal.
                                                use crate::tui::edit_sheet::BasicField;
                                                let buffer = s
                                                    .metrics_port
                                                    .map(|p| p.to_string())
                                                    .unwrap_or_default();
                                                app.input_mode = InputMode::EditSheetBasicInput {
                                                    field: BasicField::MetricsPort,
                                                    buffer,
                                                };
                                            }
                                            options_virtual_idx => {
                                                let options_idx = options_virtual_idx - 2;
                                                let row = &s.advanced_rows[options_idx];
                                                let yaml_key = row.spec.yaml_key.to_string();
                                                use crate::cloudflared_options::OptionKind;
                                                match &row.spec.kind {
                                                    OptionKind::Bool { default } => {
                                                        let current = match &row.value {
                                                            Some(crate::state::TunnelOptionValue::Bool(b)) => *b,
                                                            _ => *default,
                                                        };
                                                        if let Some(sm) = app.edit_sheet.as_mut() {
                                                            sm.advanced_rows[options_idx].value =
                                                                Some(crate::state::TunnelOptionValue::Bool(!current));
                                                            sm.dirty = true;
                                                        }
                                                    }
                                                    OptionKind::Enum { .. } => {
                                                        app.input_mode =
                                                            InputMode::EditSheetPicker {
                                                                yaml_key,
                                                                cursor: 0,
                                                            };
                                                    }
                                                    _ => {
                                                        let buffer = row.value.as_ref().map(|v| match v {
                                                            crate::state::TunnelOptionValue::String(s) => s.clone(),
                                                            crate::state::TunnelOptionValue::Int(n) => n.to_string(),
                                                            crate::state::TunnelOptionValue::List(items) => items.join("\n"),
                                                            crate::state::TunnelOptionValue::Bool(b) => b.to_string(),
                                                        }).unwrap_or_default();
                                                        app.input_mode =
                                                            InputMode::EditSheetInput {
                                                                yaml_key,
                                                                buffer,
                                                            };
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            save_edit_sheet(app).await;
                        }
                        _ => {}
                    },
                    InputMode::EditSheetInput { .. } => match key.code {
                        KeyCode::Esc => app.input_mode = InputMode::EditSheet,
                        KeyCode::Enter => {
                            commit_input_edit(app);
                            app.input_mode = InputMode::EditSheet;
                        }
                        KeyCode::Char(c) => {
                            if let InputMode::EditSheetInput { buffer, .. } = &mut app.input_mode {
                                buffer.push(c);
                            }
                        }
                        KeyCode::Backspace => {
                            if let InputMode::EditSheetInput { buffer, .. } = &mut app.input_mode {
                                buffer.pop();
                            }
                        }
                        _ => {}
                    },
                    InputMode::EditSheetBasicInput { .. } => match key.code {
                        KeyCode::Esc => app.input_mode = InputMode::EditSheet,
                        KeyCode::Enter => {
                            commit_basic_input_edit(app);
                            // commit_basic_input_edit transitions back to EditSheet on success;
                            // on parse failure it leaves input_mode unchanged so the user can
                            // correct the buffer.
                        }
                        KeyCode::Char(c) => {
                            if let InputMode::EditSheetBasicInput { buffer, .. } =
                                &mut app.input_mode
                            {
                                buffer.push(c);
                            }
                        }
                        KeyCode::Backspace => {
                            if let InputMode::EditSheetBasicInput { buffer, .. } =
                                &mut app.input_mode
                            {
                                buffer.pop();
                            }
                        }
                        _ => {}
                    },
                    InputMode::EditSheetPicker { .. } => match key.code {
                        KeyCode::Esc => app.input_mode = InputMode::EditSheet,
                        KeyCode::Up => {
                            if let InputMode::EditSheetPicker { cursor, .. } = &mut app.input_mode {
                                *cursor = cursor.saturating_sub(1);
                            }
                        }
                        KeyCode::Down => {
                            if let InputMode::EditSheetPicker { yaml_key, cursor } =
                                &mut app.input_mode
                            {
                                if let Some(spec) = crate::cloudflared_options::find(yaml_key) {
                                    if let crate::cloudflared_options::OptionKind::Enum {
                                        choices,
                                        ..
                                    } = &spec.kind
                                    {
                                        if *cursor + 1 < choices.len() {
                                            *cursor += 1;
                                        }
                                    }
                                }
                            }
                        }
                        KeyCode::Enter => {
                            commit_picker_edit(app);
                            app.input_mode = InputMode::EditSheet;
                        }
                        _ => {}
                    },
                    InputMode::EditSheetZonePicker { .. } => match key.code {
                        KeyCode::Esc => app.input_mode = InputMode::EditSheet,
                        KeyCode::Up => {
                            if let InputMode::EditSheetZonePicker { cursor } = &mut app.input_mode {
                                *cursor = cursor.saturating_sub(1);
                            }
                        }
                        KeyCode::Down => {
                            if let InputMode::EditSheetZonePicker { cursor } = &mut app.input_mode {
                                if *cursor + 1 < app.zones.len() {
                                    *cursor += 1;
                                }
                            }
                        }
                        KeyCode::Enter => {
                            if let InputMode::EditSheetZonePicker { cursor } = &app.input_mode {
                                let idx = *cursor;
                                if let Some(zone) = app.zones.get(idx).cloned() {
                                    if let Some(s) = app.edit_sheet.as_mut() {
                                        // Only stage if different from original.
                                        if zone.id == s.original_zone_id {
                                            s.pending_zone = None;
                                        } else {
                                            s.pending_zone = Some(zone);
                                            s.dirty = true;
                                        }
                                    }
                                }
                            }
                            app.input_mode = InputMode::EditSheet;
                        }
                        _ => {}
                    },
                    InputMode::EditSheetLogModePicker { .. } => match key.code {
                        KeyCode::Esc => app.input_mode = InputMode::EditSheet,
                        KeyCode::Up => {
                            if let InputMode::EditSheetLogModePicker { cursor } =
                                &mut app.input_mode
                            {
                                *cursor = cursor.saturating_sub(1);
                            }
                        }
                        KeyCode::Down => {
                            if let InputMode::EditSheetLogModePicker { cursor } =
                                &mut app.input_mode
                            {
                                if *cursor + 1 < 3 {
                                    *cursor += 1;
                                }
                            }
                        }
                        KeyCode::Enter => {
                            if let InputMode::EditSheetLogModePicker { cursor } = &app.input_mode {
                                let mode = log_mode_from_index(*cursor);
                                if let Some(s) = app.edit_sheet.as_mut() {
                                    if s.log_mode != mode {
                                        s.log_mode = mode;
                                        s.dirty = true;
                                    }
                                }
                            }
                            app.input_mode = InputMode::EditSheet;
                        }
                        _ => {}
                    },
                    InputMode::EditSheetConfirmDiscard => match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => {
                            app.edit_sheet = None;
                            app.input_mode = InputMode::Normal;
                            app.status_message = None;
                        }
                        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                            app.input_mode = InputMode::EditSheet;
                        }
                        _ => {}
                    },
                    InputMode::Confirm => match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => {
                            if let Some(PendingAction::Delete(name)) = app.pending_action.take() {
                                app.confirm_message = None;
                                app.input_mode = InputMode::Normal;

                                // Extract info from tunnel entry
                                let entry = app.tunnels.iter().find(|e| e.tunnel.name == name);
                                let is_ephemeral = entry
                                    .map(|e| e.kind == TunnelKind::Ephemeral)
                                    .unwrap_or(false);
                                let tunnel_id = entry.map(|e| e.tunnel.tunnel_id.clone());
                                let account_name = entry
                                    .map(|e| e.tunnel.account_name.clone())
                                    .unwrap_or_else(|| app.current_account_name().to_string());
                                let account = app.current_account().cloned();

                                app.spinner.start(&format!("Deleting {}...", name));

                                let fut = delete_tunnel_op(
                                    name.clone(),
                                    account_name,
                                    is_ephemeral,
                                    tunnel_id,
                                    account,
                                );
                                tokio::pin!(fut);

                                let result: Result<String> = loop {
                                    terminal.draw(|f| ui::render(f, app))?;

                                    if event::poll(Duration::from_millis(10))? {
                                        if let Event::Key(k) = event::read()? {
                                            if is_cancel_key(&k) {
                                                break Err(anyhow::anyhow!("Cancelled"));
                                            }
                                        }
                                    }

                                    tokio::select! {
                                        biased;
                                        res = &mut fut => break res,
                                        _ = tokio::time::sleep(Duration::from_millis(70)) => {
                                            app.spinner.tick();
                                        }
                                    }
                                };

                                app.spinner.stop();
                                match result {
                                    Ok(name) => {
                                        app.status_message = Some(format!("Deleted {}", name));
                                        // Deleting removes it from Cloudflare too, so the
                                        // cached listing is stale
                                        app.reload_tunnels().await?;
                                    }
                                    Err(e) if e.to_string() == "Cancelled" => {
                                        app.status_message = Some("Cancelled".to_string());
                                    }
                                    Err(e) => {
                                        app.status_message = Some(format!("Error: {}", e));
                                    }
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

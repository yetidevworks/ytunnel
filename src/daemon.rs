use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

use crate::state::{ensure_logs_dir, write_tunnel_config, PersistentTunnel, TunnelStatus};

// ============================================================================
// Platform-specific constants and paths
// ============================================================================

#[cfg(target_os = "macos")]
const LAUNCHD_LABEL_PREFIX: &str = "com.ytunnel";

#[cfg(target_os = "linux")]
const SYSTEMD_SERVICE_PREFIX: &str = "ytunnel-";

// ============================================================================
// macOS (launchd) implementation
// ============================================================================

#[cfg(target_os = "macos")]
fn launch_agents_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home.join("Library/LaunchAgents"))
}

#[cfg(target_os = "macos")]
fn launchd_label(account_name: &str, tunnel_name: &str) -> String {
    if account_name.is_empty() {
        // Legacy format for migration compatibility
        format!("{}.{}", LAUNCHD_LABEL_PREFIX, tunnel_name)
    } else {
        format!("{}.{}.{}", LAUNCHD_LABEL_PREFIX, account_name, tunnel_name)
    }
}

#[cfg(target_os = "macos")]
fn legacy_launchd_label(tunnel_name: &str) -> String {
    format!("{}.{}", LAUNCHD_LABEL_PREFIX, tunnel_name)
}

#[cfg(target_os = "macos")]
fn plist_path(account_name: &str, tunnel_name: &str) -> Result<PathBuf> {
    let agents_dir = launch_agents_dir()?;
    Ok(agents_dir.join(format!("{}.plist", launchd_label(account_name, tunnel_name))))
}

#[cfg(target_os = "macos")]
fn legacy_plist_path(tunnel_name: &str) -> Result<PathBuf> {
    let agents_dir = launch_agents_dir()?;
    Ok(agents_dir.join(format!("{}.plist", legacy_launchd_label(tunnel_name))))
}

/// Find the actual plist path - checks new naming first, then legacy
#[cfg(target_os = "macos")]
fn find_plist_path(account_name: &str, tunnel_name: &str) -> Result<Option<PathBuf>> {
    // First check the new naming convention
    let new_path = plist_path(account_name, tunnel_name)?;
    if new_path.exists() {
        return Ok(Some(new_path));
    }

    // Fall back to legacy naming (without account)
    let legacy_path = legacy_plist_path(tunnel_name)?;
    if legacy_path.exists() {
        return Ok(Some(legacy_path));
    }

    Ok(None)
}

/// Find the actual launchd label for a tunnel - checks new naming first, then legacy
#[cfg(target_os = "macos")]
async fn find_launchd_label(account_name: &str, tunnel_name: &str) -> String {
    // First check if the new label exists in launchctl
    let new_label = launchd_label(account_name, tunnel_name);
    if is_label_loaded(&new_label).await {
        return new_label;
    }

    // Check if legacy label exists
    let legacy_label = legacy_launchd_label(tunnel_name);
    if is_label_loaded(&legacy_label).await {
        return legacy_label;
    }

    // Default to new label (for new installations)
    new_label
}

#[cfg(target_os = "macos")]
async fn is_label_loaded(label: &str) -> bool {
    let output = Command::new("launchctl")
        .args(["list", label])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await;

    matches!(output, Ok(out) if out.status.success())
}

#[cfg(target_os = "macos")]
fn generate_plist(tunnel: &PersistentTunnel) -> Result<String> {
    let config_path = tunnel.config_path()?;
    let log_path = tunnel.log_path()?;
    let label = launchd_label(&tunnel.account_name, &tunnel.name);
    let metrics_port = tunnel.get_metrics_port();
    let run_at_load = if tunnel.auto_start { "true" } else { "false" };

    let cloudflared_path =
        which_cloudflared().unwrap_or_else(|| "/opt/homebrew/bin/cloudflared".to_string());

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{cloudflared}</string>
        <string>tunnel</string>
        <string>--config</string>
        <string>{config}</string>
        <string>--metrics</string>
        <string>localhost:{metrics_port}</string>
        <string>run</string>
    </array>
    <key>RunAtLoad</key>
    <{run_at_load}/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
    <key>ProcessType</key>
    <string>Background</string>
</dict>
</plist>
"#,
        label = label,
        cloudflared = cloudflared_path,
        config = config_path.display(),
        metrics_port = metrics_port,
        run_at_load = run_at_load,
        log = log_path.display()
    );

    Ok(plist)
}

#[cfg(target_os = "macos")]
pub async fn install_daemon(tunnel: &PersistentTunnel) -> Result<()> {
    ensure_logs_dir()?;
    let agents_dir = launch_agents_dir()?;
    fs::create_dir_all(&agents_dir).with_context(|| {
        format!(
            "Failed to create LaunchAgents directory: {}",
            agents_dir.display()
        )
    })?;

    write_tunnel_config(tunnel)?;

    let plist_content = generate_plist(tunnel)?;
    let path = plist_path(&tunnel.account_name, &tunnel.name)?;
    fs::write(&path, &plist_content)
        .with_context(|| format!("Failed to write plist to {}", path.display()))?;

    Ok(())
}

#[cfg(target_os = "macos")]
pub async fn uninstall_daemon(tunnel_name: &str, account_name: &str) -> Result<()> {
    stop_daemon(tunnel_name, account_name).await.ok();

    // Remove both new and legacy plist paths if they exist
    let new_path = plist_path(account_name, tunnel_name)?;
    if new_path.exists() {
        fs::remove_file(&new_path)
            .with_context(|| format!("Failed to remove plist: {}", new_path.display()))?;
    }

    let legacy_path = legacy_plist_path(tunnel_name)?;
    if legacy_path.exists() {
        fs::remove_file(&legacy_path)
            .with_context(|| format!("Failed to remove legacy plist: {}", legacy_path.display()))?;
    }

    Ok(())
}

#[cfg(target_os = "macos")]
pub async fn start_daemon(tunnel_name: &str, account_name: &str) -> Result<()> {
    // Check both new and legacy paths
    let path = match find_plist_path(account_name, tunnel_name)? {
        Some(p) => p,
        None => {
            anyhow::bail!(
                "Plist not found for tunnel '{}'. Try adding it again.",
                tunnel_name
            );
        }
    };

    let output = Command::new("launchctl")
        .args(["load", "-w"])
        .arg(&path)
        .output()
        .await
        .context("Failed to run launchctl load")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("already loaded") && !stderr.is_empty() {
            anyhow::bail!("Failed to start daemon: {}", stderr.trim());
        }
    }

    Ok(())
}

#[cfg(target_os = "macos")]
pub async fn stop_daemon(tunnel_name: &str, account_name: &str) -> Result<()> {
    // Check both new and legacy paths
    let path = match find_plist_path(account_name, tunnel_name)? {
        Some(p) => p,
        None => return Ok(()), // No plist found, nothing to stop
    };

    let output = Command::new("launchctl")
        .args(["unload"])
        .arg(&path)
        .output()
        .await
        .context("Failed to run launchctl unload")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("not find") && !stderr.contains("not loaded") && !stderr.is_empty() {
            anyhow::bail!("Failed to stop daemon: {}", stderr.trim());
        }
    }

    Ok(())
}

#[cfg(target_os = "macos")]
pub async fn is_daemon_running(tunnel_name: &str, account_name: &str) -> bool {
    // Check both new and legacy labels
    let new_label = launchd_label(account_name, tunnel_name);
    let legacy_label = legacy_launchd_label(tunnel_name);

    let output = Command::new("launchctl")
        .args(["list"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.lines().any(|line| {
                let parts: Vec<&str> = line.split('\t').collect();
                if parts.len() >= 3 && (parts[2] == new_label || parts[2] == legacy_label) {
                    parts[0].parse::<u32>().is_ok()
                } else {
                    false
                }
            })
        }
        _ => false,
    }
}

#[cfg(target_os = "macos")]
pub async fn get_daemon_status(tunnel: &PersistentTunnel) -> TunnelStatus {
    // Find the actual label being used (new or legacy)
    let label = find_launchd_label(&tunnel.account_name, &tunnel.name).await;

    let output = Command::new("launchctl")
        .args(["list", &label])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            if stdout.contains("\"PID\"") {
                // Running if: good exit status, no exit status info, or still actually running
                if stdout.contains("\"LastExitStatus\" = 0")
                    || !stdout.contains("\"LastExitStatus\"")
                    || is_daemon_running(&tunnel.name, &tunnel.account_name).await
                {
                    TunnelStatus::Running
                } else {
                    TunnelStatus::Error
                }
            } else {
                TunnelStatus::Stopped
            }
        }
        _ => TunnelStatus::Stopped,
    }
}

// ============================================================================
// Linux (systemd) implementation
// ============================================================================

#[cfg(target_os = "linux")]
fn systemd_user_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home.join(".config/systemd/user"))
}

#[cfg(target_os = "linux")]
fn service_name(account_name: &str, tunnel_name: &str) -> String {
    if account_name.is_empty() {
        // Legacy format for migration compatibility
        format!("{}{}.service", SYSTEMD_SERVICE_PREFIX, tunnel_name)
    } else {
        format!("{}{}-{}.service", SYSTEMD_SERVICE_PREFIX, account_name, tunnel_name)
    }
}

#[cfg(target_os = "linux")]
fn service_path(account_name: &str, tunnel_name: &str) -> Result<PathBuf> {
    let systemd_dir = systemd_user_dir()?;
    Ok(systemd_dir.join(service_name(account_name, tunnel_name)))
}

#[cfg(target_os = "linux")]
fn generate_service(tunnel: &PersistentTunnel) -> Result<String> {
    let config_path = tunnel.config_path()?;
    let log_path = tunnel.log_path()?;
    let metrics_port = tunnel.get_metrics_port();

    let cloudflared_path =
        which_cloudflared().unwrap_or_else(|| "/usr/local/bin/cloudflared".to_string());

    let service = format!(
        r#"[Unit]
Description=Cloudflare Tunnel - {name}
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={cloudflared} tunnel --config {config} --metrics localhost:{metrics_port} run
Restart=on-failure
RestartSec=5
StandardOutput=append:{log}
StandardError=append:{log}

[Install]
WantedBy=default.target
"#,
        name = tunnel.name,
        cloudflared = cloudflared_path,
        config = config_path.display(),
        metrics_port = metrics_port,
        log = log_path.display()
    );

    Ok(service)
}

#[cfg(target_os = "linux")]
async fn daemon_reload() -> Result<()> {
    let output = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output()
        .await
        .context("Failed to run systemctl daemon-reload")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to reload systemd: {}", stderr.trim());
    }

    Ok(())
}

#[cfg(target_os = "linux")]
pub async fn install_daemon(tunnel: &PersistentTunnel) -> Result<()> {
    ensure_logs_dir()?;
    let systemd_dir = systemd_user_dir()?;
    fs::create_dir_all(&systemd_dir).with_context(|| {
        format!(
            "Failed to create systemd user directory: {}",
            systemd_dir.display()
        )
    })?;

    write_tunnel_config(tunnel)?;

    let service_content = generate_service(tunnel)?;
    let path = service_path(&tunnel.account_name, &tunnel.name)?;
    fs::write(&path, &service_content)
        .with_context(|| format!("Failed to write service file to {}", path.display()))?;

    daemon_reload().await?;

    // Enable if auto_start is set
    if tunnel.auto_start {
        let svc = service_name(&tunnel.account_name, &tunnel.name);
        Command::new("systemctl")
            .args(["--user", "enable", &svc])
            .output()
            .await
            .context("Failed to enable service")?;
    } else {
        // Disable if auto_start is false
        let svc = service_name(&tunnel.account_name, &tunnel.name);
        Command::new("systemctl")
            .args(["--user", "disable", &svc])
            .output()
            .await
            .ok(); // Ignore errors if not enabled
    }

    Ok(())
}

#[cfg(target_os = "linux")]
pub async fn uninstall_daemon(tunnel_name: &str, account_name: &str) -> Result<()> {
    let svc = service_name(account_name, tunnel_name);

    // Stop and disable
    Command::new("systemctl")
        .args(["--user", "stop", &svc])
        .output()
        .await
        .ok();

    Command::new("systemctl")
        .args(["--user", "disable", &svc])
        .output()
        .await
        .ok();

    // Remove the service file
    let path = service_path(account_name, tunnel_name)?;
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("Failed to remove service file: {}", path.display()))?;
    }

    daemon_reload().await?;

    Ok(())
}

#[cfg(target_os = "linux")]
pub async fn start_daemon(tunnel_name: &str, account_name: &str) -> Result<()> {
    let path = service_path(account_name, tunnel_name)?;

    if !path.exists() {
        anyhow::bail!(
            "Service file not found for tunnel '{}'. Try adding it again.",
            tunnel_name
        );
    }

    let svc = service_name(account_name, tunnel_name);
    let output = Command::new("systemctl")
        .args(["--user", "start", &svc])
        .output()
        .await
        .context("Failed to run systemctl start")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to start daemon: {}", stderr.trim());
    }

    Ok(())
}

#[cfg(target_os = "linux")]
pub async fn stop_daemon(tunnel_name: &str, account_name: &str) -> Result<()> {
    let path = service_path(account_name, tunnel_name)?;

    if !path.exists() {
        return Ok(());
    }

    let svc = service_name(account_name, tunnel_name);
    let output = Command::new("systemctl")
        .args(["--user", "stop", &svc])
        .output()
        .await
        .context("Failed to run systemctl stop")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Ignore "not loaded" type errors
        if !stderr.contains("not loaded") && !stderr.is_empty() {
            anyhow::bail!("Failed to stop daemon: {}", stderr.trim());
        }
    }

    Ok(())
}

#[cfg(target_os = "linux")]
#[allow(dead_code)] // Public API for consistency with macOS
pub async fn is_daemon_running(tunnel_name: &str, account_name: &str) -> bool {
    let svc = service_name(account_name, tunnel_name);

    let output = Command::new("systemctl")
        .args(["--user", "is-active", &svc])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await;

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.trim() == "active"
        }
        _ => false,
    }
}

#[cfg(target_os = "linux")]
pub async fn get_daemon_status(tunnel: &PersistentTunnel) -> TunnelStatus {
    let svc = service_name(&tunnel.account_name, &tunnel.name);

    let output = Command::new("systemctl")
        .args(["--user", "is-active", &svc])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await;

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            match stdout.trim() {
                "active" => TunnelStatus::Running,
                "failed" => TunnelStatus::Error,
                _ => TunnelStatus::Stopped,
            }
        }
        _ => TunnelStatus::Stopped,
    }
}

// ============================================================================
// Shared utilities
// ============================================================================

// Find the path to cloudflared
fn which_cloudflared() -> Option<String> {
    #[cfg(target_os = "macos")]
    let paths = [
        "/opt/homebrew/bin/cloudflared",
        "/usr/local/bin/cloudflared",
    ];

    #[cfg(target_os = "linux")]
    let paths = ["/usr/local/bin/cloudflared", "/usr/bin/cloudflared"];

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let paths: [&str; 0] = [];

    for path in paths {
        if std::path::Path::new(path).exists() {
            return Some(path.to_string());
        }
    }

    // Try `which` as fallback
    std::process::Command::new("which")
        .arg("cloudflared")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
}

// Read recent log lines for a tunnel
pub fn read_log_tail(tunnel: &PersistentTunnel, lines: usize) -> Result<Vec<String>> {
    let log_path = tunnel.log_path()?;

    if !log_path.exists() {
        return Ok(vec!["No logs yet".to_string()]);
    }

    let content = fs::read_to_string(&log_path)
        .with_context(|| format!("Failed to read log file: {}", log_path.display()))?;

    let all_lines: Vec<String> = content.lines().map(String::from).collect();

    let start = if all_lines.len() > lines {
        all_lines.len() - lines
    } else {
        0
    };

    Ok(all_lines[start..].to_vec())
}

// ============================================================================
// Unsupported platforms
// ============================================================================

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub async fn install_daemon(_tunnel: &PersistentTunnel) -> Result<()> {
    anyhow::bail!("Daemon management is not supported on this platform. Use 'ytunnel run' for ephemeral tunnels.")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub async fn uninstall_daemon(_tunnel_name: &str, _account_name: &str) -> Result<()> {
    anyhow::bail!("Daemon management is not supported on this platform")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub async fn start_daemon(_tunnel_name: &str, _account_name: &str) -> Result<()> {
    anyhow::bail!("Daemon management is not supported on this platform. Use 'ytunnel run' for ephemeral tunnels.")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub async fn stop_daemon(_tunnel_name: &str, _account_name: &str) -> Result<()> {
    anyhow::bail!("Daemon management is not supported on this platform")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub async fn is_daemon_running(_tunnel_name: &str, _account_name: &str) -> bool {
    false
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub async fn get_daemon_status(_tunnel: &PersistentTunnel) -> TunnelStatus {
    TunnelStatus::Stopped
}

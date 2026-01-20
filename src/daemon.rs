use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

use crate::state::{ensure_logs_dir, write_tunnel_config, PersistentTunnel, TunnelStatus};

const LAUNCHD_LABEL_PREFIX: &str = "com.ytunnel";

/// Get the LaunchAgents directory path
fn launch_agents_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home.join("Library/LaunchAgents"))
}

/// Get the launchd label for a tunnel
fn launchd_label(tunnel_name: &str) -> String {
    format!("{}.{}", LAUNCHD_LABEL_PREFIX, tunnel_name)
}

/// Get the plist file path for a tunnel
fn plist_path(tunnel_name: &str) -> Result<PathBuf> {
    let agents_dir = launch_agents_dir()?;
    Ok(agents_dir.join(format!("{}.plist", launchd_label(tunnel_name))))
}

/// Generate the launchd plist XML content for a tunnel
fn generate_plist(tunnel: &PersistentTunnel) -> Result<String> {
    let config_path = tunnel.config_path()?;
    let log_path = tunnel.log_path()?;
    let label = launchd_label(&tunnel.name);
    let metrics_port = tunnel.get_metrics_port();
    let run_at_load = if tunnel.auto_start { "true" } else { "false" };

    // Find cloudflared path
    let cloudflared_path = which_cloudflared().unwrap_or_else(|| "/opt/homebrew/bin/cloudflared".to_string());

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

/// Find the path to cloudflared
fn which_cloudflared() -> Option<String> {
    // Common paths on macOS
    let paths = [
        "/opt/homebrew/bin/cloudflared",
        "/usr/local/bin/cloudflared",
    ];

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

/// Install and load the launchd plist for a tunnel
pub async fn install_daemon(tunnel: &PersistentTunnel) -> Result<()> {
    // Ensure directories exist
    ensure_logs_dir()?;
    let agents_dir = launch_agents_dir()?;
    fs::create_dir_all(&agents_dir)
        .with_context(|| format!("Failed to create LaunchAgents directory: {}", agents_dir.display()))?;

    // Write the tunnel config
    write_tunnel_config(tunnel)?;

    // Generate and write the plist
    let plist_content = generate_plist(tunnel)?;
    let path = plist_path(&tunnel.name)?;
    fs::write(&path, &plist_content)
        .with_context(|| format!("Failed to write plist to {}", path.display()))?;

    Ok(())
}

/// Uninstall the launchd plist for a tunnel
pub async fn uninstall_daemon(tunnel_name: &str) -> Result<()> {
    // Stop if running
    stop_daemon(tunnel_name).await.ok();

    // Remove the plist file
    let path = plist_path(tunnel_name)?;
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("Failed to remove plist: {}", path.display()))?;
    }

    Ok(())
}

/// Start the daemon for a tunnel
pub async fn start_daemon(tunnel_name: &str) -> Result<()> {
    let path = plist_path(tunnel_name)?;

    if !path.exists() {
        anyhow::bail!("Plist not found for tunnel '{}'. Try adding it again.", tunnel_name);
    }

    let output = Command::new("launchctl")
        .args(["load", "-w"])
        .arg(&path)
        .output()
        .await
        .context("Failed to run launchctl load")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Ignore "already loaded" errors
        if !stderr.contains("already loaded") && !stderr.is_empty() {
            anyhow::bail!("Failed to start daemon: {}", stderr.trim());
        }
    }

    Ok(())
}

/// Stop the daemon for a tunnel
pub async fn stop_daemon(tunnel_name: &str) -> Result<()> {
    let path = plist_path(tunnel_name)?;

    if !path.exists() {
        return Ok(());
    }

    let output = Command::new("launchctl")
        .args(["unload"])
        .arg(&path)
        .output()
        .await
        .context("Failed to run launchctl unload")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Ignore "not loaded" errors
        if !stderr.contains("not find") && !stderr.contains("not loaded") && !stderr.is_empty() {
            anyhow::bail!("Failed to stop daemon: {}", stderr.trim());
        }
    }

    Ok(())
}

/// Check if a daemon is running
pub async fn is_daemon_running(tunnel_name: &str) -> bool {
    let label = launchd_label(tunnel_name);

    let output = Command::new("launchctl")
        .args(["list"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            // Look for the label in the output
            // Format: "PID\tStatus\tLabel"
            stdout.lines().any(|line| {
                let parts: Vec<&str> = line.split('\t').collect();
                if parts.len() >= 3 && parts[2] == label {
                    // Check if PID is a number (running) vs "-" (not running)
                    parts[0].parse::<u32>().is_ok()
                } else {
                    false
                }
            })
        }
        _ => false,
    }
}

/// Get the current status of a tunnel daemon
pub async fn get_daemon_status(tunnel: &PersistentTunnel) -> TunnelStatus {
    let label = launchd_label(&tunnel.name);

    let output = Command::new("launchctl")
        .args(["list", &label])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            // If we get output and it contains PID, it's running
            // Format shows "PID" = number if running
            if stdout.contains("\"PID\"") {
                // Check exit status
                if stdout.contains("\"LastExitStatus\" = 0") || !stdout.contains("\"LastExitStatus\"") {
                    TunnelStatus::Running
                } else {
                    // Has non-zero exit status but might still be running with KeepAlive
                    if is_daemon_running(&tunnel.name).await {
                        TunnelStatus::Running
                    } else {
                        TunnelStatus::Error
                    }
                }
            } else {
                TunnelStatus::Stopped
            }
        }
        _ => {
            // Daemon not loaded
            TunnelStatus::Stopped
        }
    }
}

/// Read recent log lines for a tunnel
pub fn read_log_tail(tunnel: &PersistentTunnel, lines: usize) -> Result<Vec<String>> {
    let log_path = tunnel.log_path()?;

    if !log_path.exists() {
        return Ok(vec!["No logs yet".to_string()]);
    }

    let content = fs::read_to_string(&log_path)
        .with_context(|| format!("Failed to read log file: {}", log_path.display()))?;

    let all_lines: Vec<String> = content.lines().map(String::from).collect();

    // Return last N lines
    let start = if all_lines.len() > lines {
        all_lines.len() - lines
    } else {
        0
    };

    Ok(all_lines[start..].to_vec())
}

use anyhow::{Context, Result};
use std::fs;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::config;

pub async fn is_cloudflared_installed() -> bool {
    Command::new("cloudflared")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

pub async fn run_tunnel(
    tunnel_id: &str,
    credentials_path: &std::path::Path,
    hostname: &str,
    target: &str,
) -> Result<()> {
    use std::collections::BTreeMap;

    let config_dir = config::config_dir()?;
    let config_path = config_dir.join(format!("tunnel-{}.yml", tunnel_id));

    let config_content = crate::state::build_tunnel_yaml(
        tunnel_id,
        credentials_path,
        hostname,
        target,
        &BTreeMap::new(),
        &BTreeMap::new(),
    )?;

    fs::write(&config_path, &config_content)
        .with_context(|| format!("Failed to write tunnel config to {}", config_path.display()))?;

    // Run cloudflared with the config
    let mut child = Command::new("cloudflared")
        .arg("tunnel")
        .arg("--config")
        .arg(&config_path)
        .arg("run")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to start cloudflared")?;

    let display_target = if target.starts_with("http://") || target.starts_with("https://") {
        target.to_string()
    } else {
        format!("http://{}", target)
    };
    println!("Tunnel running: https://{} -> {}", hostname, display_target);
    println!("{}", "─".repeat(50));

    // Stream stderr (cloudflared logs to stderr)
    let stderr = child.stderr.take().context("Failed to capture stderr")?;
    let mut reader = BufReader::new(stderr).lines();

    // Handle Ctrl+C
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            line = reader.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        // Filter and display relevant log lines
                        if should_display_log(&line) {
                            println!("{}", line);
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        eprintln!("Error reading cloudflared output: {}", e);
                        break;
                    }
                }
            }
            _ = &mut ctrl_c => {
                println!("\n\nShutting down tunnel...");
                child.kill().await.ok();
                break;
            }
        }
    }

    // Clean up config file
    fs::remove_file(&config_path).ok();

    Ok(())
}

fn should_display_log(line: &str) -> bool {
    // Show connection status and errors, filter out noisy debug info
    line.contains("INF")
        || line.contains("ERR")
        || line.contains("WRN")
        || line.contains("connection")
        || line.contains("registered")
        || line.contains("Tunnel")
}

mod cli;
mod cloudflare;
mod config;
mod daemon;
mod metrics;
mod state;
mod tui;
mod tunnel;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands, ZonesCommands};
use state::{write_tunnel_config, PersistentTunnel, TunnelState};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => {
            // Default: open TUI
            tui::run_tui().await?;
        }
        Some(Commands::Init) => {
            cmd_init().await?;
        }
        Some(Commands::Run { args, zone }) => {
            // Parse args: if 1 arg it's target, if 2 args it's name + target
            let (name, target) = if args.len() == 2 {
                (Some(args[0].clone()), args[1].clone())
            } else {
                (None, args[0].clone())
            };
            cmd_run(name, target, zone).await?;
        }
        Some(Commands::Add {
            name,
            target,
            zone,
            start,
        }) => {
            cmd_add(name, target, zone, start).await?;
        }
        Some(Commands::Start { name }) => {
            cmd_start(name).await?;
        }
        Some(Commands::Stop { name }) => {
            cmd_stop(name).await?;
        }
        Some(Commands::Restart { name }) => {
            cmd_restart(name).await?;
        }
        Some(Commands::Logs {
            name,
            follow,
            lines,
        }) => {
            cmd_logs(name, follow, lines).await?;
        }
        Some(Commands::Zones { command }) => match command {
            None => cmd_zones_list().await?,
            Some(ZonesCommands::Default { domain }) => cmd_zones_default(domain).await?,
        },
        Some(Commands::List) => {
            cmd_list().await?;
        }
        Some(Commands::Delete { name }) => {
            cmd_delete(name).await?;
        }
        Some(Commands::Reset { yes }) => {
            cmd_reset(yes).await?;
        }
    }

    Ok(())
}

async fn cmd_init() -> Result<()> {
    // Check if already configured
    if config::config_path()?.exists() {
        println!("ytunnel is already configured.");
        println!("To reconfigure with new credentials, run: ytunnel reset");
        return Ok(());
    }

    println!("Initializing ytunnel...\n");

    // Check if cloudflared is installed
    if !tunnel::is_cloudflared_installed().await {
        anyhow::bail!(
            "cloudflared is not installed. Please install it first:\n  \
             brew install cloudflare/cloudflare/cloudflared"
        );
    }
    println!("✓ cloudflared found");

    // Get API token
    println!("\nEnter your Cloudflare API token:");
    println!("  Required permissions: Zone:Read, DNS:Edit, Cloudflare Tunnel:Edit");
    let mut token = String::new();
    std::io::stdin().read_line(&mut token)?;
    let token = token.trim().to_string();

    if token.is_empty() {
        anyhow::bail!("API token cannot be empty");
    }

    // Verify token and fetch zones
    println!("\nVerifying token and fetching zones...");
    let client = cloudflare::Client::new(&token);
    let zones = client.list_zones().await?;

    if zones.is_empty() {
        anyhow::bail!("No zones found for this API token");
    }

    println!("✓ Found {} zone(s):", zones.len());
    for (i, zone) in zones.iter().enumerate() {
        println!("  {}. {} ({})", i + 1, zone.name, zone.id);
    }

    // Get account ID from first zone
    let account_id = zones[0].account_id.clone();

    // Set default zone
    let default_zone = &zones[0];
    println!(
        "\nSetting default zone to: {} (change with `ytunnel zones default <domain>`)",
        default_zone.name
    );

    // Save config
    let cfg = config::Config {
        api_token: token,
        account_id,
        default_zone_id: default_zone.id.clone(),
        default_zone_name: default_zone.name.clone(),
        zones: zones
            .into_iter()
            .map(|z| config::ZoneConfig {
                id: z.id,
                name: z.name,
            })
            .collect(),
    };
    config::save_config(&cfg)?;

    println!(
        "\n✓ Configuration saved to {}",
        config::config_path()?.display()
    );
    println!("\nYou're ready! Try:");
    println!("  ytunnel                                 # open TUI dashboard");
    println!("  ytunnel add myapp localhost:3000 -s     # add and start a tunnel");
    println!("  ytunnel run localhost:3000              # ephemeral tunnel");

    Ok(())
}

// Run an ephemeral tunnel (foreground, stops on Ctrl+C)
async fn cmd_run(name: Option<String>, target: String, zone: Option<String>) -> Result<()> {
    let cfg = config::load_config()?;
    let client = cloudflare::Client::new(&cfg.api_token);

    // Determine zone
    let (zone_id, zone_name) = if let Some(z) = zone {
        // Find zone by name
        let found = cfg.zones.iter().find(|zc| zc.name == z);
        match found {
            Some(zc) => (zc.id.clone(), zc.name.clone()),
            None => anyhow::bail!(
                "Zone '{}' not found. Run `ytunnel zones` to see available zones.",
                z
            ),
        }
    } else {
        (cfg.default_zone_id.clone(), cfg.default_zone_name.clone())
    };

    // Determine subdomain name
    let subdomain = match name {
        Some(n) => {
            // Check if it's a full domain or just a subdomain part
            if n.contains('.') {
                // Full domain like "license.tunnel.rhuk.net" - extract the subdomain part
                n.strip_suffix(&format!(".{}", zone_name))
                    .map(|s| s.to_string())
                    .unwrap_or(n)
            } else {
                n
            }
        }
        None => {
            // Generate random name
            use rand::Rng;
            let mut rng = rand::rng();
            let suffix: String = (0..6)
                .map(|_| {
                    let chars: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
                    chars[rng.random_range(0..chars.len())] as char
                })
                .collect();
            format!("ytunnel-{}", suffix)
        }
    };

    let full_hostname = format!("{}.{}", subdomain, zone_name);
    println!("Setting up tunnel: {} -> {}", full_hostname, target);

    // Check if tunnel exists, create if not
    let tunnel_name = format!("ytunnel-{}", subdomain);
    let (tunnel, credentials_path) = match client
        .get_tunnel_by_name(&cfg.account_id, &tunnel_name)
        .await?
    {
        Some(t) => {
            println!("✓ Using existing tunnel: {}", t.name);
            let creds_path = t.credentials_path()?;
            if !creds_path.exists() {
                anyhow::bail!(
                    "Credentials file not found: {}\n\
                     This tunnel may have been created outside ytunnel.\n\
                     Delete it with `ytunnel delete {}` and try again.",
                    creds_path.display(),
                    subdomain
                );
            }
            (t, creds_path)
        }
        None => {
            println!("Creating tunnel: {}", tunnel_name);
            let result = client.create_tunnel(&cfg.account_id, &tunnel_name).await?;
            (result.tunnel, result.credentials_path)
        }
    };

    // Ensure DNS record exists
    println!("Configuring DNS record...");
    client
        .ensure_dns_record(&zone_id, &full_hostname, &tunnel.id)
        .await?;
    println!("✓ DNS configured: {}", full_hostname);

    // Run the tunnel
    println!("\nStarting tunnel (Ctrl+C to stop)...\n");
    tunnel::run_tunnel(&tunnel.id, &credentials_path, &full_hostname, &target).await?;

    Ok(())
}

// Add a persistent tunnel (non-interactive CLI command)
async fn cmd_add(name: String, target: String, zone: Option<String>, start: bool) -> Result<()> {
    let cfg = config::load_config()?;
    let client = cloudflare::Client::new(&cfg.api_token);

    // Check if tunnel already exists in state
    let state = TunnelState::load()?;
    if state.find(&name).is_some() {
        anyhow::bail!(
            "Tunnel '{}' already exists. Use `ytunnel delete {}` first.",
            name,
            name
        );
    }

    // Determine zone
    let (zone_id, zone_name) = if let Some(z) = zone {
        let found = cfg.zones.iter().find(|zc| zc.name == z);
        match found {
            Some(zc) => (zc.id.clone(), zc.name.clone()),
            None => anyhow::bail!(
                "Zone '{}' not found. Run `ytunnel zones` to see available zones.",
                z
            ),
        }
    } else {
        (cfg.default_zone_id.clone(), cfg.default_zone_name.clone())
    };

    let tunnel_name = format!("ytunnel-{}", name);
    let hostname = format!("{}.{}", name, zone_name);

    println!("Adding tunnel: {} -> {}", hostname, target);

    // Check if tunnel exists in Cloudflare, create if not
    let (cf_tunnel, _credentials_path) = match client
        .get_tunnel_by_name(&cfg.account_id, &tunnel_name)
        .await?
    {
        Some(t) => {
            let creds_path = t.credentials_path()?;
            if !creds_path.exists() {
                anyhow::bail!(
                    "Credentials file not found: {}\n\
                     This tunnel may have been created outside ytunnel.\n\
                     Delete it with `ytunnel delete {}` and try again.",
                    creds_path.display(),
                    name
                );
            }
            println!("✓ Using existing Cloudflare tunnel: {}", t.name);
            (t, creds_path)
        }
        None => {
            println!("Creating Cloudflare tunnel: {}", tunnel_name);
            let result = client.create_tunnel(&cfg.account_id, &tunnel_name).await?;
            (result.tunnel, result.credentials_path)
        }
    };

    // Ensure DNS record exists
    println!("Configuring DNS record...");
    client
        .ensure_dns_record(&zone_id, &hostname, &cf_tunnel.id)
        .await?;
    println!("✓ DNS configured: {}", hostname);

    // Create persistent tunnel
    let persistent = PersistentTunnel {
        name: name.clone(),
        target,
        zone_id,
        zone_name,
        hostname: hostname.clone(),
        tunnel_id: cf_tunnel.id,
        enabled: start,
        auto_start: false,
        metrics_port: None,
    };

    // Write tunnel config
    write_tunnel_config(&persistent)?;

    // Install daemon
    daemon::install_daemon(&persistent).await?;
    println!("✓ Daemon installed");

    // Save to state
    let mut state = TunnelState::load()?;
    state.add(persistent);
    state.save()?;
    println!("✓ Tunnel saved to state");

    if start {
        daemon::start_daemon(&name).await?;
        println!("✓ Tunnel started");
        println!("\nTunnel running: https://{}", hostname);
    } else {
        println!("\nTunnel added. Start with: ytunnel start {}", name);
    }

    Ok(())
}

// Start a stopped tunnel
async fn cmd_start(name: String) -> Result<()> {
    let mut state = TunnelState::load()?;

    // Get tunnel info and hostname before mutable borrow
    let (hostname, tunnel_clone) = {
        let tunnel = state.find(&name).ok_or_else(|| {
            anyhow::anyhow!(
                "Tunnel '{}' not found. Run `ytunnel list` to see available tunnels.",
                name
            )
        })?;
        (tunnel.hostname.clone(), tunnel.clone())
    };

    // Ensure config file exists
    write_tunnel_config(&tunnel_clone)?;

    // Ensure daemon is installed
    daemon::install_daemon(&tunnel_clone).await?;

    // Start the daemon
    daemon::start_daemon(&name).await?;

    // Update state
    if let Some(t) = state.find_mut(&name) {
        t.enabled = true;
    }
    state.save()?;

    println!("✓ Started tunnel: {}", name);
    println!("  https://{}", hostname);

    Ok(())
}

// Stop a running tunnel
async fn cmd_stop(name: String) -> Result<()> {
    let mut state = TunnelState::load()?;

    // Get hostname before mutable borrow
    let hostname = {
        let tunnel = state.find(&name).ok_or_else(|| {
            anyhow::anyhow!(
                "Tunnel '{}' not found. Run `ytunnel list` to see available tunnels.",
                name
            )
        })?;
        tunnel.hostname.clone()
    };

    daemon::stop_daemon(&name).await?;

    // Update state
    if let Some(t) = state.find_mut(&name) {
        t.enabled = false;
    }
    state.save()?;

    println!("✓ Stopped tunnel: {}", name);
    println!("  {}", hostname);

    Ok(())
}

// Restart a running tunnel (stop, reinstall daemon config, start)
async fn cmd_restart(name: String) -> Result<()> {
    let state = TunnelState::load()?;

    let tunnel = state
        .find(&name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Tunnel '{}' not found. Run `ytunnel list` to see available tunnels.",
                name
            )
        })?
        .clone();

    println!("Restarting tunnel: {}", name);

    // Stop the daemon
    daemon::stop_daemon(&name).await.ok();

    // Reinstall daemon (regenerates plist with latest config)
    write_tunnel_config(&tunnel)?;
    daemon::install_daemon(&tunnel).await?;

    // Start the daemon
    daemon::start_daemon(&name).await?;

    // Update state
    let mut state = TunnelState::load()?;
    if let Some(t) = state.find_mut(&name) {
        t.enabled = true;
    }
    state.save()?;

    println!("✓ Restarted tunnel: {}", name);
    println!("  https://{}", tunnel.hostname);

    Ok(())
}

// View logs for a tunnel
async fn cmd_logs(name: String, follow: bool, lines: usize) -> Result<()> {
    let state = TunnelState::load()?;

    let tunnel = state.find(&name).ok_or_else(|| {
        anyhow::anyhow!(
            "Tunnel '{}' not found. Run `ytunnel list` to see available tunnels.",
            name
        )
    })?;

    let log_path = tunnel.log_path()?;

    if !log_path.exists() {
        println!("No logs yet for tunnel '{}'", name);
        return Ok(());
    }

    if follow {
        // Use tail -f for following
        use std::process::Command;
        let status = Command::new("tail")
            .args(["-f", "-n", &lines.to_string()])
            .arg(&log_path)
            .status()?;

        if !status.success() {
            anyhow::bail!("Failed to tail log file");
        }
    } else {
        // Just read and print the last N lines
        let log_lines = daemon::read_log_tail(tunnel, lines)?;
        for line in log_lines {
            println!("{}", line);
        }
    }

    Ok(())
}

async fn cmd_zones_list() -> Result<()> {
    let cfg = config::load_config()?;

    println!("Available zones:");
    for zone in &cfg.zones {
        let marker = if zone.id == cfg.default_zone_id {
            " (default)"
        } else {
            ""
        };
        println!("  {}{}", zone.name, marker);
    }

    Ok(())
}

async fn cmd_zones_default(domain: String) -> Result<()> {
    let mut cfg = config::load_config()?;

    let zone = cfg.zones.iter().find(|z| z.name == domain);
    match zone {
        Some(z) => {
            cfg.default_zone_id = z.id.clone();
            cfg.default_zone_name = z.name.clone();
            config::save_config(&cfg)?;
            println!("Default zone set to: {}", domain);
        }
        None => {
            anyhow::bail!(
                "Zone '{}' not found. Run `ytunnel zones` to see available zones.",
                domain
            );
        }
    }

    Ok(())
}

async fn cmd_list() -> Result<()> {
    let state = TunnelState::load()?;

    if state.tunnels.is_empty() {
        println!("No tunnels configured.");
        println!("Add one with: ytunnel add <name> <target>");
        return Ok(());
    }

    println!("Tunnels:");
    for tunnel in &state.tunnels {
        let status = daemon::get_daemon_status(tunnel).await;
        let status_symbol = status.symbol();
        let status_text = match status {
            state::TunnelStatus::Running => "running",
            state::TunnelStatus::Stopped => "stopped",
            state::TunnelStatus::Error => "error",
        };
        println!(
            "  {} {:<12} {} -> {} ({})",
            status_symbol, tunnel.name, tunnel.hostname, tunnel.target, status_text
        );
    }

    Ok(())
}

async fn cmd_delete(name: String) -> Result<()> {
    let cfg = config::load_config()?;
    let client = cloudflare::Client::new(&cfg.api_token);

    // Handle both "name" and "ytunnel-name" formats
    let name = name.strip_prefix("ytunnel-").unwrap_or(&name).to_string();

    // Stop and uninstall daemon
    daemon::stop_daemon(&name).await.ok();
    daemon::uninstall_daemon(&name).await.ok();

    // Remove from state
    let mut state = TunnelState::load()?;
    if let Some(tunnel) = state.remove(&name) {
        // Delete from Cloudflare
        client
            .delete_tunnel(&cfg.account_id, &tunnel.tunnel_id)
            .await
            .ok();
        println!("✓ Deleted Cloudflare tunnel");

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

        state.save()?;
        println!("✓ Deleted tunnel: {}", name);
    } else {
        // Try deleting from Cloudflare directly (might be a tunnel created with `run`)
        let tunnel_name = format!("ytunnel-{}", name);
        match client
            .get_tunnel_by_name(&cfg.account_id, &tunnel_name)
            .await?
        {
            Some(t) => {
                // Delete credentials file if it exists
                if let Ok(creds_path) = t.credentials_path() {
                    std::fs::remove_file(&creds_path).ok();
                }
                client.delete_tunnel(&cfg.account_id, &t.id).await?;
                println!("✓ Deleted Cloudflare tunnel: {}", tunnel_name);
            }
            None => {
                println!("Tunnel '{}' not found.", name);
            }
        }
    }

    Ok(())
}

// Reset ytunnel configuration (allows re-initialization)
async fn cmd_reset(skip_confirm: bool) -> Result<()> {
    // Check if ytunnel is even configured
    if !config::config_path()?.exists() {
        println!("ytunnel is not configured. Nothing to reset.");
        return Ok(());
    }

    // Confirmation prompt unless -y flag
    if !skip_confirm {
        println!("This will:");
        println!("  - Stop all running tunnels");
        println!("  - Remove all tunnel configurations");
        println!("  - Delete tunnels from Cloudflare");
        println!("  - Remove ytunnel configuration");
        println!();
        println!("Are you sure? [y/N] ");

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let input = input.trim().to_lowercase();

        if input != "y" && input != "yes" {
            println!("Cancelled.");
            return Ok(());
        }
    }

    println!("Resetting ytunnel...\n");

    // Load config for Cloudflare API access
    let cfg = config::load_config().ok();
    let client = cfg.as_ref().map(|c| cloudflare::Client::new(&c.api_token));

    // Load state to get all tunnels
    let state = TunnelState::load().unwrap_or_default();

    // Stop and clean up all tunnels
    for tunnel in &state.tunnels {
        print!("Removing tunnel '{}'... ", tunnel.name);

        // Stop daemon
        daemon::stop_daemon(&tunnel.name).await.ok();

        // Uninstall daemon
        daemon::uninstall_daemon(&tunnel.name).await.ok();

        // Delete from Cloudflare
        if let (Some(cfg), Some(client)) = (&cfg, &client) {
            client
                .delete_tunnel(&cfg.account_id, &tunnel.tunnel_id)
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

        println!("done");
    }

    // Remove tunnels.toml
    if let Ok(tunnels_path) = state::tunnels_path() {
        std::fs::remove_file(&tunnels_path).ok();
    }

    // Remove config.toml
    if let Ok(config_path) = config::config_path() {
        std::fs::remove_file(&config_path).ok();
    }

    // Clean up empty directories
    if let Ok(config_dir) = config::config_dir() {
        // Remove tunnel-configs directory if empty
        let tunnel_configs_dir = config_dir.join("tunnel-configs");
        std::fs::remove_dir(&tunnel_configs_dir).ok();

        // Remove credentials directory if empty
        let credentials_dir = config_dir.join("credentials");
        std::fs::remove_dir(&credentials_dir).ok();

        // Remove logs directory if empty
        let logs_dir = config_dir.join("logs");
        std::fs::remove_dir(&logs_dir).ok();
    }

    println!("\n✓ ytunnel has been reset.");
    println!("Run `ytunnel init` to set up with new credentials.");

    Ok(())
}

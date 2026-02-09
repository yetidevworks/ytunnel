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
use cli::{AccountCommands, Cli, Commands, ZonesCommands};
use config::Account;
use state::{write_tunnel_config, PersistentTunnel, TunnelState};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let account = cli.account.as_deref();

    match cli.command {
        None => {
            // Default: open TUI
            tui::run_tui(account).await?;
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
            cmd_run(name, target, zone, account).await?;
        }
        Some(Commands::Add {
            name,
            target,
            zone,
            start,
        }) => {
            cmd_add(name, target, zone, start, account).await?;
        }
        Some(Commands::Start { name }) => {
            cmd_start(name, account).await?;
        }
        Some(Commands::Stop { name }) => {
            cmd_stop(name, account).await?;
        }
        Some(Commands::Restart { name }) => {
            cmd_restart(name, account).await?;
        }
        Some(Commands::Logs {
            name,
            follow,
            lines,
        }) => {
            cmd_logs(name, follow, lines, account).await?;
        }
        Some(Commands::Zones { command }) => match command {
            None => cmd_zones_list(account).await?,
            Some(ZonesCommands::Default { domain }) => cmd_zones_default(domain, account).await?,
        },
        Some(Commands::List) => {
            cmd_list(account).await?;
        }
        Some(Commands::Delete { name }) => {
            cmd_delete(name, account).await?;
        }
        Some(Commands::Reset { yes }) => {
            cmd_reset(yes).await?;
        }
        Some(Commands::Account { command }) => match command {
            None => cmd_account_list().await?,
            Some(AccountCommands::List) => cmd_account_list().await?,
            Some(AccountCommands::Select { name }) => cmd_account_select(name).await?,
            Some(AccountCommands::Default { name }) => cmd_account_select(name).await?,
            Some(AccountCommands::Remove { name, yes }) => cmd_account_remove(name, yes).await?,
        },
    }

    Ok(())
}

async fn cmd_init() -> Result<()> {
    // Check if cloudflared is installed (do this first for better UX)
    if !tunnel::is_cloudflared_installed().await {
        anyhow::bail!(
            "cloudflared is not installed. Please install it first:\n  \
             brew install cloudflare/cloudflare/cloudflared"
        );
    }

    // Check if already configured
    let account_name = if config::config_path()?.exists() {
        let cfg = config::load_config()?;

        // Show current accounts
        println!(
            "ytunnel is already configured with {} account(s):",
            cfg.accounts.len()
        );
        for acct in &cfg.accounts {
            let marker = if acct.name == cfg.selected_account {
                " (default)"
            } else {
                ""
            };
            println!("  - {}{}", acct.name, marker);
        }
        println!();

        // Prompt: add new or reinitialize?
        println!("What would you like to do?");
        println!("  [a] Add a new account");
        println!("  [r] Reinitialize (remove all accounts and start fresh)");
        println!("  [q] Quit");
        print!("> ");
        std::io::Write::flush(&mut std::io::stdout())?;

        let mut choice = String::new();
        std::io::stdin().read_line(&mut choice)?;

        match choice.trim().to_lowercase().as_str() {
            "a" => {
                // Continue to add account flow - prompt for name
                println!("\nEnter a name for this account (e.g., 'work', 'personal'):");
                print!("> ");
                std::io::Write::flush(&mut std::io::stdout())?;
                let mut name = String::new();
                std::io::stdin().read_line(&mut name)?;
                let name = name.trim().to_string();

                if name.is_empty() {
                    anyhow::bail!("Account name cannot be empty");
                }

                // Check for duplicate names
                if cfg.accounts.iter().any(|a| a.name == name) {
                    anyhow::bail!("Account '{}' already exists", name);
                }

                name
            }
            "r" => {
                // Confirm and reinitialize
                println!("This will remove all accounts and tunnels. Are you sure? [y/N]");
                print!("> ");
                std::io::Write::flush(&mut std::io::stdout())?;
                let mut confirm = String::new();
                std::io::stdin().read_line(&mut confirm)?;
                if confirm.trim().to_lowercase() != "y" {
                    println!("Cancelled.");
                    return Ok(());
                }
                // Reset and continue to init flow
                cmd_reset(true).await?;
                println!();

                // Prompt for account name after reset
                println!("Enter a name for this account (e.g., 'dev', 'production'):");
                print!("> ");
                std::io::Write::flush(&mut std::io::stdout())?;
                let mut name = String::new();
                std::io::stdin().read_line(&mut name)?;
                let name = name.trim().to_string();

                if name.is_empty() {
                    anyhow::bail!("Account name cannot be empty");
                }

                name
            }
            _ => {
                println!("Cancelled.");
                return Ok(());
            }
        }
    } else {
        // Fresh init - prompt for account name
        println!("Initializing ytunnel...\n");
        println!("✓ cloudflared found");

        println!("\nEnter a name for this account (e.g., 'dev', 'work', 'personal'):");
        print!("> ");
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut name = String::new();
        std::io::stdin().read_line(&mut name)?;
        let name = name.trim().to_string();

        if name.is_empty() {
            anyhow::bail!("Account name cannot be empty");
        }

        name
    };

    // Get API token
    println!("\nEnter your Cloudflare API token:");
    println!(
        "  Required permissions: Zone→Zone→Edit, Zone→DNS→Edit, Account→Cloudflare Tunnel→Edit"
    );
    print!("> ");
    std::io::Write::flush(&mut std::io::stdout())?;
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
    let cf_account_id = zones[0].account_id.clone();

    // Set default zone
    let default_zone = &zones[0];
    println!(
        "\nSetting default zone to: {} (change with `ytunnel zones default <domain>`)",
        default_zone.name
    );

    // Create the account
    let new_account = Account {
        name: account_name.clone(),
        api_token: token,
        account_id: cf_account_id,
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

    // Load existing config or create new one
    let mut cfg = if config::config_path()?.exists() {
        config::load_config()?
    } else {
        config::Config {
            selected_account: account_name.clone(),
            accounts: Vec::new(),
        }
    };

    // Add the new account
    cfg.add_account(new_account)?;

    // Ask if this should be the default (if there are multiple accounts)
    if cfg.accounts.len() > 1 && cfg.selected_account != account_name {
        println!("\nSet '{}' as the default account? [y/N]", account_name);
        print!("> ");
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut set_default = String::new();
        std::io::stdin().read_line(&mut set_default)?;
        if set_default.trim().to_lowercase() == "y" {
            cfg.select_account(&account_name)?;
            println!("Default account set to '{}'", account_name);
        }
    }

    config::save_config(&cfg)?;

    println!(
        "\n✓ Account '{}' added to {}",
        account_name,
        config::config_path()?.display()
    );
    println!("\nYou're ready! Try:");
    println!("  ytunnel                                 # open TUI dashboard");
    println!("  ytunnel add myapp localhost:3000 -s     # add and start a tunnel");
    println!("  ytunnel run localhost:3000              # ephemeral tunnel");

    Ok(())
}

// Run an ephemeral tunnel (foreground, stops on Ctrl+C)
async fn cmd_run(
    name: Option<String>,
    target: String,
    zone: Option<String>,
    account: Option<&str>,
) -> Result<()> {
    let cfg = config::load_config()?;
    let acct = cfg.get_account(account)?;
    let client = cloudflare::Client::new(&acct.api_token);

    // Determine zone
    let (zone_id, zone_name) = if let Some(z) = zone {
        // Find zone by name
        let found = acct.zones.iter().find(|zc| zc.name == z);
        match found {
            Some(zc) => (zc.id.clone(), zc.name.clone()),
            None => anyhow::bail!(
                "Zone '{}' not found. Run `ytunnel zones` to see available zones.",
                z
            ),
        }
    } else {
        (acct.default_zone_id.clone(), acct.default_zone_name.clone())
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
        .get_tunnel_by_name(&acct.account_id, &tunnel_name)
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
            let result = client.create_tunnel(&acct.account_id, &tunnel_name).await?;
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

    // Check if tunnel was imported as a managed tunnel (skip cleanup if so)
    let state = TunnelState::load()?;
    let was_imported = state.tunnels.iter().any(|t| t.tunnel_id == tunnel.id);

    if was_imported {
        println!("\nTunnel was imported as managed - keeping resources.");
    } else {
        // Clean up after tunnel stops
        println!("\nCleaning up...");

        // Delete DNS record
        if let Err(e) = client.delete_dns_record(&zone_id, &full_hostname).await {
            eprintln!("Warning: Failed to delete DNS record: {}", e);
        } else {
            println!("✓ Removed DNS record: {}", full_hostname);
        }

        // Delete tunnel from Cloudflare
        if let Err(e) = client.delete_tunnel(&acct.account_id, &tunnel.id).await {
            eprintln!("Warning: Failed to delete tunnel: {}", e);
        } else {
            println!("✓ Removed tunnel: {}", tunnel_name);
        }

        // Delete local credentials file
        if credentials_path.exists() {
            if let Err(e) = std::fs::remove_file(&credentials_path) {
                eprintln!("Warning: Failed to delete credentials file: {}", e);
            }
        }
    }

    Ok(())
}

// Add a persistent tunnel (non-interactive CLI command)
async fn cmd_add(
    name: String,
    target: String,
    zone: Option<String>,
    start: bool,
    account: Option<&str>,
) -> Result<()> {
    let cfg = config::load_config()?;
    let acct = cfg.get_account(account)?;
    let client = cloudflare::Client::new(&acct.api_token);
    let account_name = acct.name.clone();

    // Check if tunnel already exists in state for this account
    let state = TunnelState::load()?;
    if state.find_for_account(&name, &account_name).is_some() {
        anyhow::bail!(
            "Tunnel '{}' already exists for account '{}'. Use `ytunnel delete {}` first.",
            name,
            account_name,
            name
        );
    }

    // Determine zone
    let (zone_id, zone_name) = if let Some(z) = zone {
        let found = acct.zones.iter().find(|zc| zc.name == z);
        match found {
            Some(zc) => (zc.id.clone(), zc.name.clone()),
            None => anyhow::bail!(
                "Zone '{}' not found. Run `ytunnel zones` to see available zones.",
                z
            ),
        }
    } else {
        (acct.default_zone_id.clone(), acct.default_zone_name.clone())
    };

    let tunnel_name = format!("ytunnel-{}", name);
    let hostname = format!("{}.{}", name, zone_name);

    println!("Adding tunnel: {} -> {}", hostname, target);

    // Check if tunnel exists in Cloudflare, create if not
    let (cf_tunnel, _credentials_path) = match client
        .get_tunnel_by_name(&acct.account_id, &tunnel_name)
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
            let result = client.create_tunnel(&acct.account_id, &tunnel_name).await?;
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
        account_name: account_name.clone(),
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
        daemon::start_daemon(&name, &account_name).await?;
        println!("✓ Tunnel started");
        println!("\nTunnel running: https://{}", hostname);
    } else {
        println!("\nTunnel added. Start with: ytunnel start {}", name);
    }

    Ok(())
}

// Start a stopped tunnel
async fn cmd_start(name: String, account: Option<&str>) -> Result<()> {
    let cfg = config::load_config()?;
    let acct = cfg.get_account(account)?;
    let account_name = acct.name.clone();
    let client = cloudflare::Client::new(&acct.api_token);
    let mut state = TunnelState::load()?;

    // Get tunnel info and hostname before mutable borrow
    let (hostname, tunnel_clone) = {
        let tunnel = state.find_for_account(&name, &account_name).ok_or_else(|| {
            anyhow::anyhow!(
                "Tunnel '{}' not found for account '{}'. Run `ytunnel list` to see available tunnels.",
                name,
                account_name
            )
        })?;
        (tunnel.hostname.clone(), tunnel.clone())
    };

    // Use the tunnel's own account_name for daemon operations (handles legacy tunnels)
    let tunnel_account = &tunnel_clone.account_name;

    // Ensure DNS record exists (recreates if manually deleted)
    client
        .ensure_dns_record(&tunnel_clone.zone_id, &hostname, &tunnel_clone.tunnel_id)
        .await?;

    // Ensure config file exists
    write_tunnel_config(&tunnel_clone)?;

    // Ensure daemon is installed
    daemon::install_daemon(&tunnel_clone).await?;

    // Start the daemon
    daemon::start_daemon(&name, tunnel_account).await?;

    // Update state
    if let Some(t) = state.find_for_account_mut(&name, &account_name) {
        t.enabled = true;
    }
    state.save()?;

    println!("✓ Started tunnel: {}", name);
    println!("  https://{}", hostname);

    Ok(())
}

// Stop a running tunnel
async fn cmd_stop(name: String, account: Option<&str>) -> Result<()> {
    let cfg = config::load_config()?;
    let account_name = cfg.get_account(account)?.name.clone();
    let mut state = TunnelState::load()?;

    // Get tunnel info before mutable borrow
    let (hostname, tunnel_account) = {
        let tunnel = state.find_for_account(&name, &account_name).ok_or_else(|| {
            anyhow::anyhow!(
                "Tunnel '{}' not found for account '{}'. Run `ytunnel list` to see available tunnels.",
                name,
                account_name
            )
        })?;
        (tunnel.hostname.clone(), tunnel.account_name.clone())
    };

    // Use the tunnel's own account_name for daemon operations (handles legacy tunnels)
    daemon::stop_daemon(&name, &tunnel_account).await?;

    // Update state
    if let Some(t) = state.find_for_account_mut(&name, &account_name) {
        t.enabled = false;
    }
    state.save()?;

    println!("✓ Stopped tunnel: {}", name);
    println!("  {}", hostname);

    Ok(())
}

// Restart a running tunnel (stop, reinstall daemon config, start)
async fn cmd_restart(name: String, account: Option<&str>) -> Result<()> {
    let cfg = config::load_config()?;
    let acct = cfg.get_account(account)?;
    let account_name = acct.name.clone();
    let client = cloudflare::Client::new(&acct.api_token);
    let state = TunnelState::load()?;

    let tunnel = state
        .find_for_account(&name, &account_name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Tunnel '{}' not found for account '{}'. Run `ytunnel list` to see available tunnels.",
                name,
                account_name
            )
        })?
        .clone();

    // Use the tunnel's own account_name for daemon operations (handles legacy tunnels)
    let tunnel_account = &tunnel.account_name;

    println!("Restarting tunnel: {}", name);

    // Stop the daemon
    daemon::stop_daemon(&name, tunnel_account).await.ok();

    // Ensure DNS record exists (recreates if manually deleted)
    client
        .ensure_dns_record(&tunnel.zone_id, &tunnel.hostname, &tunnel.tunnel_id)
        .await?;

    // Reinstall daemon (regenerates plist with latest config)
    write_tunnel_config(&tunnel)?;
    daemon::install_daemon(&tunnel).await?;

    // Start the daemon
    daemon::start_daemon(&name, tunnel_account).await?;

    // Update state
    let mut state = TunnelState::load()?;
    if let Some(t) = state.find_for_account_mut(&name, &account_name) {
        t.enabled = true;
    }
    state.save()?;

    println!("✓ Restarted tunnel: {}", name);
    println!("  https://{}", tunnel.hostname);

    Ok(())
}

// View logs for a tunnel
async fn cmd_logs(name: String, follow: bool, lines: usize, account: Option<&str>) -> Result<()> {
    let cfg = config::load_config()?;
    let account_name = cfg.get_account(account)?.name.clone();
    let state = TunnelState::load()?;

    let tunnel = state
        .find_for_account(&name, &account_name)
        .ok_or_else(|| {
            anyhow::anyhow!(
            "Tunnel '{}' not found for account '{}'. Run `ytunnel list` to see available tunnels.",
            name,
            account_name
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

async fn cmd_zones_list(account: Option<&str>) -> Result<()> {
    let cfg = config::load_config()?;
    let acct = cfg.get_account(account)?;

    println!("Available zones for account '{}':", acct.name);
    for zone in &acct.zones {
        let marker = if zone.id == acct.default_zone_id {
            " (default)"
        } else {
            ""
        };
        println!("  {}{}", zone.name, marker);
    }

    Ok(())
}

async fn cmd_zones_default(domain: String, account: Option<&str>) -> Result<()> {
    let mut cfg = config::load_config()?;
    let acct = cfg.get_account_mut(account)?;

    let zone = acct.zones.iter().find(|z| z.name == domain).cloned();
    match zone {
        Some(z) => {
            acct.default_zone_id = z.id.clone();
            acct.default_zone_name = z.name.clone();
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

async fn cmd_list(account: Option<&str>) -> Result<()> {
    let cfg = config::load_config()?;
    let account_name = cfg.get_account(account)?.name.clone();
    let state = TunnelState::load()?;

    let tunnels: Vec<_> = state.tunnels_for_account(&account_name);

    if tunnels.is_empty() {
        println!("No tunnels configured for account '{}'.", account_name);
        println!("Add one with: ytunnel add <name> <target>");
        return Ok(());
    }

    println!("Tunnels for account '{}':", account_name);
    for tunnel in tunnels {
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

async fn cmd_delete(name: String, account: Option<&str>) -> Result<()> {
    let cfg = config::load_config()?;
    let acct = cfg.get_account(account)?;
    let account_name = acct.name.clone();
    let client = cloudflare::Client::new(&acct.api_token);

    // Handle both "name" and "ytunnel-name" formats
    let name = name.strip_prefix("ytunnel-").unwrap_or(&name).to_string();

    // Get the tunnel's own account_name for daemon operations (handles legacy tunnels)
    let state = TunnelState::load()?;
    let tunnel_account = state
        .find_for_account(&name, &account_name)
        .map(|t| t.account_name.clone())
        .unwrap_or_default();

    // Stop and uninstall daemon
    daemon::stop_daemon(&name, &tunnel_account).await.ok();
    daemon::uninstall_daemon(&name, &tunnel_account).await.ok();

    // Remove from state
    let mut state = TunnelState::load()?;
    if let Some(tunnel) = state.remove_for_account(&name, &account_name) {
        // Delete the DNS CNAME record
        client
            .delete_dns_record(&tunnel.zone_id, &tunnel.hostname)
            .await
            .ok();
        println!("✓ Deleted DNS record");

        // Delete from Cloudflare
        client
            .delete_tunnel(&acct.account_id, &tunnel.tunnel_id)
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
            .get_tunnel_by_name(&acct.account_id, &tunnel_name)
            .await?
        {
            Some(t) => {
                // Delete credentials file if it exists
                if let Ok(creds_path) = t.credentials_path() {
                    std::fs::remove_file(&creds_path).ok();
                }
                client.delete_tunnel(&acct.account_id, &t.id).await?;
                println!("✓ Deleted Cloudflare tunnel: {}", tunnel_name);
            }
            None => {
                println!(
                    "Tunnel '{}' not found for account '{}'.",
                    name, account_name
                );
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

    // Load state to get all tunnels
    let state = TunnelState::load().unwrap_or_default();

    // Stop and clean up all tunnels
    for tunnel in &state.tunnels {
        print!("Removing tunnel '{}'... ", tunnel.name);

        // Stop daemon (use tunnel's account_name, fallback to default for migrated tunnels)
        let acct_name = if tunnel.account_name.is_empty() {
            cfg.as_ref()
                .map(|c| c.selected_account.clone())
                .unwrap_or_default()
        } else {
            tunnel.account_name.clone()
        };
        daemon::stop_daemon(&tunnel.name, &acct_name).await.ok();

        // Uninstall daemon
        daemon::uninstall_daemon(&tunnel.name, &acct_name)
            .await
            .ok();

        // Delete from Cloudflare - find the right account
        if let Some(cfg) = &cfg {
            let acct = if tunnel.account_name.is_empty() {
                cfg.get_account(None).ok()
            } else {
                cfg.accounts.iter().find(|a| a.name == tunnel.account_name)
            };
            if let Some(acct) = acct {
                let client = cloudflare::Client::new(&acct.api_token);
                client
                    .delete_tunnel(&acct.account_id, &tunnel.tunnel_id)
                    .await
                    .ok();
            }
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

// List all configured accounts
async fn cmd_account_list() -> Result<()> {
    let cfg = config::load_config()?;

    if cfg.accounts.is_empty() {
        println!("No accounts configured.");
        println!("Run `ytunnel init` to add an account.");
        return Ok(());
    }

    println!("Configured accounts:");
    for acct in &cfg.accounts {
        let marker = if acct.name == cfg.selected_account {
            " (default)"
        } else {
            ""
        };
        println!("  {} - {} zones{}", acct.name, acct.zones.len(), marker);
        for zone in &acct.zones {
            let zone_marker = if zone.id == acct.default_zone_id {
                " (default)"
            } else {
                ""
            };
            println!("      - {}{}", zone.name, zone_marker);
        }
    }

    Ok(())
}

// Set the default account
async fn cmd_account_select(name: String) -> Result<()> {
    let mut cfg = config::load_config()?;
    cfg.select_account(&name)?;
    config::save_config(&cfg)?;
    println!("Default account set to: {}", name);
    Ok(())
}

// Remove an account
async fn cmd_account_remove(name: String, skip_confirm: bool) -> Result<()> {
    let mut cfg = config::load_config()?;

    // Check if account exists
    if !cfg.accounts.iter().any(|a| a.name == name) {
        anyhow::bail!(
            "Account '{}' not found. Run `ytunnel account list` to see available accounts.",
            name
        );
    }

    // Check if this is the last account
    if cfg.accounts.len() == 1 {
        anyhow::bail!(
            "Cannot remove the last account. Use `ytunnel reset` to remove all configuration."
        );
    }

    // Get tunnel count for this account
    let state = TunnelState::load()?;
    let tunnel_count = state.tunnels_for_account(&name).len();

    // Confirmation prompt unless -y flag
    if !skip_confirm {
        if tunnel_count > 0 {
            println!(
                "Account '{}' has {} tunnel(s). Removing the account will also delete these tunnels.",
                name, tunnel_count
            );
        }
        println!("Are you sure you want to remove account '{}'? [y/N]", name);
        print!("> ");
        std::io::Write::flush(&mut std::io::stdout())?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let input = input.trim().to_lowercase();

        if input != "y" && input != "yes" {
            println!("Cancelled.");
            return Ok(());
        }
    }

    // Remove tunnels for this account
    if tunnel_count > 0 {
        let acct = cfg.accounts.iter().find(|a| a.name == name).unwrap();
        let client = cloudflare::Client::new(&acct.api_token);
        let mut state = TunnelState::load()?;

        // Collect tunnels to remove
        let tunnels_to_remove: Vec<_> = state
            .tunnels
            .iter()
            .filter(|t| t.account_name == name)
            .cloned()
            .collect();

        for tunnel in tunnels_to_remove {
            print!("Removing tunnel '{}'... ", tunnel.name);

            // Stop and uninstall daemon
            daemon::stop_daemon(&tunnel.name, &name).await.ok();
            daemon::uninstall_daemon(&tunnel.name, &name).await.ok();

            // Delete from Cloudflare
            client
                .delete_tunnel(&acct.account_id, &tunnel.tunnel_id)
                .await
                .ok();

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

            // Remove from state
            state.remove_for_account(&tunnel.name, &name);

            println!("done");
        }

        state.save()?;
    }

    // Remove the account
    cfg.remove_account(&name)?;
    config::save_config(&cfg)?;

    println!("✓ Removed account: {}", name);
    println!("Default account is now: {}", cfg.selected_account);

    Ok(())
}

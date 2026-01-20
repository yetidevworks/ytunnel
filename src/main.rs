mod cli;
mod cloudflare;
mod config;
mod tunnel;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands, ZonesCommands};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init => {
            cmd_init().await?;
        }
        Commands::Run { args, zone } => {
            // Parse args: if 1 arg it's target, if 2 args it's name + target
            let (name, target) = if args.len() == 2 {
                (Some(args[0].clone()), args[1].clone())
            } else {
                (None, args[0].clone())
            };
            cmd_run(name, target, zone).await?;
        }
        Commands::Zones { command } => match command {
            None => cmd_zones_list().await?,
            Some(ZonesCommands::Default { domain }) => cmd_zones_default(domain).await?,
        },
        Commands::List => {
            cmd_list().await?;
        }
        Commands::Delete { name } => {
            cmd_delete(name).await?;
        }
    }

    Ok(())
}

async fn cmd_init() -> Result<()> {
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
    println!("  ytunnel run localhost:3000              # auto-generated subdomain");
    println!(
        "  ytunnel run myapp localhost:3000        # myapp.{}",
        cfg.default_zone_name
    );

    Ok(())
}

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
    let cfg = config::load_config()?;
    let client = cloudflare::Client::new(&cfg.api_token);

    let tunnels = client.list_tunnels(&cfg.account_id).await?;
    let ytunnels: Vec<_> = tunnels
        .iter()
        .filter(|t| t.name.starts_with("ytunnel-"))
        .collect();

    if ytunnels.is_empty() {
        println!("No ytunnel tunnels found.");
    } else {
        println!("Tunnels:");
        for t in ytunnels {
            let status = if t.deleted_at.is_some() {
                "deleted"
            } else {
                "active"
            };
            println!("  {} ({})", t.name, status);
        }
    }

    Ok(())
}

async fn cmd_delete(name: String) -> Result<()> {
    let cfg = config::load_config()?;
    let client = cloudflare::Client::new(&cfg.api_token);

    let tunnel_name = if name.starts_with("ytunnel-") {
        name
    } else {
        format!("ytunnel-{}", name)
    };

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
            println!("Deleted tunnel: {}", tunnel_name);
        }
        None => {
            println!("Tunnel '{}' not found.", tunnel_name);
        }
    }

    Ok(())
}

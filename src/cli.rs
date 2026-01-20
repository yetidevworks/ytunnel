use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ytunnel")]
#[command(about = "Simple Cloudflare Tunnel CLI for custom domains", long_about = None)]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize ytunnel with your Cloudflare API token
    Init,

    /// Create and run a tunnel
    ///
    /// Examples:
    ///   ytunnel run localhost:3000                    # auto-generated subdomain
    ///   ytunnel run myapp localhost:3000              # myapp.<default-zone>
    ///   ytunnel run api -z dev.example.com localhost:8080
    Run {
        /// Subdomain name and target. If one argument: target only (auto-generated name).
        /// If two arguments: name and target.
        #[arg(required = true, num_args = 1..=2)]
        args: Vec<String>,

        /// Zone/domain to use (overrides default)
        #[arg(short, long)]
        zone: Option<String>,
    },

    /// Manage zones/domains
    Zones {
        #[command(subcommand)]
        command: Option<ZonesCommands>,
    },

    /// List all ytunnel tunnels
    List,

    /// Delete a tunnel
    Delete {
        /// Tunnel name (with or without "ytunnel-" prefix)
        name: String,
    },
}

#[derive(Subcommand)]
pub enum ZonesCommands {
    /// Set the default zone
    Default {
        /// Domain name to set as default
        domain: String,
    },
}

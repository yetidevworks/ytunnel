use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ytunnel")]
#[command(about = "Simple Cloudflare Tunnel CLI for custom domains", long_about = None)]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize ytunnel with your Cloudflare API token
    Init,

    /// Create and run an ephemeral tunnel (foreground, stops on Ctrl+C)
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

    /// Add a persistent tunnel (non-interactive)
    ///
    /// Examples:
    ///   ytunnel add myapp localhost:3000
    ///   ytunnel add api localhost:8080 -z dev.example.com
    Add {
        /// Tunnel name (subdomain part)
        name: String,

        /// Target service (e.g., localhost:3000)
        target: String,

        /// Zone/domain to use (overrides default)
        #[arg(short, long)]
        zone: Option<String>,

        /// Start the tunnel immediately after adding
        #[arg(short, long)]
        start: bool,
    },

    /// Start a stopped tunnel
    Start {
        /// Tunnel name
        name: String,
    },

    /// Stop a running tunnel
    Stop {
        /// Tunnel name
        name: String,
    },

    /// Restart a tunnel (stop, update config, start)
    Restart {
        /// Tunnel name
        name: String,
    },

    /// View logs for a tunnel
    Logs {
        /// Tunnel name
        name: String,

        /// Follow log output (like tail -f)
        #[arg(short, long)]
        follow: bool,

        /// Number of lines to show (default: 50)
        #[arg(short, long, default_value = "50")]
        lines: usize,
    },

    /// Manage zones/domains
    Zones {
        #[command(subcommand)]
        command: Option<ZonesCommands>,
    },

    /// List all tunnels (for scripting)
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

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ytunnel")]
#[command(about = "Simple Cloudflare Tunnel CLI for custom domains", long_about = None)]
#[command(version)]
pub struct Cli {
    // Use a specific account (overrides the default selected account)
    #[arg(long, global = true)]
    pub account: Option<String>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    // Initialize ytunnel with your Cloudflare API token
    Init,

    // Create and run an ephemeral tunnel (foreground, stops on Ctrl+C)
    //
    // Examples:
    //   ytunnel run localhost:3000                    # auto-generated subdomain
    //   ytunnel run myapp localhost:3000              # myapp.<default-zone>
    //   ytunnel run api -z dev.example.com localhost:8080
    Run {
        // Subdomain name and target. If one argument: target only (auto-generated name).
        // If two arguments: name and target.
        #[arg(required = true, num_args = 1..=2)]
        args: Vec<String>,

        // Zone/domain to use (overrides default)
        #[arg(short, long)]
        zone: Option<String>,
    },

    // Add a persistent tunnel (non-interactive)
    //
    // Examples:
    //   ytunnel add myapp localhost:3000
    //   ytunnel add api localhost:8080 -z dev.example.com
    Add {
        // Tunnel name (subdomain part)
        name: String,

        // Target service (e.g., localhost:3000)
        target: String,

        // Zone/domain to use (overrides default)
        #[arg(short, long)]
        zone: Option<String>,

        // Start the tunnel immediately after adding
        #[arg(short, long)]
        start: bool,
    },

    // Start a stopped tunnel
    Start {
        // Tunnel name
        name: String,
    },

    // Stop a running tunnel
    Stop {
        // Tunnel name
        name: String,
    },

    // Restart a tunnel (stop, update config, start)
    Restart {
        // Tunnel name
        name: String,
    },

    // View logs for a tunnel
    Logs {
        // Tunnel name
        name: String,

        // Follow log output (like tail -f)
        #[arg(short, long)]
        follow: bool,

        // Number of lines to show (default: 50)
        #[arg(short, long, default_value = "50")]
        lines: usize,
    },

    // Manage zones/domains
    Zones {
        #[command(subcommand)]
        command: Option<ZonesCommands>,
    },

    // List all tunnels (for scripting)
    List,

    // Delete a tunnel
    Delete {
        // Tunnel name (with or without "ytunnel-" prefix)
        name: String,
    },

    // Reset ytunnel configuration (allows re-initializing with new credentials)
    Reset {
        // Skip confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },

    // Manage Cloudflare accounts
    Account {
        #[command(subcommand)]
        command: Option<AccountCommands>,
    },
}

#[derive(Subcommand)]
pub enum AccountCommands {
    // List all configured accounts
    List,

    // Set the default account
    Select {
        // Account name to select as default
        name: String,
    },

    // Alias for select - set the default account
    Default {
        // Account name to select as default
        name: String,
    },

    // Remove an account
    Remove {
        // Account name to remove
        name: String,

        // Skip confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub enum ZonesCommands {
    // Set the default zone
    Default {
        // Domain name to set as default
        domain: String,
    },
}

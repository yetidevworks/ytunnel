# YTunnel

╔══════════════════════════════════════════════════╗
║   ___ ___ _______                           __   ║
║  |   |   |_     _|.--.--.-----.-----.-----.|  |  ║
║   \     /  |   |  |  |  |     |     |  -__||  |  ║
║    |___|   |___|  |_____|__|__|__|__|_____||__|  ║
║                                                  ║
╚══════════════════════════════════════════════════╝

A TUI-first CLI for managing Cloudflare Tunnels with custom domains. Think ngrok, but using your own Cloudflare domain with persistent URLs and a dashboard to manage them.

## Features

- **TUI Dashboard** - Interactive interface to manage all your tunnels
- **Persistent tunnels** - Tunnels run as background daemons (via launchd on macOS)
- **Automatic DNS management** - Creates and updates CNAME records automatically
- **Multi-zone support** - Use different domains for different tunnels
- **SSL/HTTPS** - Automatic via Cloudflare
- **Ephemeral mode** - Quick one-off tunnels that stop when you exit

## Prerequisites

1. **cloudflared** - Cloudflare's tunnel daemon
   ```bash
   brew install cloudflare/cloudflare/cloudflared
   ```

2. **Cloudflare API Token** with these permissions:
   - Zone:Read
   - DNS:Edit
   - Account > Cloudflare Tunnel:Edit

   Create one at: https://dash.cloudflare.com/profile/api-tokens

3. **A domain** managed by Cloudflare

## Installation

```bash
cargo install --path .
```

## Quick Start

```bash
# First-time setup
ytunnel init

# Open the TUI dashboard
ytunnel

# Or add a tunnel directly from CLI
ytunnel add myapp localhost:3000 --start
```

## TUI Dashboard

Run `ytunnel` with no arguments to open the interactive dashboard:

```
┌─ ytunnel ──────────────────────────────────────────────────────┐
│ Tunnels                          │ Logs: myapp                 │
│ ─────────────────────────────────│─────────────────────────────│
│ ● myapp    myapp.example.com     │ 2024-01-20 10:30:21 INF ... │
│ ○ api      api.example.com       │ 2024-01-20 10:30:22 INF ... │
│   staging  staging.example.com   │ 2024-01-20 10:30:23 INF ... │
│                                  │                             │
├──────────────────────────────────┴─────────────────────────────┤
│ [a]dd  [s]tart  [S]top  [d]elete  [r]efresh  [q]uit           │
└────────────────────────────────────────────────────────────────┘
```

**Status indicators:**
- `●` Running (green)
- `○` Stopped (yellow)
- `✗` Error (red)

**Keyboard shortcuts:**
- `a` - Add a new tunnel
- `s` - Start selected tunnel
- `S` - Stop selected tunnel
- `d` - Delete selected tunnel
- `m` - Import ephemeral tunnel as managed
- `r` - Refresh status
- `↑/↓` or `j/k` - Navigate list
- `q` - Quit

Tunnels continue running in the background after you close the TUI.

**Ephemeral tunnels** (created with `ytunnel run`) also appear in the TUI marked as `[ephemeral]`. You can:
- View them alongside managed tunnels
- Delete them from Cloudflare
- Import them as managed tunnels (press `m`) to add daemon control

## CLI Commands

### Persistent Tunnels

```bash
# Add a tunnel (doesn't start it)
ytunnel add myapp localhost:3000

# Add and start immediately
ytunnel add myapp localhost:3000 --start

# Use a specific zone
ytunnel add api localhost:8080 -z dev.example.com

# Start/stop tunnels
ytunnel start myapp
ytunnel stop myapp

# List all tunnels with status
ytunnel list

# Delete a tunnel
ytunnel delete myapp
```

### Ephemeral Tunnels

For quick one-off tunnels that stop when you press Ctrl+C:

```bash
# Auto-generated subdomain (ytunnel-abc123.example.com)
ytunnel run localhost:3000

# Named subdomain (license.example.com)
ytunnel run license localhost:3000

# Nested subdomain (license.tunnel.example.com)
ytunnel run license.tunnel localhost:3000

# Different zone
ytunnel run api -z dev.example.com localhost:8080
```

### Zone Management

```bash
# List available zones
ytunnel zones

# Change default zone
ytunnel zones default dev.example.com
```

## How It Works

### Persistent Tunnels

When you add a tunnel with `ytunnel add myapp localhost:3000 --start`:

1. Creates (or reuses) a named Cloudflare tunnel via API
2. Saves tunnel credentials to `~/.config/ytunnel/<tunnel-id>.json`
3. Creates/updates a CNAME DNS record pointing to the tunnel
4. Generates a tunnel config at `~/.config/ytunnel/tunnel-configs/myapp.yml`
5. Installs a launchd plist at `~/Library/LaunchAgents/com.ytunnel.myapp.plist`
6. Starts the tunnel daemon

The tunnel continues running in the background, survives terminal closes, and can be configured to start on login.

### Ephemeral Tunnels

When you run `ytunnel run myapp localhost:3000`:

1. Creates (or reuses) a named Cloudflare tunnel
2. Creates/updates the DNS record
3. Runs `cloudflared` in the foreground
4. Stops when you press Ctrl+C (tunnel remains registered for reuse)

## Configuration

### Main Config

`~/.config/ytunnel/config.toml`:

```toml
api_token = "your-token"
account_id = "your-account-id"
default_zone_id = "zone-id"
default_zone_name = "example.com"

[[zones]]
id = "zone-id"
name = "example.com"
```

### Tunnel State

`~/.config/ytunnel/tunnels.toml`:

```toml
[[tunnels]]
name = "myapp"
target = "localhost:3000"
zone_id = "abc123"
zone_name = "example.com"
hostname = "myapp.example.com"
tunnel_id = "cf-tunnel-id"
enabled = true
```

### File Locations

| Path | Purpose |
|------|---------|
| `~/.config/ytunnel/config.toml` | API credentials and zones |
| `~/.config/ytunnel/tunnels.toml` | Persistent tunnel state |
| `~/.config/ytunnel/<tunnel-id>.json` | Cloudflare tunnel credentials |
| `~/.config/ytunnel/tunnel-configs/<name>.yml` | cloudflared config files |
| `~/.config/ytunnel/logs/<name>.log` | Tunnel daemon logs |
| `~/Library/LaunchAgents/com.ytunnel.<name>.plist` | launchd service files |

## Troubleshooting

### Check tunnel status

```bash
# Via CLI
ytunnel list

# Via launchctl
launchctl list | grep ytunnel
```

### View logs

```bash
# In TUI: select tunnel and view right pane

# Or directly
tail -f ~/.config/ytunnel/logs/myapp.log
```

### Manually stop a tunnel

```bash
launchctl unload ~/Library/LaunchAgents/com.ytunnel.myapp.plist
```

## License

MIT

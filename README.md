# YTunnel

```
  ___ ___ _______                           __
 |   |   |_     _|.--.--.-----.-----.-----.|  |
  \     /  |   |  |  |  |     |     |  -__||  |
   |___|   |___|  |_____|__|__|__|__|_____||__|
   Cloudflare tunnels made easy!

```

A TUI-first CLI for managing Cloudflare Tunnels with custom domains. Think ngrok, but using your own Cloudflare domain with persistent URLs and a dashboard to manage them.

## Features

- **TUI Dashboard** - Interactive interface to manage all your tunnels
- **Live Metrics** - Real-time request counts, error rates, and connection status
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
   > **Note:** You only need to *install* cloudflared. Do NOT run it as a brew service (`brew services start cloudflared`). YTunnel manages cloudflared processes directly.

2. **Cloudflare API Token** with these permissions:
   - Zone:Read
   - DNS:Edit
   - Account > Cloudflare Tunnel:Edit

   Create one at: https://dash.cloudflare.com/profile/api-tokens

3. **A domain** managed by Cloudflare (free tier works)

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

## Architecture

### How YTunnel Works

YTunnel is a **management tool**, not a daemon itself. Here's how the pieces fit together:

```
┌─────────────────────────────────────────────────────────────────┐
│                         ytunnel (CLI/TUI)                       │
│         Management tool - runs only when you invoke it          │
└─────────────────────────────────────────────────────────────────┘
                                  │
                    Creates & manages configs for
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────┐
│                      launchd (macOS)                            │
│              System service manager - always running            │
│                                                                 │
│   Manages plists in ~/Library/LaunchAgents/:                    │
│   • com.ytunnel.myapp.plist                                     │
│   • com.ytunnel.api.plist                                       │
└─────────────────────────────────────────────────────────────────┘
                                  │
                    Starts/stops/monitors
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────┐
│                    cloudflared processes                        │
│            One process per tunnel - runs in background          │
│                                                                 │
│   • cloudflared tunnel --config myapp.yml run                   │
│   • cloudflared tunnel --config api.yml run                     │
└─────────────────────────────────────────────────────────────────┘
                                  │
                         Connects to
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────┐
│                      Cloudflare Edge                            │
│                  Routes traffic to your tunnels                 │
└─────────────────────────────────────────────────────────────────┘
```

### Persistence Model

| Mode | How it runs | Survives reboot? | Use case |
|------|-------------|------------------|----------|
| **Ephemeral** (`ytunnel run`) | Foreground process | No | Quick testing, one-off tunnels |
| **Persistent** (`ytunnel add --start`) | launchd daemon | Yes* | Production, always-on services |

*Tunnels are configured with `RunAtLoad: false` by default. They start when you run `ytunnel start` and keep running until you `ytunnel stop` or reboot. To auto-start on login, you can modify the plist.

### What YTunnel Creates

When you run `ytunnel add myapp localhost:3000 --start`:

1. **Cloudflare Tunnel** - Created via API, persists in your Cloudflare account
2. **DNS Record** - CNAME pointing `myapp.yourdomain.com` → tunnel
3. **Credentials** - `~/Library/Application Support/ytunnel/<tunnel-id>.json`
4. **Config** - `~/Library/Application Support/ytunnel/tunnel-configs/myapp.yml`
5. **Plist** - `~/Library/LaunchAgents/com.ytunnel.myapp.plist`
6. **State** - Entry in `~/Library/Application Support/ytunnel/tunnels.toml`

The plist tells launchd to run cloudflared with your config. Logs go to the logs directory.

## TUI Dashboard

Run `ytunnel` with no arguments to open the interactive dashboard:

```
┌─ Tunnels (3) ─────────────────────┬─ Logs: myapp ─────────────────────────────────┐
│ ● myapp       myapp.example.com   │ 2024-01-20 10:30:15 INF Starting tunnel       │
│ ● api         api.example.com     │ 2024-01-20 10:30:16 INF Connection registered │
│ ○ staging     staging.example.com │ 2024-01-20 10:30:17 INF Tunnel connected      │
│                                   │ 2024-01-20 10:30:18 INF Route propagated      │
│                                   │ 2024-01-20 10:30:21 INF Request served GET /  │
│                                   ├─ Metrics ─────────────────────────────────────┤
│                                   │ Requests: 1,247  Errors: 3  Active: 2         │
│                                   │ Health: ✓ healthy                             │
│                                   │ HA Connections: 4    Edge: dfw08, den01       │
│                                   │ Status Codes: 200:1198  304:42  404:3  500:4  │
│                                   │ Traffic: ▁▂▃▅▆▄▃▂▁▂▃▄▅▆▇█▆▅▄▃▂▁▂▃▄▅▆▇         │
├───────────────────────────────────┴───────────────────────────────────────────────┤
│ Started myapp                                                                     │
│ [a]dd [s]tart [S]top [R]estart [c]opy [o]pen [h]ealth [d]elete [r]efresh [q]uit   │
└───────────────────────────────────────────────────────────────────────────────────┘
```

**Status indicators:**
- `●` Running (green)
- `○` Stopped (yellow)
- `✗` Error (red)

**Keyboard shortcuts:**
| Key | Action |
|-----|--------|
| `a` | Add a new tunnel |
| `s` | Start selected tunnel |
| `S` | Stop selected tunnel |
| `R` | Restart tunnel (updates daemon config) |
| `c` | Copy tunnel URL to clipboard |
| `o` | Open tunnel URL in browser |
| `h` | Check tunnel health |
| `d` | Delete selected tunnel |
| `m` | Import ephemeral tunnel as managed |
| `r` | Refresh status |
| `↑/↓` or `j/k` | Navigate list |
| `q` | Quit |

Tunnels continue running in the background after you close the TUI.

### Metrics Panel

For running tunnels, the TUI displays live metrics from cloudflared's Prometheus endpoint:

- **Requests** - Total requests handled by the tunnel
- **Errors** - Number of failed requests (red if > 0)
- **Active** - Currently in-flight concurrent requests
- **Health** - Whether the tunnel URL is reachable (✓ healthy / ✗ unreachable)
- **HA Connections** - Number of connections to Cloudflare edge (4 = healthy)
- **Edge** - Cloudflare edge locations (e.g., `dfw08` = Dallas)
- **Status Codes** - Breakdown of HTTP response codes
- **Traffic** - Sparkline showing request rate over time

Metrics auto-refresh every 5 seconds. Health checks run every 30 seconds. Use `h` for immediate health check.

### Notifications

When a tunnel goes down or comes back up, ytunnel sends a system notification (on macOS via `terminal-notifier` or `osascript`). This helps you catch issues even when the TUI isn't visible.

### Ephemeral Tunnels

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

# Start/stop/restart tunnels
ytunnel start myapp
ytunnel stop myapp
ytunnel restart myapp    # Stop, update config, start

# View logs
ytunnel logs myapp           # Last 50 lines
ytunnel logs myapp -n 100    # Last 100 lines
ytunnel logs myapp -f        # Follow (like tail -f)

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

# Named subdomain (myapp.example.com)
ytunnel run myapp localhost:3000

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

## Configuration

### File Locations (macOS)

| Path | Purpose |
|------|---------|
| `~/Library/Application Support/ytunnel/config.toml` | API credentials and zones |
| `~/Library/Application Support/ytunnel/tunnels.toml` | Persistent tunnel state |
| `~/Library/Application Support/ytunnel/<tunnel-id>.json` | Cloudflare tunnel credentials |
| `~/Library/Application Support/ytunnel/tunnel-configs/<name>.yml` | cloudflared config files |
| `~/Library/Application Support/ytunnel/logs/<name>.log` | Tunnel daemon logs |
| `~/Library/LaunchAgents/com.ytunnel.<name>.plist` | launchd service files |

### Main Config

`~/Library/Application Support/ytunnel/config.toml`:

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

`~/Library/Application Support/ytunnel/tunnels.toml`:

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

## Troubleshooting

### Check tunnel status

```bash
# Via ytunnel
ytunnel list

# Via launchctl
launchctl list | grep ytunnel
```

### View logs

```bash
# In TUI: select tunnel and view right pane

# Or directly
tail -f ~/Library/Application\ Support/ytunnel/logs/myapp.log
```

### Tunnel won't start

1. Check if cloudflared is installed: `cloudflared --version`
2. Check the log file for errors
3. Verify credentials exist: `ls ~/Library/Application\ Support/ytunnel/*.json`
4. Try running manually: `cloudflared tunnel --config <config-path> run`

### Manually manage a tunnel

```bash
# Stop
launchctl unload ~/Library/LaunchAgents/com.ytunnel.myapp.plist

# Start
launchctl load ~/Library/LaunchAgents/com.ytunnel.myapp.plist

# Remove completely
launchctl unload ~/Library/LaunchAgents/com.ytunnel.myapp.plist
rm ~/Library/LaunchAgents/com.ytunnel.myapp.plist
```

## License

MIT

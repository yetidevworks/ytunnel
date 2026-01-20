# YTunnel

```
  ___ ___ _______                           __
 |   |   |_     _|.--.--.-----.-----.-----.|  |
  \     /  |   |  |  |  |     |     |  -__||  |
   |___|   |___|  |_____|__|__|__|__|_____||__|
   Cloudflare tunnels made easy!

```

A TUI-first CLI for managing Cloudflare Tunnels with custom domains. Think ngrok, but using your own Cloudflare domain with persistent URLs and a dashboard to manage them.

**Supported Platforms:** macOS and Linux

## Features

- **TUI Dashboard** - Interactive interface to manage all your tunnels
- **Live Metrics** - Real-time request counts, error rates, and connection status
- **Persistent tunnels** - Tunnels run as background daemons (launchd on macOS, systemd on Linux)
- **Automatic DNS management** - Creates and updates CNAME records automatically
- **Multi-zone support** - Use different domains for different tunnels
- **SSL/HTTPS** - Automatic via Cloudflare
- **Ephemeral mode** - Quick one-off tunnels that stop when you exit

## Prerequisites

1. **cloudflared** - Cloudflare's tunnel daemon

   **macOS:**
   ```bash
   brew install cloudflare/cloudflare/cloudflared
   ```

   **Linux (Debian/Ubuntu):**
   ```bash
   curl -L https://pkg.cloudflare.com/cloudflare-main.gpg | sudo tee /usr/share/keyrings/cloudflare-archive-keyring.gpg
   echo "deb [signed-by=/usr/share/keyrings/cloudflare-archive-keyring.gpg] https://pkg.cloudflare.com/cloudflared $(lsb_release -cs) main" | sudo tee /etc/apt/sources.list.d/cloudflared.list
   sudo apt update && sudo apt install cloudflared
   ```

   **Linux (other):**
   ```bash
   # Download the latest release from https://github.com/cloudflare/cloudflared/releases
   sudo cp cloudflared /usr/local/bin/
   sudo chmod +x /usr/local/bin/cloudflared
   ```

   > **Note:** You only need to *install* cloudflared. Do NOT run it as a system service. YTunnel manages cloudflared processes directly.

2. **Cloudflare API Token** with these permissions:
   - Zone:Read
   - DNS:Edit
   - Account > Cloudflare Tunnel:Edit

   Create one at: https://dash.cloudflare.com/profile/api-tokens

3. **A domain** managed by Cloudflare (free tier works)

## Installation

### Homebrew (recommended)

```bash
brew install yetidevworks/ytunnel/ytunnel
```

### From crates.io

```bash
cargo install ytunnel
```

### From source

```bash
git clone https://github.com/yetidevworks/ytunnel
cd ytunnel
cargo install --path .
```

### Pre-built binaries

Download from [GitHub Releases](https://github.com/yetidevworks/ytunnel/releases).

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
│              launchd (macOS) / systemd (Linux)                  │
│              System service manager - always running            │
│                                                                 │
│   macOS: ~/Library/LaunchAgents/com.ytunnel.<name>.plist        │
│   Linux: ~/.config/systemd/user/ytunnel-<name>.service          │
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
| **Persistent** (`ytunnel add --start`) | launchd/systemd | Yes* | Production, always-on services |

*Tunnels don't auto-start by default. They start when you run `ytunnel start` and keep running until you `ytunnel stop` or reboot. To auto-start on login, press `A` in the TUI to toggle auto-start (⟳ indicator shows when enabled).

### What YTunnel Creates

When you run `ytunnel add myapp localhost:3000 --start`:

1. **Cloudflare Tunnel** - Created via API, persists in your Cloudflare account
2. **DNS Record** - CNAME pointing `myapp.yourdomain.com` → tunnel
3. **Credentials** - Tunnel credentials JSON file
4. **Config** - cloudflared YAML config file
5. **Service** - launchd plist (macOS) or systemd unit (Linux)
6. **State** - Entry in `tunnels.toml`

The service file tells launchd/systemd to run cloudflared with your config. Logs go to the logs directory.

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
- `⟳` Auto-start enabled (cyan, shown after hostname)

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
| `A` | Toggle auto-start on login (⟳ = enabled) |
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

When a tunnel goes down or comes back up, ytunnel sends a system notification. This helps you catch issues even when the TUI isn't visible.

- **macOS:** Uses `terminal-notifier` (if installed) or `osascript`
- **Linux:** Uses `notify-send` (requires `libnotify`)

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

# Reset all configuration (start fresh)
ytunnel reset
ytunnel reset -y  # Skip confirmation
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

### File Locations

**macOS:**
| Path | Purpose |
|------|---------|
| `~/Library/Application Support/ytunnel/config.toml` | API credentials and zones |
| `~/Library/Application Support/ytunnel/tunnels.toml` | Persistent tunnel state |
| `~/Library/Application Support/ytunnel/<tunnel-id>.json` | Cloudflare tunnel credentials |
| `~/Library/Application Support/ytunnel/tunnel-configs/<name>.yml` | cloudflared config files |
| `~/Library/Application Support/ytunnel/logs/<name>.log` | Tunnel daemon logs |
| `~/Library/LaunchAgents/com.ytunnel.<name>.plist` | launchd service files |

**Linux:**
| Path | Purpose |
|------|---------|
| `~/.config/ytunnel/config.toml` | API credentials and zones |
| `~/.config/ytunnel/tunnels.toml` | Persistent tunnel state |
| `~/.config/ytunnel/<tunnel-id>.json` | Cloudflare tunnel credentials |
| `~/.config/ytunnel/tunnel-configs/<name>.yml` | cloudflared config files |
| `~/.config/ytunnel/logs/<name>.log` | Tunnel daemon logs |
| `~/.config/systemd/user/ytunnel-<name>.service` | systemd service files |

### Main Config

Config file location: `~/Library/Application Support/ytunnel/config.toml` (macOS) or `~/.config/ytunnel/config.toml` (Linux):

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

`tunnels.toml` (same directory as config.toml):

```toml
[[tunnels]]
name = "myapp"
target = "localhost:3000"
zone_id = "abc123"
zone_name = "example.com"
hostname = "myapp.example.com"
tunnel_id = "cf-tunnel-id"
enabled = true
auto_start = false  # Set to true to start on login
```

## Troubleshooting

### Check tunnel status

```bash
# Via ytunnel
ytunnel list

# Via system service manager
launchctl list | grep ytunnel          # macOS
systemctl --user list-units 'ytunnel-*' # Linux
```

### View logs

```bash
# In TUI: select tunnel and view right pane

# Or directly
tail -f ~/Library/Application\ Support/ytunnel/logs/myapp.log  # macOS
tail -f ~/.config/ytunnel/logs/myapp.log                       # Linux
```

### Tunnel won't start

1. Check if cloudflared is installed: `cloudflared --version`
2. Check the log file for errors
3. Verify credentials exist in the config directory
4. Try running manually: `cloudflared tunnel --config <config-path> run`

### Manually manage a tunnel

**macOS:**
```bash
# Stop
launchctl unload ~/Library/LaunchAgents/com.ytunnel.myapp.plist

# Start
launchctl load ~/Library/LaunchAgents/com.ytunnel.myapp.plist

# Remove completely
launchctl unload ~/Library/LaunchAgents/com.ytunnel.myapp.plist
rm ~/Library/LaunchAgents/com.ytunnel.myapp.plist
```

**Linux:**
```bash
# Stop
systemctl --user stop ytunnel-myapp.service

# Start
systemctl --user start ytunnel-myapp.service

# Remove completely
systemctl --user stop ytunnel-myapp.service
systemctl --user disable ytunnel-myapp.service
rm ~/.config/systemd/user/ytunnel-myapp.service
systemctl --user daemon-reload
```

## Changelog

### v0.3.2

- **Fix init check** - TUI now properly exits with message if `ytunnel init` hasn't been run

### v0.3.1

- **Homebrew tap support** - Install via `brew install yetidevworks/ytunnel/ytunnel`

### v0.3.0

- **Health indicators in tunnel list** - Show red ⚠ warning next to unhealthy tunnels
- **Check all tunnels health** - Periodic health checks now run for all running tunnels, not just selected

### v0.2.0

- **Remote desktop support** - Fixed TUI input issues when using remote desktop/screen sharing (supports key repeat events and paste input)
- **Fixed add tunnel modal** - Input text now renders correctly in the add tunnel dialog
- **Reset command** - Added `ytunnel reset` to remove all configuration and start fresh
- **Init protection** - Prevents accidental re-initialization if already configured

### v0.1.0

- Initial release with TUI dashboard, persistent tunnels, ephemeral mode, and live metrics

## License

MIT

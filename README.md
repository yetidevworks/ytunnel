# ytunnel

A simple CLI for creating Cloudflare Tunnels with custom domains. Think ngrok, but using your own Cloudflare domain with persistent URLs.

## Features

- One command to create a tunnel with a custom subdomain
- Automatic DNS record management
- Persistent named tunnels (great for OAuth callbacks, webhooks, etc.)
- Multi-zone support
- SSL/HTTPS automatic via Cloudflare

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

## Usage

### First-time setup

```bash
ytunnel init
```

This will:
- Verify cloudflared is installed
- Prompt for your API token
- Auto-discover your Cloudflare zones
- Set a default zone

### Run a tunnel

```bash
# Auto-generated subdomain (ytunnel-abc123.example.com)
ytunnel run localhost:3000

# Named subdomain (license.example.com)
ytunnel run license localhost:3000

# Nested subdomain (license.tunnel.example.com)
ytunnel run license.tunnel localhost:3000

# Different zone
ytunnel run api -z dev.example.com localhost:8080

# Specify full URL
ytunnel run myapp http://127.0.0.1:8080
```

### Manage zones

```bash
# List available zones
ytunnel zones

# Change default zone
ytunnel zones default dev.example.com
```

### Manage tunnels

```bash
# List all ytunnel-created tunnels
ytunnel list

# Delete a tunnel (also removes DNS record association)
ytunnel delete license
```

## How it works

When you run `ytunnel run myapp localhost:3000`:

1. Creates (or reuses) a named Cloudflare tunnel via API
2. Saves tunnel credentials to `~/.config/ytunnel/<tunnel-id>.json`
3. Creates/updates a CNAME DNS record pointing to the tunnel
4. Generates a tunnel config and runs `cloudflared tunnel run`

The tunnel remains registered with Cloudflare, so the same subdomain works every time you run it - perfect for OAuth callbacks that need consistent URLs.

## Configuration

Config is stored in `~/.config/ytunnel/config.toml`:

```toml
api_token = "your-token"
account_id = "your-account-id"
default_zone_id = "zone-id"
default_zone_name = "example.com"

[[zones]]
id = "zone-id"
name = "example.com"
```

## License

MIT

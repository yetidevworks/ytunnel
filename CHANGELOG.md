## Changelog

### v0.9.0

- **Show tunnels ytunnel didn't create** - `ytunnel list` and the TUI now query Cloudflare and list every tunnel on the account, not just the ones in local state. Tunnels created in the dashboard, by hand with `cloudflared`, or running on another machine appear marked `[not managed by ytunnel]` instead of being silently omitted
- **Cloudflare-reported status for unmanaged tunnels** - Shows `healthy` / `degraded` / `down` / `inactive` straight from the API, since there's no local process to inspect
- **Clearer refusals** - Start, stop, restart, edit, auto-start, import, and delete now explain why they don't apply to an unmanaged tunnel instead of silently doing nothing
- **Fix `list` reporting an empty account** - It printed "No tunnels configured" for accounts that had tunnels, as long as ytunnel hadn't created them

### v0.8.0

- **Self-update** (`ytunnel update`) — checks GitHub Releases for the latest version and replaces the binary in-place. Detects Homebrew and cargo installs and suggests the appropriate upgrade command instead
- **Update check** (`ytunnel update --check`) — just prints whether a newer version is available, without installing
- **Automatic update notifications** — after CLI commands, ytunnel checks a local cache (refreshed every 24 hours in the background) and prints a one-liner hint to stderr when a newer version is available

### v0.7.1

- **Static Linux binaries** - Linux releases now use musl instead of glibc, producing fully static binaries that run on any Linux distro regardless of glibc version (fixes Raspberry Pi OS compatibility)

### v0.7.0

- **Auto-repair DNS on start** - `start` and `restart` now ensure the CNAME DNS record exists, automatically recreating it if manually deleted

### v0.6.0

- **Tunnel details panel** - New fixed-height panel above logs shows destination URL and public hostname
- **Fix DNS cleanup on delete** - CNAME records are now properly deleted when removing a tunnel
- **Compact metrics panel** - Reduced metrics section height for better log visibility

### v0.5.0

- **Edit tunnel** - Press `e` in TUI to edit a tunnel's target URL or zone/domain without recreating it
- **Animated spinner** - Braille dots spinner animation during async operations (create, start, stop, restart, delete)
- **Cancel operations** - Press `Esc` or `Ctrl+C` to cancel in-progress operations
- **Ctrl+C to quit** - Standard terminal behavior, exits the TUI cleanly
- **Ctrl+Z to suspend** - Suspend the app and resume with `fg`

### v0.4.0

- **Multi-account support** - Manage tunnels across multiple Cloudflare accounts with different API tokens
  - `ytunnel init` now prompts for account name and supports adding additional accounts
  - `ytunnel account list/select/remove/default` commands for account management
  - `--account` flag on all commands to override the default account
  - Press `;` in TUI to cycle through accounts
- **Ephemeral tunnel cleanup** - `ytunnel run` now automatically cleans up DNS records and tunnels on Ctrl+C
- **Text selection in TUI** - Removed mouse capture to allow selecting and copying log text

### v0.3.3

- **Updated dependencies** - ratatui 0.30, crossterm 0.29

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
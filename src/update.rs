use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const GITHUB_REPO_OWNER: &str = "yetidevworks";
const GITHUB_REPO_NAME: &str = "ytunnel";
const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

// ---------- version helpers ----------

fn parse_version(v: &str) -> (u64, u64, u64) {
    let parts: Vec<u64> = v.split('.').filter_map(|p| p.parse().ok()).collect();
    (
        parts.first().copied().unwrap_or(0),
        parts.get(1).copied().unwrap_or(0),
        parts.get(2).copied().unwrap_or(0),
    )
}

fn is_newer(current: &str, latest: &str) -> bool {
    parse_version(latest) > parse_version(current)
}

// ---------- cache ----------

#[derive(serde::Serialize, serde::Deserialize)]
struct UpdateCache {
    latest_version: String,
    checked_at: u64,
}

fn cache_path() -> Option<PathBuf> {
    Some(
        dirs::config_dir()?
            .join("ytunnel")
            .join("update-check.json"),
    )
}

fn read_cache() -> Option<UpdateCache> {
    let content = std::fs::read_to_string(cache_path()?).ok()?;
    serde_json::from_str(&content).ok()
}

fn write_cache(cache: &UpdateCache) -> Result<()> {
    let path = cache_path().context("Could not determine cache path")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string(cache)?)?;
    Ok(())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------- GitHub API ----------

async fn fetch_latest_version() -> Result<String> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/releases/latest",
        GITHUB_REPO_OWNER, GITHUB_REPO_NAME
    );

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(&url)
        .header(
            "User-Agent",
            format!("ytunnel/{}", env!("CARGO_PKG_VERSION")),
        )
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .context("Failed to reach GitHub API")?
        .json()
        .await
        .context("Failed to parse GitHub response")?;

    let tag = body["tag_name"]
        .as_str()
        .context("No tag_name in release")?;

    Ok(tag.strip_prefix('v').unwrap_or(tag).to_string())
}

// ---------- platform ----------

fn platform_target() -> Option<&'static str> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return Some("darwin-aarch64");
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    return Some("darwin-x86_64");
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return Some("linux-x86_64");
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return Some("linux-aarch64");
    #[allow(unreachable_code)]
    None
}

// ---------- install method ----------

enum InstallMethod {
    Homebrew,
    Cargo,
    Binary(PathBuf),
}

fn detect_install_method() -> InstallMethod {
    let exe = std::env::current_exe().unwrap_or_default();
    let s = exe.to_string_lossy();

    if s.contains("/Cellar/") || s.contains("/homebrew/") {
        InstallMethod::Homebrew
    } else if s.contains("/.cargo/bin/") {
        InstallMethod::Cargo
    } else {
        InstallMethod::Binary(exe)
    }
}

// ---------- public entry points ----------

/// `ytunnel update [--check]`
pub async fn cmd_update(check_only: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");

    eprintln!("Checking for updates...");
    let latest = fetch_latest_version().await?;

    let _ = write_cache(&UpdateCache {
        latest_version: latest.clone(),
        checked_at: now_secs(),
    });

    if !is_newer(current, &latest) {
        eprintln!("ytunnel v{} is already the latest version.", current);
        return Ok(());
    }

    eprintln!("Update available: v{} -> v{}", current, latest);

    if check_only {
        eprintln!("\nRun `ytunnel update` to install.");
        return Ok(());
    }

    match detect_install_method() {
        InstallMethod::Homebrew => {
            eprintln!("\nytunnel was installed via Homebrew. Run:");
            eprintln!("  brew upgrade ytunnel");
        }
        InstallMethod::Cargo => {
            eprintln!("\nytunnel was installed via cargo. Run:");
            eprintln!("  cargo install ytunnel");
        }
        InstallMethod::Binary(exe_path) => {
            perform_update(&exe_path, &latest).await?;
        }
    }

    Ok(())
}

/// Non-blocking hint printed after CLI commands (reads cache, never does network I/O).
/// Spawns a background refresh when the cache is stale.
pub fn maybe_print_update_hint() {
    let current = env!("CARGO_PKG_VERSION");
    let now = now_secs();

    if let Some(cache) = read_cache() {
        if is_newer(current, &cache.latest_version) {
            eprintln!(
                "\nytunnel v{} available (current: v{}). Run `ytunnel update` to upgrade.",
                cache.latest_version, current
            );
            return;
        }
        if now.saturating_sub(cache.checked_at) < CHECK_INTERVAL_SECS {
            return;
        }
    }

    // Cache is stale or missing — fire-and-forget background check
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe)
            .args(["update", "--check"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

// ---------- download & replace ----------

async fn perform_update(exe_path: &Path, version: &str) -> Result<()> {
    let target = platform_target().context("Unsupported platform for self-update")?;

    let asset_name = format!("ytunnel-{}.tar.gz", target);

    let download_url = format!(
        "https://github.com/{}/{}/releases/download/v{}/{}",
        GITHUB_REPO_OWNER, GITHUB_REPO_NAME, version, asset_name
    );

    eprintln!("Downloading {}...", asset_name);

    let tmp = std::env::temp_dir().join(format!("ytunnel-update-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let _cleanup = TempDirGuard(tmp.clone());

    let archive_path = tmp.join(&asset_name);

    // Download
    let client = reqwest::Client::new();
    let bytes = client
        .get(&download_url)
        .header(
            "User-Agent",
            format!("ytunnel/{}", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .context("Failed to download release")?
        .bytes()
        .await
        .context("Failed to read response body")?;

    std::fs::write(&archive_path, &bytes)?;

    // Extract
    let status = std::process::Command::new("tar")
        .args([
            "xzf",
            &archive_path.to_string_lossy(),
            "-C",
            &tmp.to_string_lossy(),
        ])
        .status()
        .context("Failed to run tar")?;

    if !status.success() {
        anyhow::bail!("tar extraction failed");
    }

    let new_bin = tmp.join("ytunnel");
    if !new_bin.exists() {
        anyhow::bail!("Binary not found in archive");
    }

    // Replace
    replace_binary(&new_bin, exe_path)?;

    eprintln!("Updated ytunnel to v{}", version);
    Ok(())
}

fn replace_binary(new_bin: &Path, exe_path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(new_bin, std::fs::Permissions::from_mode(0o755))?;

        if std::fs::rename(new_bin, exe_path).is_err() {
            std::fs::copy(new_bin, exe_path)?;
        }
    }

    #[cfg(not(unix))]
    {
        anyhow::bail!("Self-update is only supported on macOS and Linux");
    }

    Ok(())
}

struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version() {
        assert_eq!(parse_version("0.7.1"), (0, 7, 1));
        assert_eq!(parse_version("1.0.0"), (1, 0, 0));
    }

    #[test]
    fn test_is_newer() {
        assert!(is_newer("0.7.1", "0.7.2"));
        assert!(is_newer("0.7.1", "0.8.0"));
        assert!(is_newer("0.7.1", "1.0.0"));
        assert!(!is_newer("0.7.1", "0.7.1"));
        assert!(!is_newer("0.7.1", "0.7.0"));
    }

    #[test]
    fn test_platform_target_is_some() {
        assert!(platform_target().is_some());
    }
}

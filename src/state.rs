use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use crate::config;

// Represents the current runtime status of a tunnel
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelStatus {
    Running,
    Stopped,
    Error,
}

impl TunnelStatus {
    pub fn symbol(&self) -> &'static str {
        match self {
            TunnelStatus::Running => "●",
            TunnelStatus::Stopped => "○",
            TunnelStatus::Error => "✗",
        }
    }
}

// A YAML-serializable value for a cloudflared option.
// Untagged enum: TOML/YAML value shape determines the variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TunnelOptionValue {
    Bool(bool),
    Int(i64),
    String(String),
    List(Vec<String>),
}

// User-selected log mode: cloudflared verbosity + ytunnel display filter.
// Default: cloudflared at info, raw display. Debug: cloudflared at debug, raw
// display (verbose, includes headers). NgrokDev: cloudflared at debug, parsed
// display (compact per-request rows via crate::tui::log_filter).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LogMode {
    #[default]
    Default,
    Debug,
    NgrokDev,
}

impl LogMode {
    pub fn is_default(&self) -> bool {
        matches!(self, LogMode::Default)
    }
    // The cloudflared loglevel value this mode implies. None => omit from YAML.
    pub fn to_cloudflared_loglevel(self) -> Option<&'static str> {
        match self {
            LogMode::Default => None,
            LogMode::Debug | LogMode::NgrokDev => Some("debug"),
        }
    }
}

// A persistent tunnel configuration stored in tunnels.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistentTunnel {
    pub name: String,
    // Which account owns this tunnel (defaults to selected account for migration)
    #[serde(default)]
    pub account_name: String,
    pub target: String,
    pub zone_id: String,
    pub zone_name: String,
    pub hostname: String,
    pub tunnel_id: String,
    pub enabled: bool,
    // Whether to auto-start on login (RunAtLoad in launchd)
    #[serde(default)]
    pub auto_start: bool,
    // Port for cloudflared metrics endpoint (optional, calculated if not set)
    #[serde(default)]
    pub metrics_port: Option<u16>,
    // Top-level cloudflared YAML options (loglevel, protocol, retries, ...).
    // Serialized as top-level keys in the generated per-tunnel YAML.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tunnel_options: BTreeMap<String, TunnelOptionValue>,
    // Per-ingress-rule originRequest options (httpHostHeader, noTLSVerify, ...).
    // Attached to the ingress rule as `originRequest:` in the generated YAML.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub origin_request: BTreeMap<String, TunnelOptionValue>,
    // Log verbosity + display filter. Default = cloudflared info, raw display.
    // See LogMode for details.
    #[serde(default, skip_serializing_if = "LogMode::is_default")]
    pub log_mode: LogMode,
}

impl PersistentTunnel {
    // Get the path to the credentials file for this tunnel
    pub fn credentials_path(&self) -> Result<PathBuf> {
        let config_dir = config::config_dir()?;
        Ok(config_dir.join(format!("{}.json", self.tunnel_id)))
    }

    // Get the path to the tunnel config file
    pub fn config_path(&self) -> Result<PathBuf> {
        let config_dir = config::config_dir()?;
        let configs_dir = config_dir.join("tunnel-configs");
        Ok(configs_dir.join(format!("{}.yml", self.name)))
    }

    // Get the path to the log file for this tunnel
    pub fn log_path(&self) -> Result<PathBuf> {
        let config_dir = config::config_dir()?;
        let logs_dir = config_dir.join("logs");
        Ok(logs_dir.join(format!("{}.log", self.name)))
    }

    // Get the metrics port for this tunnel (calculates from name hash if not set)
    pub fn get_metrics_port(&self) -> u16 {
        self.metrics_port.unwrap_or_else(|| {
            // Calculate a port based on the tunnel name hash
            // Range: 21000-21999 to avoid conflicts with cloudflared defaults (20241-20245)
            let hash: u32 = self
                .name
                .bytes()
                .fold(0u32, |acc, b| acc.wrapping_add(b as u32).wrapping_mul(31));
            21000 + (hash % 1000) as u16
        })
    }

    // Get the metrics URL for this tunnel
    pub fn metrics_url(&self) -> String {
        format!("http://localhost:{}/metrics", self.get_metrics_port())
    }
}

// The collection of all persistent tunnels
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TunnelState {
    #[serde(default)]
    pub tunnels: Vec<PersistentTunnel>,
}

// If a tunnel has tunnel_options["loglevel"] set (from before log_mode
// existed), promote it to the first-class log_mode field and remove the
// map entry. Preserves the user's intent across the schema change.
pub fn migrate_loglevel_to_log_mode(state: &mut TunnelState) {
    for tunnel in &mut state.tunnels {
        if let Some(TunnelOptionValue::String(s)) = tunnel.tunnel_options.get("loglevel") {
            if s == "debug" && tunnel.log_mode.is_default() {
                tunnel.log_mode = LogMode::Debug;
            }
            tunnel.tunnel_options.remove("loglevel");
        }
    }
}

impl TunnelState {
    // Load the tunnel state from disk
    pub fn load() -> Result<Self> {
        let path = tunnels_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read tunnels from {}", path.display()))?;

        let state: TunnelState =
            toml::from_str(&contents).with_context(|| "Failed to parse tunnels.toml")?;

        Ok(state)
    }

    // Load tunnel state and migrate any tunnels with empty account_name
    // to the specified default account
    pub fn load_and_migrate(default_account: &str) -> Result<Self> {
        let mut state = Self::load()?;

        // Check if any tunnels need account_name migration
        let needs_account_migration = state.tunnels.iter().any(|t| t.account_name.is_empty());

        if needs_account_migration {
            for tunnel in &mut state.tunnels {
                if tunnel.account_name.is_empty() {
                    tunnel.account_name = default_account.to_string();
                }
            }
        }

        // Check if any tunnels have a legacy loglevel tunnel_option that should
        // be promoted to the first-class log_mode field.
        let needs_loglevel_migration = state
            .tunnels
            .iter()
            .any(|t| t.tunnel_options.contains_key("loglevel"));

        if needs_loglevel_migration {
            migrate_loglevel_to_log_mode(&mut state);
        }

        // Save once if any migration ran
        if needs_account_migration || needs_loglevel_migration {
            state.save()?;
        }

        Ok(state)
    }

    // Save the tunnel state to disk
    pub fn save(&self) -> Result<()> {
        let dir = config::config_dir()?;
        fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create config directory: {}", dir.display()))?;

        let path = tunnels_path()?;
        let contents = toml::to_string_pretty(self).context("Failed to serialize tunnels")?;
        fs::write(&path, contents)
            .with_context(|| format!("Failed to write tunnels to {}", path.display()))?;

        Ok(())
    }

    // Find a tunnel by name (searches all accounts)
    pub fn find(&self, name: &str) -> Option<&PersistentTunnel> {
        self.tunnels.iter().find(|t| t.name == name)
    }

    // Find a tunnel by name (mutable, searches all accounts)
    pub fn find_mut(&mut self, name: &str) -> Option<&mut PersistentTunnel> {
        self.tunnels.iter_mut().find(|t| t.name == name)
    }

    // Find a tunnel by name for a specific account
    pub fn find_for_account(&self, name: &str, account: &str) -> Option<&PersistentTunnel> {
        self.tunnels
            .iter()
            .find(|t| t.name == name && t.account_name == account)
    }

    // Find a tunnel by name for a specific account (mutable)
    pub fn find_for_account_mut(
        &mut self,
        name: &str,
        account: &str,
    ) -> Option<&mut PersistentTunnel> {
        self.tunnels
            .iter_mut()
            .find(|t| t.name == name && t.account_name == account)
    }

    // Get all tunnels for a specific account
    pub fn tunnels_for_account(&self, account: &str) -> Vec<&PersistentTunnel> {
        self.tunnels
            .iter()
            .filter(|t| t.account_name == account)
            .collect()
    }

    // Add a new tunnel
    pub fn add(&mut self, tunnel: PersistentTunnel) {
        self.tunnels.push(tunnel);
    }

    // Remove a tunnel by name (from any account)
    pub fn remove(&mut self, name: &str) -> Option<PersistentTunnel> {
        if let Some(pos) = self.tunnels.iter().position(|t| t.name == name) {
            Some(self.tunnels.remove(pos))
        } else {
            None
        }
    }

    // Remove a tunnel by name for a specific account
    pub fn remove_for_account(&mut self, name: &str, account: &str) -> Option<PersistentTunnel> {
        if let Some(pos) = self
            .tunnels
            .iter()
            .position(|t| t.name == name && t.account_name == account)
        {
            Some(self.tunnels.remove(pos))
        } else {
            None
        }
    }
}

// Get the path to the tunnels.toml file
pub fn tunnels_path() -> Result<PathBuf> {
    Ok(config::config_dir()?.join("tunnels.toml"))
}

// Ensure the tunnel-configs directory exists
pub fn ensure_configs_dir() -> Result<PathBuf> {
    let config_dir = config::config_dir()?;
    let configs_dir = config_dir.join("tunnel-configs");
    fs::create_dir_all(&configs_dir).with_context(|| {
        format!(
            "Failed to create configs directory: {}",
            configs_dir.display()
        )
    })?;
    Ok(configs_dir)
}

// Ensure the logs directory exists
pub fn ensure_logs_dir() -> Result<PathBuf> {
    let config_dir = config::config_dir()?;
    let logs_dir = config_dir.join("logs");
    fs::create_dir_all(&logs_dir)
        .with_context(|| format!("Failed to create logs directory: {}", logs_dir.display()))?;
    Ok(logs_dir)
}

// Emit a scalar YAML value for a TunnelOptionValue in double-quoted style
// for strings (safe against YAML type coercion; e.g. "yes"/"no"/"on"/"off"/"1.0"
// being parsed as bools or numbers in YAML 1.1). Booleans and integers are bare.
fn yaml_scalar(value: &TunnelOptionValue) -> String {
    match value {
        TunnelOptionValue::Bool(b) => b.to_string(),
        TunnelOptionValue::Int(n) => n.to_string(),
        TunnelOptionValue::String(s) => yaml_quote(s),
        TunnelOptionValue::List(_) => {
            // Lists are emitted as block sequences by the caller, not as scalars.
            String::new()
        }
    }
}

// Wrap a string in YAML double-quoted style with proper escaping.
fn yaml_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str(r"\\"),
            '"' => out.push_str(r#"\""#),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '\t' => out.push_str(r"\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

// Write a (key, value) pair from an option map into an in-progress YAML
// string at the given indent. Handles scalars and lists.
fn write_yaml_option(out: &mut String, key: &str, value: &TunnelOptionValue, indent: usize) {
    use std::fmt::Write;
    let pad = " ".repeat(indent);
    match value {
        TunnelOptionValue::Bool(_) | TunnelOptionValue::Int(_) | TunnelOptionValue::String(_) => {
            let _ = writeln!(out, "{pad}{key}: {}", yaml_scalar(value));
        }
        TunnelOptionValue::List(items) => {
            let _ = writeln!(out, "{pad}{key}:");
            for item in items {
                let _ = writeln!(out, "{pad}  - {}", yaml_quote(item));
            }
        }
    }
}

// Build the cloudflared YAML from raw parts. Shared between persistent tunnels
// (via generate_tunnel_config) and ephemeral tunnels (via tunnel::run_tunnel).
pub fn build_tunnel_yaml(
    tunnel_id: &str,
    credentials_path: &std::path::Path,
    hostname: &str,
    target: &str,
    tunnel_options: &BTreeMap<String, TunnelOptionValue>,
    origin_request: &BTreeMap<String, TunnelOptionValue>,
) -> Result<String> {
    use std::fmt::Write;

    let target_url = if target.starts_with("http://") || target.starts_with("https://") {
        target.to_string()
    } else {
        format!("http://{}", target)
    };

    let mut out = String::new();
    let _ = writeln!(out, "tunnel: {}", tunnel_id);
    let _ = writeln!(out, "credentials-file: {}", credentials_path.display());

    for (key, value) in tunnel_options {
        write_yaml_option(&mut out, key, value, 0);
    }

    let _ = writeln!(out, "ingress:");
    let _ = writeln!(out, "  - hostname: {}", hostname);
    let _ = writeln!(out, "    service: {}", target_url);
    if !origin_request.is_empty() {
        let _ = writeln!(out, "    originRequest:");
        for (key, value) in origin_request {
            write_yaml_option(&mut out, key, value, 6);
        }
    }
    let _ = writeln!(out, "  - service: http_status:404");

    Ok(out)
}

// Generate the cloudflared config YAML content for a tunnel.
pub fn generate_tunnel_config(tunnel: &PersistentTunnel) -> Result<String> {
    let credentials_path = tunnel.credentials_path()?;
    // Merge log_mode's implied loglevel into tunnel_options for YAML emission.
    let mut effective_options = tunnel.tunnel_options.clone();
    if let Some(level) = tunnel.log_mode.to_cloudflared_loglevel() {
        effective_options.insert(
            "loglevel".to_string(),
            TunnelOptionValue::String(level.to_string()),
        );
    }
    build_tunnel_yaml(
        &tunnel.tunnel_id,
        &credentials_path,
        &tunnel.hostname,
        &tunnel.target,
        &effective_options,
        &tunnel.origin_request,
    )
}

// Write the cloudflared config file for a tunnel
pub fn write_tunnel_config(tunnel: &PersistentTunnel) -> Result<PathBuf> {
    ensure_configs_dir()?;
    let config_path = tunnel.config_path()?;
    let config_content = generate_tunnel_config(tunnel)?;
    fs::write(&config_path, &config_content)
        .with_context(|| format!("Failed to write tunnel config to {}", config_path.display()))?;
    Ok(config_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_tunnel_deserializes_without_new_fields() {
        // Represents a tunnels.toml entry written by an older ytunnel version.
        let legacy_toml = r#"
            name = "demo"
            account_name = "acct"
            target = "http://localhost:3000"
            zone_id = "zone123"
            zone_name = "example.com"
            hostname = "demo.example.com"
            tunnel_id = "uuid-1"
            enabled = true
        "#;

        let tunnel: PersistentTunnel = toml::from_str(legacy_toml).expect("should parse");
        assert_eq!(tunnel.name, "demo");
        assert!(tunnel.tunnel_options.is_empty());
        assert!(tunnel.origin_request.is_empty());
    }

    #[test]
    fn tunnel_option_value_roundtrips_all_variants() {
        use std::collections::BTreeMap;

        let mut opts: BTreeMap<String, TunnelOptionValue> = BTreeMap::new();
        opts.insert("loglevel".into(), TunnelOptionValue::String("debug".into()));
        opts.insert("retries".into(), TunnelOptionValue::Int(5));
        opts.insert("no-autoupdate".into(), TunnelOptionValue::Bool(true));
        opts.insert(
            "features".into(),
            TunnelOptionValue::List(vec!["a".into(), "b".into()]),
        );

        let serialized = toml::to_string(&opts).unwrap();
        let deserialized: BTreeMap<String, TunnelOptionValue> =
            toml::from_str(&serialized).unwrap();
        assert_eq!(opts.len(), deserialized.len());
        assert!(
            matches!(deserialized.get("loglevel"), Some(TunnelOptionValue::String(s)) if s == "debug")
        );
        assert!(matches!(
            deserialized.get("retries"),
            Some(TunnelOptionValue::Int(5))
        ));
        assert!(matches!(
            deserialized.get("no-autoupdate"),
            Some(TunnelOptionValue::Bool(true))
        ));
        assert!(
            matches!(deserialized.get("features"), Some(TunnelOptionValue::List(v)) if v.len() == 2)
        );
    }

    fn sample_tunnel() -> PersistentTunnel {
        PersistentTunnel {
            name: "demo".into(),
            account_name: "acct".into(),
            target: "http://localhost:3000".into(),
            zone_id: "zone123".into(),
            zone_name: "example.com".into(),
            hostname: "demo.example.com".into(),
            tunnel_id: "uuid-1".into(),
            enabled: true,
            auto_start: false,
            metrics_port: Some(21000),
            tunnel_options: BTreeMap::new(),
            origin_request: BTreeMap::new(),
            log_mode: LogMode::Default,
        }
    }

    #[test]
    fn log_mode_defaults_to_default() {
        let tunnel: PersistentTunnel = toml::from_str(
            r#"
            name = "demo"
            account_name = "acct"
            target = "http://localhost:3000"
            zone_id = "z"
            zone_name = "example.com"
            hostname = "demo.example.com"
            tunnel_id = "uuid"
            enabled = true
        "#,
        )
        .unwrap();
        assert_eq!(tunnel.log_mode, LogMode::Default);
    }

    #[test]
    fn log_mode_debug_emits_loglevel_in_yaml() {
        let mut tunnel = sample_tunnel();
        tunnel.log_mode = LogMode::Debug;
        let yaml = generate_tunnel_config(&tunnel).unwrap();
        assert!(
            yaml.contains("\nloglevel: \"debug\"\n"),
            "expected loglevel line:\n{yaml}"
        );
    }

    #[test]
    fn log_mode_ngrok_dev_emits_loglevel_in_yaml() {
        let mut tunnel = sample_tunnel();
        tunnel.log_mode = LogMode::NgrokDev;
        let yaml = generate_tunnel_config(&tunnel).unwrap();
        assert!(yaml.contains("\nloglevel: \"debug\"\n"));
    }

    #[test]
    fn log_mode_default_does_not_emit_loglevel() {
        let tunnel = sample_tunnel();
        let yaml = generate_tunnel_config(&tunnel).unwrap();
        assert!(!yaml.contains("loglevel"));
    }

    #[test]
    fn migration_moves_legacy_loglevel_to_log_mode() {
        let toml = r#"
            [[tunnels]]
            name = "demo"
            account_name = "acct"
            target = "http://localhost:3000"
            zone_id = "z"
            zone_name = "example.com"
            hostname = "demo.example.com"
            tunnel_id = "uuid"
            enabled = true

            [tunnels.tunnel_options]
            loglevel = "debug"
        "#;
        let mut state: TunnelState = toml::from_str(toml).unwrap();
        migrate_loglevel_to_log_mode(&mut state);
        assert_eq!(state.tunnels[0].log_mode, LogMode::Debug);
        assert!(!state.tunnels[0].tunnel_options.contains_key("loglevel"));
    }

    #[test]
    fn empty_options_produces_legacy_shape() {
        let tunnel = sample_tunnel();
        let yaml = generate_tunnel_config(&tunnel).expect("generate");

        // Must contain the original core lines, unchanged.
        assert!(
            yaml.starts_with("tunnel: uuid-1\n"),
            "unexpected start:\n{yaml}"
        );
        assert!(
            yaml.contains("\ncredentials-file: "),
            "missing credentials-file:\n{yaml}"
        );
        assert!(yaml.contains("\ningress:\n"), "missing ingress:\n{yaml}");
        assert!(
            yaml.contains("\n  - hostname: demo.example.com\n    service: http://localhost:3000\n"),
            "unexpected ingress rule shape:\n{yaml}"
        );
        assert!(
            yaml.contains("\n  - service: http_status:404\n"),
            "missing catch-all:\n{yaml}"
        );

        // Must NOT include any advanced-options artifacts when both maps empty.
        assert!(
            !yaml.contains("originRequest"),
            "unexpected originRequest key"
        );
        for key in ["loglevel", "protocol", "retries"] {
            assert!(
                !yaml.contains(&format!("\n{key}:")),
                "unexpected top-level key {key} in empty-options output:\n{yaml}"
            );
        }
    }

    #[test]
    fn tunnel_options_render_as_top_level_yaml_keys() {
        let mut tunnel = sample_tunnel();
        tunnel
            .tunnel_options
            .insert("loglevel".into(), TunnelOptionValue::String("debug".into()));
        tunnel
            .tunnel_options
            .insert("retries".into(), TunnelOptionValue::Int(5));
        tunnel
            .tunnel_options
            .insert("no-autoupdate".into(), TunnelOptionValue::Bool(true));
        tunnel.tunnel_options.insert(
            "features".into(),
            TunnelOptionValue::List(vec!["diag-http".into(), "diag-mem".into()]),
        );

        let yaml = generate_tunnel_config(&tunnel).unwrap();

        // Strings are double-quoted (safe against YAML type coercion).
        assert!(
            yaml.contains("\nloglevel: \"debug\"\n"),
            "loglevel line missing:\n{yaml}"
        );
        // Ints and bools are emitted bare.
        assert!(
            yaml.contains("\nretries: 5\n"),
            "retries line missing:\n{yaml}"
        );
        assert!(
            yaml.contains("\nno-autoupdate: true\n"),
            "bool line missing:\n{yaml}"
        );
        // Lists get a block-style sequence with quoted string items.
        assert!(
            yaml.contains("\nfeatures:\n  - \"diag-http\"\n  - \"diag-mem\"\n"),
            "list serialization wrong:\n{yaml}"
        );

        // Top-level options must appear BEFORE ingress:.
        let features_pos = yaml.find("\nfeatures:").expect("features line");
        let ingress_pos = yaml.find("\ningress:").expect("ingress line");
        assert!(features_pos < ingress_pos, "options must precede ingress");
    }

    #[test]
    fn yaml_string_escapes_special_chars() {
        let mut tunnel = sample_tunnel();
        tunnel.tunnel_options.insert(
            "label".into(),
            // Value contains a backslash and a double-quote.
            TunnelOptionValue::String(r#"weird\value"here"#.into()),
        );

        let yaml = generate_tunnel_config(&tunnel).unwrap();
        // Both special chars should be escaped in double-quoted YAML style.
        assert!(
            yaml.contains(r#"label: "weird\\value\"here""#),
            "escaping wrong:\n{yaml}"
        );
    }

    #[test]
    fn origin_request_attaches_to_hostname_rule_only() {
        let mut tunnel = sample_tunnel();
        tunnel.origin_request.insert(
            "httpHostHeader".into(),
            TunnelOptionValue::String("myapp.localhost".into()),
        );
        tunnel
            .origin_request
            .insert("noTLSVerify".into(), TunnelOptionValue::Bool(true));
        tunnel.origin_request.insert(
            "connectTimeout".into(),
            TunnelOptionValue::String("30s".into()),
        );

        let yaml = generate_tunnel_config(&tunnel).unwrap();

        // originRequest appears indented under the hostname rule (indent 4).
        assert!(
            yaml.contains("    originRequest:\n"),
            "originRequest block missing or wrong indent:\n{yaml}"
        );
        // Its child keys are indented one level deeper (indent 6).
        assert!(
            yaml.contains("      httpHostHeader: \"myapp.localhost\"\n"),
            "httpHostHeader missing / wrong indent:\n{yaml}"
        );
        assert!(
            yaml.contains("      noTLSVerify: true\n"),
            "noTLSVerify wrong:\n{yaml}"
        );
        assert!(
            yaml.contains("      connectTimeout: \"30s\"\n"),
            "connectTimeout wrong:\n{yaml}"
        );

        // originRequest must appear EXACTLY ONCE — never on the catch-all rule.
        assert_eq!(
            yaml.matches("originRequest").count(),
            1,
            "originRequest should appear once, not on catch-all:\n{yaml}"
        );

        // originRequest block sits BEFORE the catch-all `- service: http_status:404` line.
        let origin_pos = yaml.find("originRequest").unwrap();
        let catch_all_pos = yaml.find("http_status:404").unwrap();
        assert!(origin_pos < catch_all_pos);
    }

    #[test]
    fn build_yaml_from_parts_matches_persistent_shape() {
        use std::path::PathBuf;

        let yaml = build_tunnel_yaml(
            "uuid-1",
            &PathBuf::from("/tmp/creds.json"),
            "demo.example.com",
            "http://localhost:3000",
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();

        assert!(yaml.starts_with("tunnel: uuid-1\n"));
        assert!(yaml.contains("\ncredentials-file: /tmp/creds.json\n"));
        assert!(
            yaml.contains("\n  - hostname: demo.example.com\n    service: http://localhost:3000\n")
        );
        assert!(yaml.contains("\n  - service: http_status:404\n"));
    }
}

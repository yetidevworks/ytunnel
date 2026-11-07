// Static registry of cloudflared configuration options ytunnel can render in the
// Advanced edit pane. Each entry describes one option's YAML key, scope, type,
// default (if any), and a one-line human description.

use crate::state::TunnelOptionValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionScope {
    // Top-level YAML key on the tunnel config document.
    Tunnel,
    // Nested under the ingress rule's `originRequest:` block.
    OriginRequest,
}

#[derive(Debug, Clone)]
pub enum OptionKind {
    Bool {
        default: bool,
    },
    Int {
        default: Option<i64>,
        min: Option<i64>,
        max: Option<i64>,
    },
    // Free-text string. `placeholder` shown in the empty edit field.
    String {
        default: Option<&'static str>,
        placeholder: &'static str,
    },
    // Duration in cloudflared syntax (e.g. "30s", "1m30s"). Validated on save.
    Duration {
        default: Option<&'static str>,
    },
    // Fixed set of allowed values.
    Enum {
        default: Option<&'static str>,
        choices: &'static [&'static str],
    },
    // Newline-separated list in the UI; serialized as a YAML sequence.
    // No `default` field — absence from the map is the effective default.
    List {
        placeholder: &'static str,
    },
}

#[derive(Debug, Clone)]
pub struct OptionSpec {
    pub yaml_key: &'static str,
    pub scope: OptionScope,
    pub display_name: &'static str,
    pub description: &'static str,
    pub kind: OptionKind,
}

pub const OPTIONS: &[OptionSpec] = &[
    // --- Tunnel scope ---
    // Note: `loglevel` is intentionally omitted here — it's promoted to a
    // first-class field on the Basic edit tab via `LogMode`. See `src/state.rs`.
    OptionSpec {
        yaml_key: "transport-loglevel",
        scope: OptionScope::Tunnel,
        display_name: "Transport log level",
        description: "Transport-layer logging verbosity.",
        kind: OptionKind::Enum {
            default: Some("info"),
            choices: &["debug", "info", "warn", "error", "fatal"],
        },
    },
    OptionSpec {
        yaml_key: "protocol",
        scope: OptionScope::Tunnel,
        display_name: "Protocol",
        description: "Transport protocol to Cloudflare edge.",
        kind: OptionKind::Enum {
            default: Some("auto"),
            choices: &["auto", "http2", "quic"],
        },
    },
    OptionSpec {
        yaml_key: "retries",
        scope: OptionScope::Tunnel,
        display_name: "Retries",
        description: "Max connection retry attempts on error.",
        kind: OptionKind::Int {
            default: Some(5),
            min: Some(0),
            max: Some(100),
        },
    },
    OptionSpec {
        yaml_key: "grace-period",
        scope: OptionScope::Tunnel,
        display_name: "Grace period",
        description: "Shutdown grace period after SIGTERM.",
        kind: OptionKind::Duration {
            default: Some("30s"),
        },
    },
    OptionSpec {
        yaml_key: "edge-ip-version",
        scope: OptionScope::Tunnel,
        display_name: "Edge IP version",
        description: "IP version for connections to Cloudflare edge.",
        kind: OptionKind::Enum {
            default: Some("auto"),
            choices: &["4", "6", "auto"],
        },
    },
    OptionSpec {
        yaml_key: "region",
        scope: OptionScope::Tunnel,
        display_name: "Region",
        description: "Cloudflare edge region. Empty = global.",
        kind: OptionKind::String {
            default: None,
            placeholder: "us / eu / (empty for global)",
        },
    },
    OptionSpec {
        yaml_key: "metrics-update-freq",
        scope: OptionScope::Tunnel,
        display_name: "Metrics update frequency",
        description: "How often to refresh internal metrics.",
        kind: OptionKind::Duration {
            default: Some("5s"),
        },
    },
    OptionSpec {
        yaml_key: "compression-quality",
        scope: OptionScope::Tunnel,
        display_name: "Compression quality",
        description: "Cross-stream compression level. 0=off, 3+=high.",
        kind: OptionKind::Int {
            default: Some(0),
            min: Some(0),
            max: Some(9),
        },
    },
    OptionSpec {
        yaml_key: "post-quantum",
        scope: OptionScope::Tunnel,
        display_name: "Post-quantum",
        description: "Enable experimental post-quantum secure tunnel.",
        kind: OptionKind::Bool { default: false },
    },
    OptionSpec {
        yaml_key: "label",
        scope: OptionScope::Tunnel,
        display_name: "Connector label",
        description: "Human-readable label for this connector instance.",
        kind: OptionKind::String {
            default: None,
            placeholder: "e.g. laptop-office",
        },
    },
    OptionSpec {
        yaml_key: "features",
        scope: OptionScope::Tunnel,
        display_name: "Features",
        description: "Opt into experimental features (one per line).",
        kind: OptionKind::List {
            placeholder: "diag-http\ndiag-mem",
        },
    },
    OptionSpec {
        yaml_key: "bastion",
        scope: OptionScope::Tunnel,
        display_name: "Bastion",
        description: "Run as jump host.",
        kind: OptionKind::Bool { default: false },
    },
    OptionSpec {
        yaml_key: "pidfile",
        scope: OptionScope::Tunnel,
        display_name: "PID file",
        description: "Path to write cloudflared PID.",
        kind: OptionKind::String {
            default: None,
            placeholder: "/var/run/cloudflared.pid",
        },
    },
    OptionSpec {
        yaml_key: "logfile",
        scope: OptionScope::Tunnel,
        display_name: "Log file",
        description: "Path to write cloudflared logs.",
        kind: OptionKind::String {
            default: None,
            placeholder: "/var/log/cloudflared.log",
        },
    },
    OptionSpec {
        yaml_key: "log-directory",
        scope: OptionScope::Tunnel,
        display_name: "Log directory",
        description: "Directory to write cloudflared logs.",
        kind: OptionKind::String {
            default: None,
            placeholder: "/var/log/cloudflared",
        },
    },
    // --- Origin request scope ---
    OptionSpec {
        yaml_key: "httpHostHeader",
        scope: OptionScope::OriginRequest,
        display_name: "HTTP Host header",
        description: "Override the Host header sent to origin.",
        kind: OptionKind::String {
            default: None,
            placeholder: "e.g. myapp.localhost",
        },
    },
    OptionSpec {
        yaml_key: "originServerName",
        scope: OptionScope::OriginRequest,
        display_name: "Origin server name",
        description: "SNI hostname when connecting to origin over TLS.",
        kind: OptionKind::String {
            default: None,
            placeholder: "e.g. api.internal",
        },
    },
    OptionSpec {
        yaml_key: "caPool",
        scope: OptionScope::OriginRequest,
        display_name: "Origin CA pool",
        description: "Path to CA bundle for validating origin cert.",
        kind: OptionKind::String {
            default: None,
            placeholder: "/etc/ssl/certs/ca.pem",
        },
    },
    OptionSpec {
        yaml_key: "noTLSVerify",
        scope: OptionScope::OriginRequest,
        display_name: "No TLS verify",
        description: "Skip TLS verification of the origin cert (unsafe).",
        kind: OptionKind::Bool { default: false },
    },
    OptionSpec {
        yaml_key: "noHappyEyeballs",
        scope: OptionScope::OriginRequest,
        display_name: "No happy eyeballs",
        description: "Disable IPv4/v6 happy-eyeballs fallback.",
        kind: OptionKind::Bool { default: false },
    },
    OptionSpec {
        yaml_key: "connectTimeout",
        scope: OptionScope::OriginRequest,
        display_name: "Connect timeout",
        description: "Origin connection establishment timeout.",
        kind: OptionKind::Duration {
            default: Some("30s"),
        },
    },
    OptionSpec {
        yaml_key: "tlsTimeout",
        scope: OptionScope::OriginRequest,
        display_name: "TLS timeout",
        description: "Origin TLS handshake timeout.",
        kind: OptionKind::Duration {
            default: Some("10s"),
        },
    },
    OptionSpec {
        yaml_key: "tcpKeepAlive",
        scope: OptionScope::OriginRequest,
        display_name: "TCP keepalive",
        description: "TCP keepalive interval for origin connection.",
        kind: OptionKind::Duration {
            default: Some("30s"),
        },
    },
    OptionSpec {
        yaml_key: "keepAliveConnections",
        scope: OptionScope::OriginRequest,
        display_name: "Keepalive pool size",
        description: "Max idle origin connections to keep alive.",
        kind: OptionKind::Int {
            default: Some(100),
            min: Some(0),
            max: Some(10_000),
        },
    },
    OptionSpec {
        yaml_key: "keepAliveTimeout",
        scope: OptionScope::OriginRequest,
        display_name: "Keepalive timeout",
        description: "Idle timeout before closing a keepalive conn.",
        kind: OptionKind::Duration {
            default: Some("1m30s"),
        },
    },
    OptionSpec {
        yaml_key: "http2Origin",
        scope: OptionScope::OriginRequest,
        display_name: "HTTP/2 origin",
        description: "Speak HTTP/2 to the origin server.",
        kind: OptionKind::Bool { default: false },
    },
    OptionSpec {
        yaml_key: "disableChunkedEncoding",
        scope: OptionScope::OriginRequest,
        display_name: "Disable chunked encoding",
        description: "Disable transfer-encoding: chunked (WSGI hack).",
        kind: OptionKind::Bool { default: false },
    },
    OptionSpec {
        yaml_key: "proxyAddress",
        scope: OptionScope::OriginRequest,
        display_name: "Proxy listen address",
        description: "cloudflared's local proxy listen address.",
        kind: OptionKind::String {
            default: Some("127.0.0.1"),
            placeholder: "127.0.0.1",
        },
    },
    OptionSpec {
        yaml_key: "proxyPort",
        scope: OptionScope::OriginRequest,
        display_name: "Proxy listen port",
        description: "cloudflared's local proxy listen port.",
        kind: OptionKind::Int {
            default: Some(0),
            min: Some(0),
            max: Some(65_535),
        },
    },
    OptionSpec {
        yaml_key: "proxyType",
        scope: OptionScope::OriginRequest,
        display_name: "Proxy type",
        description: "Proxy protocol (empty = HTTP).",
        kind: OptionKind::Enum {
            default: None,
            choices: &["", "socks"],
        },
    },
];

// Validate a TunnelOptionValue against its OptionSpec. Used before save.
pub fn validate_value(spec: &OptionSpec, value: &TunnelOptionValue) -> Result<(), String> {
    match (&spec.kind, value) {
        (OptionKind::Bool { .. }, TunnelOptionValue::Bool(_)) => Ok(()),
        (OptionKind::Int { min, max, .. }, TunnelOptionValue::Int(n)) => {
            if let Some(lo) = min {
                if n < lo {
                    return Err(format!("{} < min {}", n, lo));
                }
            }
            if let Some(hi) = max {
                if n > hi {
                    return Err(format!("{} > max {}", n, hi));
                }
            }
            Ok(())
        }
        (OptionKind::String { .. }, TunnelOptionValue::String(_)) => Ok(()),
        (OptionKind::Duration { .. }, TunnelOptionValue::String(s)) => parse_duration(s)
            .map(|_| ())
            .map_err(|e| format!("invalid duration: {}", e)),
        (OptionKind::Enum { choices, .. }, TunnelOptionValue::String(s)) => {
            if choices.contains(&s.as_str()) {
                Ok(())
            } else {
                Err(format!("must be one of: {}", choices.join(", ")))
            }
        }
        (OptionKind::List { .. }, TunnelOptionValue::List(_)) => Ok(()),
        _ => Err("type mismatch between spec and value".into()),
    }
}

// Parse a cloudflared-style duration ("30s", "1m30s", "500ms").
// Minimal parser: accepts sequences of <int><unit> where unit ∈ {ms, s, m, h}.
pub fn parse_duration(s: &str) -> Result<std::time::Duration, String> {
    if s.is_empty() {
        return Err("empty".into());
    }
    let mut total = std::time::Duration::ZERO;
    let mut buf = String::new();
    let mut iter = s.chars().peekable();
    while let Some(c) = iter.next() {
        if c.is_ascii_digit() {
            buf.push(c);
            continue;
        }
        let n: u64 = buf.parse().map_err(|e| format!("bad number: {}", e))?;
        buf.clear();
        let unit = if c == 'm' && iter.peek() == Some(&'s') {
            iter.next();
            "ms"
        } else {
            match c {
                'h' => "h",
                'm' => "m",
                's' => "s",
                other => return Err(format!("bad unit: {}", other)),
            }
        };
        let segment = match unit {
            "ms" => std::time::Duration::from_millis(n),
            "s" => std::time::Duration::from_secs(n),
            "m" => n
                .checked_mul(60)
                .map(std::time::Duration::from_secs)
                .ok_or_else(|| format!("value too large: {}m", n))?,
            "h" => n
                .checked_mul(3600)
                .map(std::time::Duration::from_secs)
                .ok_or_else(|| format!("value too large: {}h", n))?,
            _ => unreachable!(),
        };
        total = total
            .checked_add(segment)
            .ok_or_else(|| "duration sum overflowed".to_string())?;
    }
    if !buf.is_empty() {
        return Err(format!("trailing digits without unit: {}", buf));
    }
    Ok(total)
}

pub fn find(yaml_key: &str) -> Option<&'static OptionSpec> {
    OPTIONS.iter().find(|o| o.yaml_key == yaml_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_yaml_keys_unique() {
        let mut seen = std::collections::HashSet::new();
        for spec in OPTIONS {
            assert!(
                seen.insert(spec.yaml_key),
                "duplicate yaml_key: {}",
                spec.yaml_key
            );
        }
    }

    #[test]
    fn all_specs_have_non_empty_fields() {
        for spec in OPTIONS {
            assert!(!spec.yaml_key.is_empty());
            assert!(!spec.display_name.is_empty());
            assert!(!spec.description.is_empty());
            if let OptionKind::Enum { choices, .. } = &spec.kind {
                assert!(!choices.is_empty(), "enum {} has no choices", spec.yaml_key);
            }
        }
    }

    #[test]
    fn parse_duration_accepts_common_formats() {
        assert_eq!(parse_duration("30s").unwrap().as_secs(), 30);
        assert_eq!(parse_duration("1m").unwrap().as_secs(), 60);
        assert_eq!(parse_duration("1m30s").unwrap().as_secs(), 90);
        assert_eq!(parse_duration("1h2m3s").unwrap().as_secs(), 3723);
        assert_eq!(parse_duration("2h").unwrap().as_secs(), 7200);
        assert_eq!(parse_duration("500ms").unwrap().as_millis(), 500);
    }

    #[test]
    fn parse_duration_rejects_overflow() {
        // u64::MAX seconds converted from minutes would overflow.
        let huge = format!("{}m", u64::MAX);
        assert!(
            parse_duration(&huge).is_err(),
            "should reject overflowing value"
        );
    }

    #[test]
    fn parse_duration_rejects_bad_input() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("30").is_err());
        assert!(parse_duration("30x").is_err());
        assert!(parse_duration("abc").is_err());
    }

    #[test]
    fn validate_int_range() {
        let spec = find("retries").unwrap();
        assert!(validate_value(spec, &TunnelOptionValue::Int(5)).is_ok());
        assert!(validate_value(spec, &TunnelOptionValue::Int(-1)).is_err());
        assert!(validate_value(spec, &TunnelOptionValue::Int(101)).is_err());
    }

    #[test]
    fn validate_enum_membership() {
        // Use transport-loglevel now that loglevel is out of the Advanced registry.
        let spec = find("transport-loglevel").unwrap();
        assert!(validate_value(spec, &TunnelOptionValue::String("debug".into())).is_ok());
        assert!(validate_value(spec, &TunnelOptionValue::String("verbose".into())).is_err());
    }
}

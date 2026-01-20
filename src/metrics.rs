use anyhow::Result;
use std::collections::HashMap;
use std::time::Duration;

/// Metrics collected from cloudflared's Prometheus endpoint
#[derive(Debug, Clone, Default)]
pub struct TunnelMetrics {
    /// Total number of requests handled
    pub total_requests: u64,
    /// Number of request errors
    pub request_errors: u64,
    /// Number of active HA connections to Cloudflare edge
    pub ha_connections: u64,
    /// Number of currently active/concurrent requests
    pub concurrent_requests: u64,
    /// Response counts by status code
    pub response_codes: HashMap<u16, u64>,
    /// Connected edge locations (e.g., "dfw08", "den01")
    pub edge_locations: Vec<String>,
    /// Whether metrics were successfully fetched
    pub available: bool,
}

impl TunnelMetrics {
    /// Fetch metrics from a cloudflared metrics endpoint
    pub async fn fetch(metrics_url: &str) -> Self {
        fetch_metrics_internal(metrics_url)
            .await
            .unwrap_or_default()
    }

    /// Get the list of edge locations as a string
    pub fn locations_string(&self) -> String {
        if self.edge_locations.is_empty() {
            "None".to_string()
        } else {
            self.edge_locations.join(", ")
        }
    }
}

async fn fetch_metrics_internal(metrics_url: &str) -> Result<TunnelMetrics> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;

    let response = client.get(metrics_url).send().await?;
    let text = response.text().await?;

    Ok(parse_prometheus_metrics(&text))
}

/// Parse Prometheus text format metrics
fn parse_prometheus_metrics(text: &str) -> TunnelMetrics {
    let mut metrics = TunnelMetrics {
        available: true,
        ..Default::default()
    };

    for line in text.lines() {
        // Skip comments and empty lines
        if line.starts_with('#') || line.is_empty() {
            continue;
        }

        // Parse cloudflared_tunnel_total_requests
        if line.starts_with("cloudflared_tunnel_total_requests ") {
            if let Some(value) = extract_value(line) {
                metrics.total_requests = value as u64;
            }
        }
        // Parse cloudflared_tunnel_request_errors
        else if line.starts_with("cloudflared_tunnel_request_errors ") {
            if let Some(value) = extract_value(line) {
                metrics.request_errors = value as u64;
            }
        }
        // Parse cloudflared_tunnel_ha_connections
        else if line.starts_with("cloudflared_tunnel_ha_connections ") {
            if let Some(value) = extract_value(line) {
                metrics.ha_connections = value as u64;
            }
        }
        // Parse cloudflared_tunnel_concurrent_requests_per_tunnel
        else if line.starts_with("cloudflared_tunnel_concurrent_requests_per_tunnel ") {
            if let Some(value) = extract_value(line) {
                metrics.concurrent_requests = value as u64;
            }
        }
        // Parse cloudflared_tunnel_response_by_code{status_code="200"} 5
        else if line.starts_with("cloudflared_tunnel_response_by_code{") {
            if let (Some(code), Some(count)) = (extract_status_code(line), extract_value(line)) {
                metrics.response_codes.insert(code, count as u64);
            }
        }
        // Parse cloudflared_tunnel_server_locations{connection_id="0",edge_location="dfw08"} 1
        else if line.starts_with("cloudflared_tunnel_server_locations{") {
            if let Some(location) = extract_edge_location(line) {
                if !metrics.edge_locations.contains(&location) {
                    metrics.edge_locations.push(location);
                }
            }
        }
    }

    // Sort edge locations for consistent display
    metrics.edge_locations.sort();

    metrics
}

/// Extract the numeric value from a Prometheus metric line
fn extract_value(line: &str) -> Option<f64> {
    // Format: metric_name{labels} value or metric_name value
    line.split_whitespace().last()?.parse().ok()
}

/// Extract status code from response_by_code metric
fn extract_status_code(line: &str) -> Option<u16> {
    // Format: cloudflared_tunnel_response_by_code{status_code="200"} 5
    let start = line.find("status_code=\"")? + 13;
    let end = line[start..].find('"')? + start;
    line[start..end].parse().ok()
}

/// Extract edge location from server_locations metric
fn extract_edge_location(line: &str) -> Option<String> {
    // Format: cloudflared_tunnel_server_locations{connection_id="0",edge_location="dfw08"} 1
    let start = line.find("edge_location=\"")? + 15;
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_metrics() {
        let text = r#"
# HELP cloudflared_tunnel_total_requests Total requests
# TYPE cloudflared_tunnel_total_requests counter
cloudflared_tunnel_total_requests 42
cloudflared_tunnel_request_errors 2
cloudflared_tunnel_ha_connections 4
cloudflared_tunnel_concurrent_requests_per_tunnel 1
cloudflared_tunnel_response_by_code{status_code="200"} 35
cloudflared_tunnel_response_by_code{status_code="404"} 5
cloudflared_tunnel_server_locations{connection_id="0",edge_location="dfw08"} 1
cloudflared_tunnel_server_locations{connection_id="1",edge_location="den01"} 1
"#;

        let metrics = parse_prometheus_metrics(text);
        assert!(metrics.available);
        assert_eq!(metrics.total_requests, 42);
        assert_eq!(metrics.request_errors, 2);
        assert_eq!(metrics.ha_connections, 4);
        assert_eq!(metrics.concurrent_requests, 1);
        assert_eq!(metrics.response_codes.get(&200), Some(&35));
        assert_eq!(metrics.response_codes.get(&404), Some(&5));
        assert_eq!(metrics.edge_locations, vec!["den01", "dfw08"]);
    }
}

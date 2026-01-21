use anyhow::{Context, Result};
use rand::Rng;
use serde::{Deserialize, Serialize};

const API_BASE: &str = "https://api.cloudflare.com/client/v4";

fn format_errors(errors: &[ApiError]) -> String {
    errors
        .iter()
        .map(|e| e.message.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

pub struct Client {
    http: reqwest::Client,
    token: String,
}

#[derive(Debug, Deserialize)]
pub struct Zone {
    pub id: String,
    pub name: String,
    #[serde(rename = "account")]
    pub account: Account,
}

#[derive(Debug, Deserialize)]
pub struct Account {
    pub id: String,
}

// Flatten for config storage
impl Zone {
    pub fn into_flat(self) -> FlatZone {
        FlatZone {
            id: self.id,
            name: self.name,
            account_id: self.account.id,
        }
    }
}

pub struct FlatZone {
    pub id: String,
    pub name: String,
    pub account_id: String,
}

#[derive(Debug, Deserialize)]
pub struct Tunnel {
    pub id: String,
    pub name: String,
    pub deleted_at: Option<String>,
}

pub struct TunnelWithCredentials {
    pub tunnel: Tunnel,
    pub credentials_path: std::path::PathBuf,
}

impl Tunnel {
    pub fn credentials_path(&self) -> anyhow::Result<std::path::PathBuf> {
        let config_dir = crate::config::config_dir()?;
        Ok(config_dir.join(format!("{}.json", self.id)))
    }
}

#[derive(Debug, Serialize)]
struct TunnelCredentials {
    #[serde(rename = "AccountTag")]
    account_tag: String,
    #[serde(rename = "TunnelID")]
    tunnel_id: String,
    #[serde(rename = "TunnelSecret")]
    tunnel_secret: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct DnsRecord {
    pub id: String,
    pub name: String,
    pub content: String,
    #[serde(rename = "type")]
    pub record_type: String,
}

#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    success: bool,
    result: Option<T>,
    errors: Vec<ApiError>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    message: String,
}

#[derive(Debug, Serialize)]
struct CreateTunnelRequest {
    name: String,
    tunnel_secret: String,
}

#[derive(Debug, Serialize)]
struct CreateDnsRecordRequest {
    #[serde(rename = "type")]
    record_type: String,
    name: String,
    content: String,
    proxied: bool,
}

impl Client {
    pub fn new(token: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            token: token.to_string(),
        }
    }

    pub async fn list_zones(&self) -> Result<Vec<FlatZone>> {
        let url = format!("{}/zones", API_BASE);
        let resp: ApiResponse<Vec<Zone>> = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .context("Failed to fetch zones")?
            .json()
            .await
            .context("Failed to parse zones response")?;

        if !resp.success {
            anyhow::bail!("Cloudflare API error: {}", format_errors(&resp.errors));
        }

        Ok(resp
            .result
            .unwrap_or_default()
            .into_iter()
            .map(|z| z.into_flat())
            .collect())
    }

    pub async fn list_tunnels(&self, account_id: &str) -> Result<Vec<Tunnel>> {
        let url = format!("{}/accounts/{}/cfd_tunnel", API_BASE, account_id);
        let resp: ApiResponse<Vec<Tunnel>> = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .context("Failed to fetch tunnels")?
            .json()
            .await
            .context("Failed to parse tunnels response")?;

        if !resp.success {
            anyhow::bail!("Cloudflare API error: {}", format_errors(&resp.errors));
        }

        Ok(resp.result.unwrap_or_default())
    }

    pub async fn get_tunnel_by_name(&self, account_id: &str, name: &str) -> Result<Option<Tunnel>> {
        let tunnels = self.list_tunnels(account_id).await?;
        Ok(tunnels
            .into_iter()
            .find(|t| t.name == name && t.deleted_at.is_none()))
    }

    pub async fn create_tunnel(
        &self,
        account_id: &str,
        name: &str,
    ) -> Result<TunnelWithCredentials> {
        let url = format!("{}/accounts/{}/cfd_tunnel", API_BASE, account_id);

        // Generate a random tunnel secret (32 bytes, base64 encoded)
        let mut secret = [0u8; 32];
        rand::rng().fill(&mut secret);
        let secret_b64 = base64_encode(&secret);

        let body = CreateTunnelRequest {
            name: name.to_string(),
            tunnel_secret: secret_b64.clone(),
        };

        let resp: ApiResponse<Tunnel> = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .context("Failed to create tunnel")?
            .json()
            .await
            .context("Failed to parse create tunnel response")?;

        if !resp.success {
            anyhow::bail!("Failed to create tunnel: {}", format_errors(&resp.errors));
        }

        let tunnel = resp.result.context("No tunnel returned from API")?;

        // Save credentials file
        let credentials = TunnelCredentials {
            account_tag: account_id.to_string(),
            tunnel_id: tunnel.id.clone(),
            tunnel_secret: secret_b64,
        };

        let config_dir = crate::config::config_dir()?;
        std::fs::create_dir_all(&config_dir)?;
        let credentials_path = config_dir.join(format!("{}.json", tunnel.id));

        let credentials_json = serde_json::to_string_pretty(&credentials)
            .context("Failed to serialize credentials")?;
        std::fs::write(&credentials_path, credentials_json).with_context(|| {
            format!(
                "Failed to write credentials to {}",
                credentials_path.display()
            )
        })?;

        Ok(TunnelWithCredentials {
            tunnel,
            credentials_path,
        })
    }

    pub async fn delete_tunnel(&self, account_id: &str, tunnel_id: &str) -> Result<()> {
        let url = format!(
            "{}/accounts/{}/cfd_tunnel/{}",
            API_BASE, account_id, tunnel_id
        );

        let resp: ApiResponse<serde_json::Value> = self
            .http
            .delete(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .context("Failed to delete tunnel")?
            .json()
            .await
            .context("Failed to parse delete tunnel response")?;

        if !resp.success {
            anyhow::bail!("Failed to delete tunnel: {}", format_errors(&resp.errors));
        }

        Ok(())
    }

    pub async fn ensure_dns_record(
        &self,
        zone_id: &str,
        hostname: &str,
        tunnel_id: &str,
    ) -> Result<()> {
        let tunnel_cname = format!("{}.cfargotunnel.com", tunnel_id);

        // Check if record exists
        let existing = self.get_dns_record(zone_id, hostname).await?;

        match existing {
            Some(record) if record.content == tunnel_cname => {
                // Already correct
                Ok(())
            }
            Some(record) => {
                // Update existing record
                self.update_dns_record(zone_id, &record.id, hostname, &tunnel_cname)
                    .await
            }
            None => {
                // Create new record
                self.create_dns_record(zone_id, hostname, &tunnel_cname)
                    .await
            }
        }
    }

    async fn get_dns_record(&self, zone_id: &str, name: &str) -> Result<Option<DnsRecord>> {
        let url = format!(
            "{}/zones/{}/dns_records?type=CNAME&name={}",
            API_BASE, zone_id, name
        );
        let resp: ApiResponse<Vec<DnsRecord>> = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .context("Failed to fetch DNS records")?
            .json()
            .await
            .context("Failed to parse DNS records response")?;

        if !resp.success {
            anyhow::bail!(
                "Failed to fetch DNS records: {}",
                format_errors(&resp.errors)
            );
        }

        Ok(resp.result.and_then(|records| records.into_iter().next()))
    }

    async fn create_dns_record(&self, zone_id: &str, name: &str, content: &str) -> Result<()> {
        let url = format!("{}/zones/{}/dns_records", API_BASE, zone_id);
        let body = CreateDnsRecordRequest {
            record_type: "CNAME".to_string(),
            name: name.to_string(),
            content: content.to_string(),
            proxied: true,
        };

        let resp: ApiResponse<DnsRecord> = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .context("Failed to create DNS record")?
            .json()
            .await
            .context("Failed to parse create DNS record response")?;

        if !resp.success {
            anyhow::bail!(
                "Failed to create DNS record: {}",
                format_errors(&resp.errors)
            );
        }

        Ok(())
    }

    async fn update_dns_record(
        &self,
        zone_id: &str,
        record_id: &str,
        name: &str,
        content: &str,
    ) -> Result<()> {
        let url = format!("{}/zones/{}/dns_records/{}", API_BASE, zone_id, record_id);
        let body = CreateDnsRecordRequest {
            record_type: "CNAME".to_string(),
            name: name.to_string(),
            content: content.to_string(),
            proxied: true,
        };

        let resp: ApiResponse<DnsRecord> = self
            .http
            .put(&url)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .context("Failed to update DNS record")?
            .json()
            .await
            .context("Failed to parse update DNS record response")?;

        if !resp.success {
            anyhow::bail!(
                "Failed to update DNS record: {}",
                format_errors(&resp.errors)
            );
        }

        Ok(())
    }

    // Delete a DNS record by hostname
    pub async fn delete_dns_record(&self, zone_id: &str, hostname: &str) -> Result<()> {
        // First find the record
        let record = self.get_dns_record(zone_id, hostname).await?;

        if let Some(record) = record {
            let url = format!("{}/zones/{}/dns_records/{}", API_BASE, zone_id, record.id);

            let resp: ApiResponse<serde_json::Value> = self
                .http
                .delete(&url)
                .bearer_auth(&self.token)
                .send()
                .await
                .context("Failed to delete DNS record")?
                .json()
                .await
                .context("Failed to parse delete DNS record response")?;

            if !resp.success {
                anyhow::bail!(
                    "Failed to delete DNS record: {}",
                    format_errors(&resp.errors)
                );
            }
        }

        Ok(())
    }
}

fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();

    for chunk in data.chunks(3) {
        let n = chunk.len();
        let b0 = chunk[0] as usize;
        let b1 = if n > 1 { chunk[1] as usize } else { 0 };
        let b2 = if n > 2 { chunk[2] as usize } else { 0 };

        result.push(ALPHABET[b0 >> 2] as char);
        result.push(ALPHABET[((b0 & 0x03) << 4) | (b1 >> 4)] as char);

        if n > 1 {
            result.push(ALPHABET[((b1 & 0x0f) << 2) | (b2 >> 6)] as char);
        } else {
            result.push('=');
        }

        if n > 2 {
            result.push(ALPHABET[b2 & 0x3f] as char);
        } else {
            result.push('=');
        }
    }

    result
}

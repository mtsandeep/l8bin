use anyhow::Context;
use serde::{Deserialize, Serialize};

/// Cloudflare DNS API client for managing A and CNAME records.
pub struct CloudflareClient {
    api_token: String,
    zone_id: String,
    client: reqwest::Client,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsRecord {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub record_type: String,
    pub content: String,
    pub proxied: bool,
    pub ttl: i64,
}

/// Cloudflare API response wrapper
#[derive(Debug, Deserialize)]
struct CfResponse<T> {
    success: bool,
    errors: Option<Vec<CfError>>,
    result: Option<T>,
}

#[derive(Debug, Deserialize)]
struct CfError {
    message: String,
}

impl CloudflareClient {
    pub fn new(api_token: &str, zone_id: &str) -> Self {
        Self {
            api_token: api_token.to_string(),
            zone_id: zone_id.to_string(),
            client: reqwest::Client::new(),
        }
    }

    fn base_url(&self) -> String {
        format!(
            "https://api.cloudflare.com/client/v4/zones/{}/dns_records",
            self.zone_id
        )
    }

    /// List DNS records, optionally filtered by name and type.
    pub async fn list_records(
        &self,
        name: &str,
        record_type: &str,
    ) -> anyhow::Result<Vec<DnsRecord>> {
        let url = format!(
            "{}?name={}&type={}",
            self.base_url(),
            urlencoding::encode(name),
            record_type
        );

        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_token))
            .send()
            .await
            .context("cloudflare list_records request failed")?;

        let status = resp.status();
        let raw = resp
            .text()
            .await
            .context("cloudflare list_records: failed to read response body")?;

        let body: CfResponse<Vec<DnsRecord>> =
            serde_json::from_str(&raw).with_context(|| {
                format!(
                    "cloudflare list_records response parse failed (status {}): {}",
                    status, raw
                )
            })?;

        if !body.success {
            let errors = body
                .errors
                .unwrap_or_default()
                .iter()
                .map(|e| e.message.clone())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!("cloudflare list_records failed: {}", errors);
        }

        Ok(body.result.unwrap_or_default())
    }

    /// List all DNS records in the zone matching a name suffix (e.g. ".l8b.in").
    /// Used for cleanup — find stale records to delete.
    pub async fn list_records_by_suffix(
        &self,
        suffix: &str,
        record_type: &str,
    ) -> anyhow::Result<Vec<DnsRecord>> {
        // Cloudflare API doesn't support suffix filtering natively.
        // We list all records of the given type and filter client-side.
        let url = format!(
            "{}?type={}&per_page=5000",
            self.base_url(),
            record_type
        );

        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_token))
            .send()
            .await
            .context("cloudflare list_records_by_suffix request failed")?;

        let status = resp.status();
        let raw = resp
            .text()
            .await
            .context("cloudflare list_records_by_suffix: failed to read response body")?;

        let body: CfResponse<Vec<DnsRecord>> =
            serde_json::from_str(&raw).with_context(|| {
                format!(
                    "cloudflare list_records_by_suffix response parse failed (status {}): {}",
                    status, raw
                )
            })?;

        if !body.success {
            let errors = body
                .errors
                .unwrap_or_default()
                .iter()
                .map(|e| e.message.clone())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!("cloudflare list_records_by_suffix failed: {}", errors);
        }

        let records = body
            .result
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.name.ends_with(suffix))
            .collect();

        Ok(records)
    }

    /// Create or update a DNS record. If a record with the same name and type exists,
    /// it is updated (PUT). Otherwise, a new record is created (POST).
    /// Returns true if a new record was created, false if it already existed or was updated.
    pub async fn upsert_record(
        &self,
        name: &str,
        record_type: &str,
        content: &str,
        ttl: i64,
        proxied: bool,
    ) -> anyhow::Result<bool> {
        let existing = self.list_records(name, record_type).await?;

        if let Some(record) = existing.first() {
            // Update existing record
            let url = format!("{}/{}", self.base_url(), record.id);
            let body = serde_json::json!({
                "type": record_type,
                "name": name,
                "content": content,
                "ttl": ttl,
                "proxied": proxied
            });

            let resp = self
                .client
                .put(&url)
                .header("Authorization", format!("Bearer {}", self.api_token))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .context("cloudflare upsert PUT failed")?;

            let status = resp.status();
            let raw = resp
                .text()
                .await
                .context("cloudflare upsert PUT: failed to read response body")?;

            let cf_resp: CfResponse<DnsRecord> =
                serde_json::from_str(&raw).with_context(|| {
                    format!(
                        "cloudflare upsert PUT response parse failed (status {}): {}",
                        status, raw
                    )
                })?;

            if !cf_resp.success {
                let errors = cf_resp
                    .errors
                    .unwrap_or_default()
                    .iter()
                    .map(|e| e.message.clone())
                    .collect::<Vec<_>>()
                    .join(", ");
                anyhow::bail!("cloudflare upsert PUT failed: {}", errors);
            }

            tracing::info!(name, record_type, content, "updated DNS record");
            Ok(false)
        } else {
            // Create new record
            let body = serde_json::json!({
                "type": record_type,
                "name": name,
                "content": content,
                "ttl": ttl,
                "proxied": proxied
            });

            let resp = self
                .client
                .post(self.base_url())
                .header("Authorization", format!("Bearer {}", self.api_token))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .context("cloudflare upsert POST failed")?;

            let status = resp.status();
            let raw = resp
                .text()
                .await
                .context("cloudflare upsert POST: failed to read response body")?;

            let cf_resp: CfResponse<DnsRecord> =
                serde_json::from_str(&raw).with_context(|| {
                    format!(
                        "cloudflare upsert POST response parse failed (status {}): {}",
                        status, raw
                    )
                })?;

            if !cf_resp.success {
                let errors = cf_resp
                    .errors
                    .unwrap_or_default()
                    .iter()
                    .map(|e| e.message.clone())
                    .collect::<Vec<_>>()
                    .join(", ");
                if errors.contains("An identical record already exists") {
                    tracing::info!(name, record_type, content, "DNS record already exists");
                    return Ok(false);
                }
                anyhow::bail!("cloudflare upsert POST failed: {}", errors);
            }

            tracing::info!(name, record_type, content, "created DNS record");
            Ok(true)
        }
    }

    /// Delete a DNS record by its Cloudflare record ID.
    pub async fn delete_record(&self, record_id: &str) -> anyhow::Result<()> {
        let url = format!("{}/{}", self.base_url(), record_id);

        let resp = self
            .client
            .delete(&url)
            .header("Authorization", format!("Bearer {}", self.api_token))
            .send()
            .await
            .context("cloudflare delete_record request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("cloudflare delete_record failed ({}): {}", status, body);
        }

        tracing::info!(record_id, "deleted DNS record");
        Ok(())
    }
}

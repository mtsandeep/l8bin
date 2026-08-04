//! Platform hostname settings: DB is source of truth after install seed from env.

use std::sync::{Arc, RwLock};

use sqlx::SqlitePool;

use crate::config::Config;

#[derive(Debug, Clone)]
pub struct PlatformSettings {
    pub domain: String,
    pub dashboard_subdomain: String,
    pub poke_subdomain: String,
    pub dns_target: String,
}

impl PlatformSettings {
    pub fn is_tryout_domain(domain: &str) -> bool {
        let d = domain.trim().to_ascii_lowercase();
        d.ends_with(".sslip.io") || d.ends_with(".nip.io")
    }

    pub fn tryout(&self) -> bool {
        Self::is_tryout_domain(&self.domain)
    }
}

/// Thread-safe snapshot of platform hostname settings.
#[derive(Clone)]
pub struct PlatformHandle {
    inner: Arc<RwLock<PlatformSettings>>,
}

impl PlatformHandle {
    pub fn new(settings: PlatformSettings) -> Self {
        Self { inner: Arc::new(RwLock::new(settings)) }
    }

    pub fn snapshot(&self) -> PlatformSettings {
        self.inner.read().expect("platform settings lock").clone()
    }

    pub fn domain(&self) -> String {
        self.inner.read().expect("platform settings lock").domain.clone()
    }

    pub fn dashboard_subdomain(&self) -> String {
        self.inner.read().expect("platform settings lock").dashboard_subdomain.clone()
    }

    pub fn poke_subdomain(&self) -> String {
        self.inner.read().expect("platform settings lock").poke_subdomain.clone()
    }

    pub fn dns_target(&self) -> String {
        self.inner.read().expect("platform settings lock").dns_target.clone()
    }

    pub fn update<F>(&self, f: F)
    where
        F: FnOnce(&mut PlatformSettings),
    {
        let mut guard = self.inner.write().expect("platform settings lock");
        f(&mut guard);
    }

    pub fn set_domain(&self, domain: String) {
        self.update(|s| s.domain = domain);
    }

    pub fn set_dashboard_subdomain(&self, dashboard_subdomain: String) {
        self.update(|s| s.dashboard_subdomain = dashboard_subdomain);
    }

    pub fn set_poke_subdomain(&self, poke_subdomain: String) {
        self.update(|s| s.poke_subdomain = poke_subdomain);
    }

    pub fn set_dns_target(&self, dns_target: String) {
        self.update(|s| s.dns_target = dns_target);
    }
}

/// Seed missing platform keys from env config, then load into memory.
pub async fn load_platform_settings(db: &SqlitePool, config: &Config) -> anyhow::Result<PlatformHandle> {
    sqlx::query(
        "INSERT OR IGNORE INTO settings (key, value) VALUES
         ('domain', ?),
         ('dashboard_subdomain', ?),
         ('poke_subdomain', ?),
         ('dns_target', ?)",
    )
    .bind(&config.domain)
    .bind(&config.dashboard_subdomain)
    .bind(&config.poke_subdomain)
    .bind("")
    .execute(db)
    .await?;

    let domain = setting_or(db, "domain", &config.domain).await?;
    let dashboard_subdomain = setting_or(db, "dashboard_subdomain", &config.dashboard_subdomain).await?;
    let poke_subdomain = setting_or(db, "poke_subdomain", &config.poke_subdomain).await?;
    let dns_target = setting_or(db, "dns_target", "").await?;

    tracing::info!(
        domain = %domain,
        dashboard = %dashboard_subdomain,
        poke = %poke_subdomain,
        tryout = PlatformSettings::is_tryout_domain(&domain),
        "platform settings loaded"
    );

    Ok(PlatformHandle::new(PlatformSettings {
        domain,
        dashboard_subdomain,
        poke_subdomain,
        dns_target,
    }))
}

async fn setting_or(db: &SqlitePool, key: &str, fallback: &str) -> anyhow::Result<String> {
    let value: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = ?").bind(key).fetch_optional(db).await?;
    Ok(value.filter(|v| !v.is_empty()).unwrap_or_else(|| fallback.to_string()))
}

/// Normalize a platform domain hostname (lowercase, trim, strip trailing dot).
pub fn normalize_domain(raw: &str) -> Result<String, String> {
    let mut d = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if d.is_empty() {
        return Err("domain must not be empty".into());
    }
    if d.contains("://") || d.contains('/') || d.contains(' ') {
        return Err("domain must be a hostname only (e.g. example.com or apps.example.com)".into());
    }
    if d.starts_with('.') || d.ends_with('.') || d.contains("..") {
        return Err("invalid domain format".into());
    }
    // Strip accidental leading wildcard
    if let Some(rest) = d.strip_prefix("*.") {
        d = rest.to_string();
    }
    if !d.contains('.') && d != "localhost" {
        return Err("domain must include a TLD (e.g. example.com) or be localhost".into());
    }
    Ok(d)
}

pub fn normalize_subdomain_label(raw: &str) -> Result<String, String> {
    let s = raw.trim().to_ascii_lowercase();
    if s.is_empty() {
        return Err("subdomain must not be empty".into());
    }
    if s.contains('.') {
        return Err("subdomain must not contain dots".into());
    }
    if !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("subdomain may only contain letters, numbers, hyphens, and underscores".into());
    }
    Ok(s)
}

/// Status of a platform domain-change job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DomainJobStatus {
    Pending,
    Running,
    Failed,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DomainStepStatus {
    Pending,
    Running,
    Done,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct DomainJobStep {
    pub id: String,
    pub label: String,
    pub status: DomainStepStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct DomainJob {
    pub id: String,
    pub status: DomainJobStatus,
    pub domain: String,
    pub old_domain: String,
    pub steps: Vec<DomainJobStep>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Index of the step to resume from on retry.
    pub resume_from: usize,
    pub dashboard_url: String,
}

impl DomainJob {
    pub fn new(id: String, old_domain: String, domain: String, dashboard_subdomain: &str) -> Self {
        let steps = vec![
            DomainJobStep {
                id: "persist".into(),
                label: "Save new domain".into(),
                status: DomainStepStatus::Pending,
                error: None,
            },
            DomainJobStep {
                id: "routes".into(),
                label: "Rebuild master routes".into(),
                status: DomainStepStatus::Pending,
                error: None,
            },
            DomainJobStep {
                id: "agents".into(),
                label: "Re-register agents".into(),
                status: DomainStepStatus::Pending,
                error: None,
            },
            DomainJobStep {
                id: "agent_caddy".into(),
                label: "Push agent Caddy configs".into(),
                status: DomainStepStatus::Pending,
                error: None,
            },
            DomainJobStep {
                id: "dns".into(),
                label: "Update Cloudflare DNS".into(),
                status: DomainStepStatus::Pending,
                error: None,
            },
        ];
        Self {
            id,
            status: DomainJobStatus::Pending,
            domain: domain.clone(),
            old_domain,
            steps,
            error: None,
            resume_from: 0,
            dashboard_url: format!("https://{}.{}", dashboard_subdomain, domain),
        }
    }
}

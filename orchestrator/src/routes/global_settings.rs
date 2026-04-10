use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

use crate::{AppState, config::Config};

#[derive(Debug, Serialize, Deserialize)]
pub struct GlobalSettings {
    pub default_memory_limit_mb: i64,
    pub default_cpu_limit: f64,
    pub projects_dir: String,
    pub domain: String,
    pub dns_target: String,
    pub routing_mode: String,
    pub cloudflare_api_token: String,
    pub cloudflare_zone_id: String,
    pub dashboard_subdomain: String,
    pub poke_subdomain: String,
}

pub fn resolve_projects_dir() -> String {
    std::fs::canonicalize("projects")
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "projects".to_string())
}

#[derive(Deserialize)]
pub struct UpdateGlobalSettings {
    pub default_memory_limit_mb: Option<i64>,
    pub default_cpu_limit: Option<f64>,
    pub domain: Option<String>,
    pub dns_target: Option<String>,
    pub routing_mode: Option<String>,
    pub cloudflare_api_token: Option<String>,
    pub cloudflare_zone_id: Option<String>,
    pub dashboard_subdomain: Option<String>,
    pub poke_subdomain: Option<String>,
}

pub async fn get_settings(
    State(state): State<AppState>,
) -> Result<Json<GlobalSettings>, (StatusCode, String)> {
    let settings = load_settings(&state.db, &state.config).await?;
    Ok(Json(settings))
}

pub async fn update_settings(
    State(state): State<AppState>,
    Json(payload): Json<UpdateGlobalSettings>,
) -> Result<Json<GlobalSettings>, (StatusCode, String)> {
    if let Some(mem) = payload.default_memory_limit_mb {
        if mem < 64 {
            return Err((StatusCode::BAD_REQUEST, "memory must be at least 64 MB".into()));
        }
        upsert_setting(&state.db, "default_memory_limit_mb", &mem.to_string()).await?;
    }
    if let Some(cpu) = payload.default_cpu_limit {
        if cpu <= 0.0 {
            return Err((StatusCode::BAD_REQUEST, "cpu_limit must be > 0".into()));
        }
        upsert_setting(&state.db, "default_cpu_limit", &cpu.to_string()).await?;
    }
    if let Some(domain) = payload.domain {
        let domain = domain.trim().to_string();
        if domain.is_empty() {
            return Err((StatusCode::BAD_REQUEST, "domain must not be empty".into()));
        }
        upsert_setting(&state.db, "domain", &domain).await?;
    }
    if let Some(dns_target) = payload.dns_target {
        let dns_target = dns_target.trim().to_string();
        if dns_target.is_empty() {
            return Err((StatusCode::BAD_REQUEST, "dns_target must not be empty".into()));
        }
        upsert_setting(&state.db, "dns_target", &dns_target).await?;
    }
    if let Some(routing_mode) = payload.routing_mode {
        let routing_mode = routing_mode.trim().to_string();
        if !["master_proxy", "cloudflare_dns"].contains(&routing_mode.as_str()) {
            return Err((StatusCode::BAD_REQUEST, "routing_mode must be 'master_proxy' or 'cloudflare_dns'".into()));
        }
        upsert_setting(&state.db, "routing_mode", &routing_mode).await?;

        // Hot-swap the router
        let cf_token = get_setting(&state.db, "cloudflare_api_token").await?.unwrap_or_default();
        let cf_zone = get_setting(&state.db, "cloudflare_zone_id").await?.unwrap_or_default();

        let new_router = crate::build_routing_provider(
            &routing_mode,
            &cf_token,
            &cf_zone,
            &state.config.caddy_admin_url,
            state.node_clients.clone(),
            state.db.clone(),
            state.config.clone(),
        );

        {
            let mut guard = state.router.write().await;
            *guard = new_router;
        }
        tracing::info!(routing_mode = %routing_mode, "router hot-swapped");

        // Trigger full sync on new router
        let orchestrator_upstream = format!("litebin-orchestrator:{}", state.config.port);
        let routes = crate::routing_helpers::resolve_all_routes(
            &state.db, &state.config.domain, &orchestrator_upstream,
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        if let Err(e) = state.router.read().await.sync_routes(
            &routes,
            &state.config.domain,
            &orchestrator_upstream,
            &state.config.dashboard_subdomain,
            &state.config.poke_subdomain,
        ).await {
            tracing::warn!(error = %e, "post-swap sync_routes failed");
        }
    }
    if let Some(cloudflare_api_token) = payload.cloudflare_api_token {
        upsert_setting(&state.db, "cloudflare_api_token", &cloudflare_api_token).await?;
    }
    if let Some(cloudflare_zone_id) = payload.cloudflare_zone_id {
        upsert_setting(&state.db, "cloudflare_zone_id", &cloudflare_zone_id).await?;
    }
    if let Some(dashboard_subdomain) = payload.dashboard_subdomain {
        let dashboard_subdomain = dashboard_subdomain.trim().to_string();
        if dashboard_subdomain.is_empty() {
            return Err((StatusCode::BAD_REQUEST, "dashboard_subdomain must not be empty".into()));
        }
        if dashboard_subdomain.contains('.') {
            return Err((StatusCode::BAD_REQUEST, "dashboard_subdomain must not contain dots".into()));
        }
        upsert_setting(&state.db, "dashboard_subdomain", &dashboard_subdomain).await?;
    }
    if let Some(poke_subdomain) = payload.poke_subdomain {
        let poke_subdomain = poke_subdomain.trim().to_string();
        if poke_subdomain.is_empty() {
            return Err((StatusCode::BAD_REQUEST, "poke_subdomain must not be empty".into()));
        }
        if poke_subdomain.contains('.') {
            return Err((StatusCode::BAD_REQUEST, "poke_subdomain must not contain dots".into()));
        }
        upsert_setting(&state.db, "poke_subdomain", &poke_subdomain).await?;
    }
    let settings = load_settings(&state.db, &state.config).await?;
    Ok(Json(settings))
}

pub async fn load_settings(db: &sqlx::SqlitePool, config: &Config) -> Result<GlobalSettings, (StatusCode, String)> {
    let mem: i64 = get_setting(db, "default_memory_limit_mb").await?.as_deref().unwrap_or("256").parse().unwrap_or(256);
    let cpu: f64 = get_setting(db, "default_cpu_limit").await?.as_deref().unwrap_or("0.5").parse().unwrap_or(0.5);
    let dns_target: String = get_setting(db, "dns_target").await?.unwrap_or_default();
    let routing_mode: String = get_setting(db, "routing_mode").await?.unwrap_or_else(|| config.routing_mode.clone());
    let cloudflare_api_token: String = get_setting(db, "cloudflare_api_token").await?.unwrap_or_else(|| config.cloudflare_api_token.clone());
    let cloudflare_zone_id: String = get_setting(db, "cloudflare_zone_id").await?.unwrap_or_else(|| config.cloudflare_zone_id.clone());
    let dashboard_subdomain: String = get_setting(db, "dashboard_subdomain").await?.unwrap_or_else(|| config.dashboard_subdomain.clone());
    let poke_subdomain: String = get_setting(db, "poke_subdomain").await?.unwrap_or_else(|| config.poke_subdomain.clone());
    Ok(GlobalSettings {
        default_memory_limit_mb: mem,
        default_cpu_limit: cpu,
        projects_dir: resolve_projects_dir(),
        domain: config.domain.clone(),
        dns_target,
        routing_mode,
        cloudflare_api_token,
        cloudflare_zone_id,
        dashboard_subdomain,
        poke_subdomain,
    })
}

async fn get_setting(db: &sqlx::SqlitePool, key: &str) -> Result<Option<String>, (StatusCode, String)> {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn upsert_setting(db: &sqlx::SqlitePool, key: &str, value: &str) -> Result<(), (StatusCode, String)> {
    sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
        .bind(key)
        .bind(value)
        .execute(db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(())
}

#[derive(Serialize)]
pub struct CleanupDnsResponse {
    pub deleted_count: usize,
}

pub async fn cleanup_dns(
    State(state): State<AppState>,
) -> Result<Json<CleanupDnsResponse>, (StatusCode, String)> {
    let cf_token = get_setting(&state.db, "cloudflare_api_token")
        .await?
        .unwrap_or_default();
    let cf_zone = get_setting(&state.db, "cloudflare_zone_id")
        .await?
        .unwrap_or_default();

    if cf_token.is_empty() || cf_zone.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Cloudflare API token and Zone ID must be configured".into(),
        ));
    }

    let domain = state.config.domain.clone();
    let suffix = format!(".{}", domain);

    let cloudflare = litebin_common::cloudflare::CloudflareClient::new(&cf_token, &cf_zone);
    let records = cloudflare
        .list_records_by_suffix(&suffix, "A")
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut deleted_count = 0usize;
    for record in &records {
        match cloudflare.delete_record(&record.id).await {
            Ok(_) => deleted_count += 1,
            Err(e) => {
                tracing::warn!(record = %record.name, error = %e, "failed to delete DNS record during cleanup");
            }
        }
    }

    tracing::info!(deleted = deleted_count, total = records.len(), "DNS cleanup complete");
    Ok(Json(CleanupDnsResponse { deleted_count }))
}

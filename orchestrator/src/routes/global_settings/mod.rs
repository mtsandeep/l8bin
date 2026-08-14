mod dns_ops;
mod domain_job;

use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::platform::normalize_subdomain_label;
use litebin_common::types::RoutingMode;

pub use dns_ops::*;
pub use domain_job::*;

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
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
    /// True when platform domain is sslip.io / nip.io tryout DNS.
    pub tryout: bool,
}

pub fn resolve_projects_dir() -> String {
    "projects".to_string()
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct UpdateGlobalSettings {
    pub default_memory_limit_mb: Option<i64>,
    pub default_cpu_limit: Option<f64>,
    /// Ignored on PATCH — use /settings/domain/apply instead.
    pub domain: Option<String>,
    pub dns_target: Option<String>,
    pub routing_mode: Option<String>,
    pub cloudflare_api_token: Option<String>,
    pub cloudflare_zone_id: Option<String>,
    pub dashboard_subdomain: Option<String>,
    pub poke_subdomain: Option<String>,
}

#[utoipa::path(
    get,
    path = "/settings",
    responses(
        (status = 200, body = GlobalSettings),
        (status = 500),
    ),
    tag = "global-settings",
    security(("session_auth" = []))
)]
pub async fn get_settings(State(state): State<AppState>) -> Result<Json<GlobalSettings>, (StatusCode, String)> {
    let settings = load_settings(&state).await?;
    Ok(Json(settings))
}

#[utoipa::path(
    patch,
    path = "/settings",
    request_body = UpdateGlobalSettings,
    responses(
        (status = 200, body = GlobalSettings),
        (status = 400),
        (status = 500),
    ),
    tag = "global-settings",
    security(("session_auth" = []))
)]
pub async fn update_settings(
    State(state): State<AppState>,
    Json(payload): Json<UpdateGlobalSettings>,
) -> Result<Json<GlobalSettings>, (StatusCode, String)> {
    if payload.domain.is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            "domain cannot be changed via PATCH /settings; use POST /settings/domain/apply".into(),
        ));
    }

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

    // Update DockerManager defaults so new containers use the latest values
    let mem_setting: i64 =
        get_setting(&state.db, "default_memory_limit_mb").await?.as_deref().unwrap_or("256").parse().unwrap_or(256);
    let cpu_setting: f64 =
        get_setting(&state.db, "default_cpu_limit").await?.as_deref().unwrap_or("0.5").parse().unwrap_or(0.5);
    state.docker.update_defaults(mem_setting * 1024 * 1024, cpu_setting);

    if let Some(dns_target) = payload.dns_target {
        let dns_target = dns_target.trim().to_string();
        upsert_setting(&state.db, "dns_target", &dns_target).await?;
        state.platform.set_dns_target(dns_target);
    }

    let mut need_route_sync = false;

    if let Some(routing_mode) = payload.routing_mode {
        let routing_mode = routing_mode.trim().to_string();
        if !["master_proxy", "cloudflare_dns"].contains(&routing_mode.as_str()) {
            return Err((StatusCode::BAD_REQUEST, "routing_mode must be 'master_proxy' or 'cloudflare_dns'".into()));
        }
        let routing_mode_enum: RoutingMode = match routing_mode.as_str() {
            "cloudflare_dns" => RoutingMode::CloudflareDns,
            _ => RoutingMode::MasterProxy,
        };
        upsert_setting(&state.db, "routing_mode", &routing_mode).await?;

        // Save Cloudflare credentials before hot-swap so the router reads the latest values
        if let Some(cloudflare_api_token) = payload.cloudflare_api_token {
            upsert_setting(&state.db, "cloudflare_api_token", &cloudflare_api_token).await?;
        }
        if let Some(cloudflare_zone_id) = payload.cloudflare_zone_id {
            upsert_setting(&state.db, "cloudflare_zone_id", &cloudflare_zone_id).await?;
        }

        // Hot-swap the router
        let cf_token = get_setting(&state.db, "cloudflare_api_token").await?.unwrap_or_default();
        let cf_zone = get_setting(&state.db, "cloudflare_zone_id").await?.unwrap_or_default();

        let new_router = crate::routing_helpers::build_routing_provider(
            &routing_mode_enum,
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
        need_route_sync = true;
    } else {
        if let Some(cloudflare_api_token) = payload.cloudflare_api_token {
            upsert_setting(&state.db, "cloudflare_api_token", &cloudflare_api_token).await?;
        }
        if let Some(cloudflare_zone_id) = payload.cloudflare_zone_id {
            upsert_setting(&state.db, "cloudflare_zone_id", &cloudflare_zone_id).await?;
        }
    }

    if let Some(dashboard_subdomain) = payload.dashboard_subdomain {
        let dashboard_subdomain =
            normalize_subdomain_label(&dashboard_subdomain).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
        let poke = state.platform.poke_subdomain();
        if dashboard_subdomain == poke {
            return Err((StatusCode::BAD_REQUEST, "dashboard_subdomain must not equal poke_subdomain".into()));
        }
        upsert_setting(&state.db, "dashboard_subdomain", &dashboard_subdomain).await?;
        state.platform.set_dashboard_subdomain(dashboard_subdomain);
        need_route_sync = true;
    }
    if let Some(poke_subdomain) = payload.poke_subdomain {
        let poke_subdomain = normalize_subdomain_label(&poke_subdomain).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
        let dash = state.platform.dashboard_subdomain();
        if poke_subdomain == dash {
            return Err((StatusCode::BAD_REQUEST, "poke_subdomain must not equal dashboard_subdomain".into()));
        }
        upsert_setting(&state.db, "poke_subdomain", &poke_subdomain).await?;
        state.platform.set_poke_subdomain(poke_subdomain);
        need_route_sync = true;
    }

    if need_route_sync {
        sync_platform_routes(&state, true).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        // Re-register agents so host filters / wake URLs stay correct
        let (_ok, errs) = crate::routes::nodes::reregister_online_agents(&state).await;
        for e in errs {
            tracing::warn!(error = %e, "agent re-register after settings update failed");
        }
    }

    let settings = load_settings(&state).await?;
    Ok(Json(settings))
}

pub async fn load_settings(state: &AppState) -> Result<GlobalSettings, (StatusCode, String)> {
    let mem: i64 =
        get_setting(&state.db, "default_memory_limit_mb").await?.as_deref().unwrap_or("256").parse().unwrap_or(256);
    let cpu: f64 =
        get_setting(&state.db, "default_cpu_limit").await?.as_deref().unwrap_or("0.5").parse().unwrap_or(0.5);
    let routing_mode: String =
        get_setting(&state.db, "routing_mode").await?.unwrap_or_else(|| state.config.routing_mode.to_string());
    let cloudflare_api_token: String = get_setting(&state.db, "cloudflare_api_token")
        .await?
        .unwrap_or_else(|| state.config.cloudflare_api_token.clone());
    let cloudflare_zone_id: String =
        get_setting(&state.db, "cloudflare_zone_id").await?.unwrap_or_else(|| state.config.cloudflare_zone_id.clone());

    let snap = state.platform.snapshot();
    Ok(GlobalSettings {
        default_memory_limit_mb: mem,
        default_cpu_limit: cpu,
        projects_dir: resolve_projects_dir(),
        domain: snap.domain.clone(),
        dns_target: snap.dns_target,
        routing_mode,
        cloudflare_api_token,
        cloudflare_zone_id,
        dashboard_subdomain: snap.dashboard_subdomain,
        poke_subdomain: snap.poke_subdomain,
        tryout: crate::platform::PlatformSettings::is_tryout_domain(&snap.domain),
    })
}

pub(crate) async fn get_setting(db: &sqlx::SqlitePool, key: &str) -> Result<Option<String>, (StatusCode, String)> {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

pub(crate) async fn upsert_setting(db: &sqlx::SqlitePool, key: &str, value: &str) -> Result<(), (StatusCode, String)> {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(())
}

pub(super) async fn sync_platform_routes(state: &AppState, sync_dns: bool) -> Result<(), String> {
    let snap = state.platform.snapshot();
    let orchestrator_upstream = format!("litebin-orchestrator:{}", state.config.port);
    let routes = crate::routing_helpers::resolve_all_routes(&state.db, &snap.domain, &orchestrator_upstream)
        .await
        .map_err(|e| e.to_string())?;

    state
        .router
        .read()
        .await
        .sync_routes(
            &routes,
            &snap.domain,
            &orchestrator_upstream,
            &snap.dashboard_subdomain,
            &snap.poke_subdomain,
            sync_dns,
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

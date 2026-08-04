use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::platform::{
    DomainJob, DomainJobStatus, DomainStepStatus, normalize_domain, normalize_subdomain_label,
};
use crate::AppState;
use litebin_common::cloudflare::CloudflareClient;
use litebin_common::types::RoutingMode;

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

        let new_router = crate::build_routing_provider(
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
        let dashboard_subdomain = normalize_subdomain_label(&dashboard_subdomain)
            .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
        let poke = state.platform.poke_subdomain();
        if dashboard_subdomain == poke {
            return Err((StatusCode::BAD_REQUEST, "dashboard_subdomain must not equal poke_subdomain".into()));
        }
        upsert_setting(&state.db, "dashboard_subdomain", &dashboard_subdomain).await?;
        state.platform.set_dashboard_subdomain(dashboard_subdomain);
        need_route_sync = true;
    }
    if let Some(poke_subdomain) = payload.poke_subdomain {
        let poke_subdomain =
            normalize_subdomain_label(&poke_subdomain).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
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
    let cloudflare_zone_id: String = get_setting(&state.db, "cloudflare_zone_id")
        .await?
        .unwrap_or_else(|| state.config.cloudflare_zone_id.clone());

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

async fn sync_platform_routes(state: &AppState, sync_dns: bool) -> Result<(), String> {
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

#[derive(Serialize, utoipa::ToSchema)]
pub struct CleanupDnsResponse {
    pub deleted_count: usize,
}

#[utoipa::path(
    post,
    path = "/settings/cleanup-dns",
    responses(
        (status = 200, body = CleanupDnsResponse),
        (status = 400),
        (status = 500),
    ),
    tag = "global-settings",
    security(("session_auth" = []))
)]
pub async fn cleanup_dns(State(state): State<AppState>) -> Result<Json<CleanupDnsResponse>, (StatusCode, String)> {
    let cf_token = get_setting(&state.db, "cloudflare_api_token").await?.unwrap_or_default();
    let cf_zone = get_setting(&state.db, "cloudflare_zone_id").await?.unwrap_or_default();

    if cf_token.is_empty() || cf_zone.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Cloudflare API token and Zone ID must be configured".into()));
    }

    let domain = state.platform.domain();
    let suffix = format!(".{}", domain);

    let cloudflare = CloudflareClient::new(&cf_token, &cf_zone);
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

#[derive(Serialize, utoipa::ToSchema)]
pub struct SyncDnsResponse {
    pub created: usize,
    pub deleted: usize,
    pub unchanged: usize,
    pub errors: usize,
}

#[utoipa::path(
    post,
    path = "/settings/sync-dns",
    responses(
        (status = 200, body = SyncDnsResponse),
        (status = 400),
        (status = 500),
    ),
    tag = "global-settings",
    security(("session_auth" = []))
)]
pub async fn sync_dns(State(state): State<AppState>) -> Result<Json<SyncDnsResponse>, (StatusCode, String)> {
    let cf_token = get_setting(&state.db, "cloudflare_api_token").await?.unwrap_or_default();
    let cf_zone = get_setting(&state.db, "cloudflare_zone_id").await?.unwrap_or_default();

    if cf_token.is_empty() || cf_zone.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Cloudflare API token and Zone ID must be configured".into()));
    }

    let snap = state.platform.snapshot();
    let orchestrator_upstream = format!("litebin-orchestrator:{}", state.config.port);

    let routes = crate::routing_helpers::resolve_all_routes(&state.db, &snap.domain, &orchestrator_upstream)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let result = state
        .router
        .read()
        .await
        .sync_dns_only(&routes, &snap.domain, &snap.dashboard_subdomain, &snap.poke_subdomain)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(SyncDnsResponse {
        created: result.created,
        deleted: result.deleted,
        unchanged: result.unchanged,
        errors: result.errors,
    }))
}

// --- Domain change preflight / apply / job status ---

#[derive(Deserialize, utoipa::ToSchema)]
pub struct DomainPreflightRequest {
    pub domain: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct DomainPreflightResponse {
    pub ok: bool,
    pub domain: String,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

#[utoipa::path(
    post,
    path = "/settings/domain/preflight",
    request_body = DomainPreflightRequest,
    responses(
        (status = 200, body = DomainPreflightResponse),
        (status = 400),
        (status = 500),
    ),
    tag = "global-settings",
    security(("session_auth" = []))
)]
pub async fn domain_preflight(
    State(state): State<AppState>,
    Json(payload): Json<DomainPreflightRequest>,
) -> Result<Json<DomainPreflightResponse>, (StatusCode, String)> {
    let domain = normalize_domain(&payload.domain).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let (errors, warnings) = run_domain_preflight(&state, &domain).await;
    Ok(Json(DomainPreflightResponse { ok: errors.is_empty(), domain, errors, warnings }))
}

async fn run_domain_preflight(state: &AppState, domain: &str) -> (Vec<String>, Vec<String>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let current = state.platform.domain();
    if domain == current {
        errors.push("new domain is the same as the current domain".into());
        return (errors, warnings);
    }

    if crate::platform::PlatformSettings::is_tryout_domain(domain) {
        warnings.push(
            "This looks like tryout DNS (sslip.io / nip.io). Let's Encrypt may fail; not recommended for production."
                .into(),
        );
    }

    let routing_mode = get_setting(&state.db, "routing_mode")
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| state.config.routing_mode.to_string());

    if routing_mode == "cloudflare_dns" {
        let cf_token = get_setting(&state.db, "cloudflare_api_token").await.ok().flatten().unwrap_or_default();
        let cf_zone = get_setting(&state.db, "cloudflare_zone_id").await.ok().flatten().unwrap_or_default();
        if cf_token.is_empty() || cf_zone.is_empty() {
            errors.push("Cloudflare API token and Zone ID must be configured for cloudflare_dns mode".into());
        } else {
            let cf = CloudflareClient::new(&cf_token, &cf_zone);
            match cf.get_zone_name().await {
                Ok(zone_name) => {
                    if !CloudflareClient::zone_covers_hostname(&zone_name, domain) {
                        errors.push(format!(
                            "Cloudflare zone '{zone_name}' does not cover '{domain}'. Use a subdomain of the zone or change the zone."
                        ));
                    }
                    // Prove token can list records
                    if let Err(e) = cf.list_records_by_suffix(&format!(".{domain}"), "A").await {
                        errors.push(format!("Cloudflare token cannot list DNS records: {e}"));
                    }
                }
                Err(e) => {
                    errors.push(format!("Cannot read Cloudflare zone (check token and zone ID): {e}"));
                }
            }
        }
    } else {
        // Master proxy: DNS resolve mismatch is a warning
        let dashboard = state.platform.dashboard_subdomain();
        let check_host = format!("{dashboard}.{domain}");
        let expected = {
            let dns_target = state.platform.dns_target();
            if !dns_target.is_empty() {
                dns_target
            } else {
                state.config.public_ip.clone()
            }
        };
        if expected.is_empty() {
            warnings.push(
                "Could not determine this server's public IP. Confirm DNS for the new domain points here before continuing."
                    .into(),
            );
        } else {
            match tokio::net::lookup_host((check_host.as_str(), 80)).await {
                Ok(addrs) => {
                    let resolved: Vec<String> = addrs.map(|a| a.ip().to_string()).collect();
                    if resolved.is_empty() {
                        warnings.push(format!(
                            "{check_host} does not resolve yet. Point DNS (wildcard *.{domain} or {check_host}) to {expected} before traffic will work."
                        ));
                    } else if !resolved.iter().any(|ip| ip == &expected) {
                        warnings.push(format!(
                            "{check_host} resolves to [{}], expected {expected}. Update DNS before or after applying.",
                            resolved.join(", ")
                        ));
                    }
                }
                Err(_) => {
                    warnings.push(format!(
                        "{check_host} does not resolve yet. Point DNS (wildcard *.{domain}) to {expected}."
                    ));
                }
            }
        }
        warnings.push(
            "After changing domain, old URLs stop working. You must reopen the dashboard on the new host and sign in again."
                .into(),
        );
    }

    (errors, warnings)
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct DomainApplyRequest {
    pub domain: String,
    #[serde(default)]
    pub acknowledge_dns: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct DomainApplyResponse {
    pub job_id: String,
}

#[utoipa::path(
    post,
    path = "/settings/domain/apply",
    request_body = DomainApplyRequest,
    responses(
        (status = 200, body = DomainApplyResponse),
        (status = 400),
        (status = 409),
        (status = 500),
    ),
    tag = "global-settings",
    security(("session_auth" = []))
)]
pub async fn domain_apply(
    State(state): State<AppState>,
    Json(payload): Json<DomainApplyRequest>,
) -> Result<Json<DomainApplyResponse>, (StatusCode, String)> {
    let domain = normalize_domain(&payload.domain).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let (errors, warnings) = run_domain_preflight(&state, &domain).await;
    if !errors.is_empty() {
        return Err((StatusCode::BAD_REQUEST, errors.join("; ")));
    }
    if !warnings.is_empty() && !payload.acknowledge_dns {
        return Err((
            StatusCode::BAD_REQUEST,
            "acknowledge_dns required when preflight returns warnings".into(),
        ));
    }

    // Reject if another domain job is already running
    for entry in state.domain_jobs.iter() {
        if matches!(entry.status, DomainJobStatus::Pending | DomainJobStatus::Running) {
            return Err((StatusCode::CONFLICT, format!("domain change job {} already in progress", entry.id)));
        }
    }

    let old_domain = state.platform.domain();
    let job_id = Uuid::new_v4().to_string();
    let job = DomainJob::new(job_id.clone(), old_domain, domain, &state.platform.dashboard_subdomain());
    state.domain_jobs.insert(job_id.clone(), job);

    let state_clone = state.clone();
    let job_id_clone = job_id.clone();
    tokio::spawn(async move {
        run_domain_job(&state_clone, &job_id_clone, 0).await;
    });

    Ok(Json(DomainApplyResponse { job_id }))
}

#[utoipa::path(
    get,
    path = "/settings/domain/jobs/{id}",
    params(("id" = String, Path, description = "Job ID")),
    responses(
        (status = 200, body = DomainJob),
        (status = 404),
    ),
    tag = "global-settings",
    security(("session_auth" = []))
)]
pub async fn domain_job_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<DomainJob>, (StatusCode, String)> {
    state
        .domain_jobs
        .get(&id)
        .map(|j| Json(j.clone()))
        .ok_or_else(|| (StatusCode::NOT_FOUND, "job not found".into()))
}

#[utoipa::path(
    post,
    path = "/settings/domain/jobs/{id}/retry",
    params(("id" = String, Path, description = "Job ID")),
    responses(
        (status = 200, body = DomainApplyResponse),
        (status = 400),
        (status = 404),
        (status = 409),
    ),
    tag = "global-settings",
    security(("session_auth" = []))
)]
pub async fn domain_job_retry(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<DomainApplyResponse>, (StatusCode, String)> {
    let resume_from = {
        let mut job = state.domain_jobs.get_mut(&id).ok_or((StatusCode::NOT_FOUND, "job not found".into()))?;
        if job.status != DomainJobStatus::Failed {
            return Err((StatusCode::BAD_REQUEST, "only failed jobs can be retried".into()));
        }
        job.status = DomainJobStatus::Pending;
        job.error = None;
        let idx = job.resume_from;
        if let Some(step) = job.steps.get_mut(idx) {
            step.status = DomainStepStatus::Pending;
            step.error = None;
        }
        idx
    };

    let state_clone = state.clone();
    let job_id = id.clone();
    tokio::spawn(async move {
        run_domain_job(&state_clone, &job_id, resume_from).await;
    });

    Ok(Json(DomainApplyResponse { job_id: id }))
}

fn update_job_step(state: &AppState, job_id: &str, step_idx: usize, status: DomainStepStatus, error: Option<String>) {
    if let Some(mut job) = state.domain_jobs.get_mut(job_id) {
        if let Some(step) = job.steps.get_mut(step_idx) {
            step.status = status;
            step.error = error.clone();
        }
        if status == DomainStepStatus::Failed {
            job.status = DomainJobStatus::Failed;
            job.error = error;
            job.resume_from = step_idx;
        } else if status == DomainStepStatus::Running {
            job.status = DomainJobStatus::Running;
        }
    }
}

async fn run_domain_job(state: &AppState, job_id: &str, start_from: usize) {
    let (new_domain, old_domain) = match state.domain_jobs.get(job_id) {
        Some(j) => (j.domain.clone(), j.old_domain.clone()),
        None => return,
    };

    if let Some(mut job) = state.domain_jobs.get_mut(job_id) {
        job.status = DomainJobStatus::Running;
    }

    // Step 0: Persist
    if start_from <= 0 {
        update_job_step(state, job_id, 0, DomainStepStatus::Running, None);
        if let Err(e) = upsert_setting(&state.db, "domain", &new_domain).await {
            update_job_step(state, job_id, 0, DomainStepStatus::Failed, Some(e.1));
            return;
        }
        state.platform.set_domain(new_domain.clone());
        update_job_step(state, job_id, 0, DomainStepStatus::Done, None);
    }

    // Step 1: Rebuild master routes (no DNS yet)
    if start_from <= 1 {
        update_job_step(state, job_id, 1, DomainStepStatus::Running, None);
        if let Err(e) = sync_platform_routes(state, false).await {
            update_job_step(state, job_id, 1, DomainStepStatus::Failed, Some(e));
            return;
        }
        update_job_step(state, job_id, 1, DomainStepStatus::Done, None);
    }

    // Step 2: Re-register agents
    if start_from <= 2 {
        update_job_step(state, job_id, 2, DomainStepStatus::Running, None);
        let (ok, errs) = crate::routes::nodes::reregister_online_agents(state).await;
        if ok == 0 && !errs.is_empty() {
            // Only fail if we had agents and all failed — soft if no agents
            let online = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM nodes WHERE status = 'online' AND id != 'local'",
            )
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);
            if online > 0 {
                update_job_step(state, job_id, 2, DomainStepStatus::Failed, Some(errs.join("; ")));
                return;
            }
        }
        for e in &errs {
            tracing::warn!(error = %e, "domain job: agent re-register warning");
        }
        update_job_step(state, job_id, 2, DomainStepStatus::Done, None);
    }

    // Step 3: Push agent Caddy (second sync_routes; CF mode pushes agent configs)
    if start_from <= 3 {
        update_job_step(state, job_id, 3, DomainStepStatus::Running, None);
        if let Err(e) = sync_platform_routes(state, false).await {
            update_job_step(state, job_id, 3, DomainStepStatus::Failed, Some(e));
            return;
        }
        update_job_step(state, job_id, 3, DomainStepStatus::Done, None);
    }

    // Step 4: Cloudflare DNS cleanup (old) + sync (new)
    if start_from <= 4 {
        update_job_step(state, job_id, 4, DomainStepStatus::Running, None);
        let routing_mode = get_setting(&state.db, "routing_mode")
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| state.config.routing_mode.to_string());
        if routing_mode == "cloudflare_dns" {
            let cf_token = get_setting(&state.db, "cloudflare_api_token").await.ok().flatten().unwrap_or_default();
            let cf_zone = get_setting(&state.db, "cloudflare_zone_id").await.ok().flatten().unwrap_or_default();
            if !cf_token.is_empty() && !cf_zone.is_empty() {
                let cf = CloudflareClient::new(&cf_token, &cf_zone);
                let old_suffix = format!(".{}", old_domain);
                match cf.list_records_by_suffix(&old_suffix, "A").await {
                    Ok(records) => {
                        for record in &records {
                            if let Err(e) = cf.delete_record(&record.id).await {
                                tracing::warn!(record = %record.name, error = %e, "domain job: failed to delete old DNS record");
                            }
                        }
                    }
                    Err(e) => {
                        update_job_step(
                            state,
                            job_id,
                            4,
                            DomainStepStatus::Failed,
                            Some(format!("failed to list old DNS records: {e}")),
                        );
                        return;
                    }
                }
            }
            if let Err(e) = sync_platform_routes(state, true).await {
                update_job_step(state, job_id, 4, DomainStepStatus::Failed, Some(e));
                return;
            }
            update_job_step(state, job_id, 4, DomainStepStatus::Done, None);
        } else {
            update_job_step(state, job_id, 4, DomainStepStatus::Skipped, None);
        }
    }

    if let Some(mut job) = state.domain_jobs.get_mut(job_id) {
        job.status = DomainJobStatus::Completed;
        job.error = None;
        // Refresh dashboard URL from current subdomain
        job.dashboard_url = format!(
            "https://{}.{}",
            state.platform.dashboard_subdomain(),
            state.platform.domain()
        );
    }
    tracing::info!(job_id, domain = %new_domain, "domain change job completed");
}
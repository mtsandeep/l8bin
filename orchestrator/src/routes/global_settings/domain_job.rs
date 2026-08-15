use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::platform::{DomainJob, DomainJobStatus, DomainStepStatus, normalize_domain};
use litebin_common::cloudflare::CloudflareClient;

use super::{get_setting, sync_platform_routes, upsert_setting};

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
            if !dns_target.is_empty() { dns_target } else { state.config.public_ip.clone() }
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
        return Err((StatusCode::BAD_REQUEST, "acknowledge_dns required when preflight returns warnings".into()));
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
    state.domain_jobs.get(&id).map(|j| Json(j.clone())).ok_or_else(|| (StatusCode::NOT_FOUND, "job not found".into()))
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
    if start_from == 0 {
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
            let online =
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM nodes WHERE status = 'online' AND id != 'local'")
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
        job.dashboard_url = format!("https://{}.{}", state.platform.dashboard_subdomain(), state.platform.domain());
    }
    tracing::info!(job_id, domain = %new_domain, "domain change job completed");
}

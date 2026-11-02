const PREVIOUS_DOMAIN_GRACE_SECS: i64 = 24 * 60 * 60;

async fn run_domain_job(state: &AppState, job_id: &str, start_from: usize) {
    let (new_domain, old_domain) = match state.domain_jobs.get(job_id) {
        Some(j) => (j.domain.clone(), j.old_domain.clone()),
        None => return,
    };

    if let Some(mut job) = state.domain_jobs.get_mut(job_id) {
        job.status = DomainJobStatus::Running;
    }

    // Phase 1 — prepare: the new domain is staged alongside the old one.
    // Nothing persists; a failure here leaves the current domain untouched.

    // Step 0: Stage new-domain routes in Caddy
    if start_from == 0 {
        update_job_step(state, job_id, 0, DomainStepStatus::Running, None);
        let staged = async {
            super::upsert_setting(&state.db, "staged_domain", &new_domain).await.map_err(|e| e.1)?;
            stage_domain_routes(state, &new_domain).await
        };
        if let Err(e) = staged.await {
            let _ = super::delete_setting(&state.db, "staged_domain").await;
            update_job_step(state, job_id, 0, DomainStepStatus::Failed, Some(e));
            return;
        }
        update_job_step(state, job_id, 0, DomainStepStatus::Done, None);
    }

    // Step 1: Verify the new dashboard answers (on-demand TLS issues the cert)
    if start_from <= 1 {
        update_job_step(state, job_id, 1, DomainStepStatus::Running, None);
        let url = format!("https://{}.{}", state.platform.dashboard_subdomain(), new_domain);
        if let Err(e) = wait_for_dashboard(&url).await {
            let _ = super::delete_setting(&state.db, "staged_domain").await;
            update_job_step(state, job_id, 1, DomainStepStatus::Failed, Some(e));
            return;
        }
        update_job_step(state, job_id, 1, DomainStepStatus::Done, None);
    }

    // Phase 2 — activate: one shot, only after the new URL was verified.

    // Step 2: Persist domain + grace bookkeeping
    if start_from <= 2 {
        update_job_step(state, job_id, 2, DomainStepStatus::Running, None);
        let activate = async {
            // An older previous domain is retired immediately; only the latest is retained.
            if let Some(older) = crate::platform::previous_domain(&state.db).await {
                delete_previous_domain_records(state, &older).await;
            }
            let retire_at = (chrono::Utc::now().timestamp() + PREVIOUS_DOMAIN_GRACE_SECS).to_string();
            super::upsert_setting(&state.db, "previous_domain", &old_domain).await.map_err(|e| anyhow::anyhow!(e.1))?;
            super::upsert_setting(&state.db, "previous_domain_retire_at", &retire_at)
                .await
                .map_err(|e| anyhow::anyhow!(e.1))?;
            super::delete_setting(&state.db, "staged_domain").await.map_err(|e| anyhow::anyhow!(e.1))?;
            super::upsert_setting(&state.db, "domain", &new_domain).await.map_err(|e| anyhow::anyhow!(e.1))?;
            anyhow::Ok(())
        };
        if let Err(e) = activate.await {
            update_job_step(state, job_id, 2, DomainStepStatus::Failed, Some(e.to_string()));
            return;
        }
        state.platform.set_domain(new_domain.clone());
        update_job_step(state, job_id, 2, DomainStepStatus::Done, None);
    }

    // Step 3: Rebuild master routes
    if start_from <= 3 {
        update_job_step(state, job_id, 3, DomainStepStatus::Running, None);
        if let Err(e) = sync_platform_routes(state, false).await {
            update_job_step(state, job_id, 3, DomainStepStatus::Failed, Some(e));
            return;
        }
        update_job_step(state, job_id, 3, DomainStepStatus::Done, None);
    }

    // Step 4: Re-register agents
    if start_from <= 4 {
        update_job_step(state, job_id, 4, DomainStepStatus::Running, None);
        let (ok, errs) = crate::routes::nodes::reregister_online_agents(state).await;
        if ok == 0 && !errs.is_empty() {
            // Only fail if we had agents and all failed — soft if no agents
            let online =
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM nodes WHERE status = 'online' AND id != 'local'")
                    .fetch_one(&state.db)
                    .await
                    .unwrap_or(0);
            if online > 0 {
                update_job_step(state, job_id, 4, DomainStepStatus::Failed, Some(errs.join("; ")));
                return;
            }
        }
        for e in &errs {
            tracing::warn!(error = %e, "domain job: agent re-register warning");
        }
        update_job_step(state, job_id, 4, DomainStepStatus::Done, None);
    }

    // Step 5: Push agent Caddy (CF mode pushes agent configs)
    if start_from <= 5 {
        update_job_step(state, job_id, 5, DomainStepStatus::Running, None);
        if let Err(e) = sync_platform_routes(state, false).await {
            update_job_step(state, job_id, 5, DomainStepStatus::Failed, Some(e));
            return;
        }
        update_job_step(state, job_id, 5, DomainStepStatus::Done, None);
    }

    // Step 6: Cloudflare DNS sync for the new domain (old-domain records
    // are removed at retirement, not here)
    if start_from <= 6 {
        update_job_step(state, job_id, 6, DomainStepStatus::Running, None);
        let routing_mode = get_setting(&state.db, "routing_mode")
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| state.config.routing_mode.to_string());
        if routing_mode == "cloudflare_dns" {
            if let Err(e) = sync_platform_routes(state, true).await {
                update_job_step(state, job_id, 6, DomainStepStatus::Failed, Some(e));
                return;
            }
            update_job_step(state, job_id, 6, DomainStepStatus::Done, None);
        } else {
            update_job_step(state, job_id, 6, DomainStepStatus::Skipped, None);
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

/// Serve the staged domain alongside the current one (prepare phase).
async fn stage_domain_routes(state: &AppState, staged: &str) -> Result<(), String> {
    let snap = state.platform.snapshot();
    let upstream = format!("litebin-orchestrator:{}", state.config.port);
    let mut routes = crate::routing_helpers::resolve_all_routes(&state.db, &snap.domain, &upstream)
        .await
        .map_err(|e| e.to_string())?;
    if staged != snap.domain {
        match crate::routing_helpers::resolve_routes_for_domain(&state.db, staged, &upstream).await {
            Ok(mut staged_routes) => routes.append(&mut staged_routes),
            Err(e) => tracing::warn!(error = %e, staged = %staged, "failed to resolve staged-domain routes"),
        }
    }
    state
        .router
        .read()
        .await
        .sync_routes(
            &routes,
            &snap.domain,
            Some(staged),
            &upstream,
            &snap.dashboard_subdomain,
            &snap.poke_subdomain,
            false,
        )
        .await
        .map_err(|e| e.to_string())
}

/// Poll the new dashboard URL until it answers; the first probe triggers
/// on-demand TLS issuance.
async fn wait_for_dashboard(url: &str) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    for _ in 0..36 {
        if client.get(url).send().await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
    Err(format!("{url} did not answer within 3 minutes — check that DNS for the new domain points at this server"))
}

#[utoipa::path(
    post,
    path = "/settings/domain/retire-previous",
    description = "Retire the retained previous domain now: drop its routes and its managed DNS records.",
    responses(
        (status = 200),
        (status = 500),
    ),
    tag = "global-settings",
    security(("session_auth" = []))
)]
pub async fn retire_previous(State(state): State<AppState>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let retired = retire_previous_domain(&state).await;
    Ok(Json(serde_json::json!({ "retired": retired })))
}

pub(crate) async fn retire_previous_domain(state: &AppState) -> bool {
    let Some(prev) = crate::platform::previous_domain(&state.db).await else { return false };
    delete_previous_domain_records(state, &prev).await;
    let _ = super::delete_setting(&state.db, "previous_domain").await;
    let _ = super::delete_setting(&state.db, "previous_domain_retire_at").await;
    let _ = state.route_sync_tx.send(());
    tracing::info!(previous_domain = %prev, "previous domain retired");
    true
}

/// Delete a domain's managed CF records (creds permitting).
async fn delete_previous_domain_records(state: &AppState, domain: &str) {
    let token = get_setting(&state.db, "cloudflare_api_token").await.ok().flatten().unwrap_or_default();
    let zone = get_setting(&state.db, "cloudflare_zone_id").await.ok().flatten().unwrap_or_default();
    let Ok(ledger) = crate::dns_ledger::rows(&state.db).await else { return };
    if token.is_empty() || zone.is_empty() {
        tracing::warn!(domain, "previous-domain records left in Cloudflare — no credentials configured");
    } else {
        let cf = CloudflareClient::new(&token, &zone);
        for row in crate::dns_ledger::under_domain(&ledger, domain) {
            let z = if row.zone_id.is_empty() { zone.as_str() } else { row.zone_id.as_str() };
            if let Err(e) = cf.delete_record_in_zone(z, &row.record_id).await {
                tracing::warn!(record = %row.name, error = %e, "failed to delete previous-domain DNS record");
            }
        }
    }
    for row in crate::dns_ledger::under_domain(&ledger, domain) {
        let _ = crate::dns_ledger::remove(&state.db, &row.name).await;
    }
}

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

use super::{get_setting, sync_platform_routes};

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
    pub checks: Vec<PreflightCheck>,
    /// dns_target / PUBLIC_IP / freshly detected — the IP records must point at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_public_ip: Option<String>,
    pub sslip: bool,
    /// Cloudflare mode only: per-record outcome if this domain were synced now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<DnsPlanPreview>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct PreflightCheck {
    pub id: String,
    /// ok | warn | error | skipped
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Serialize, Default, utoipa::ToSchema)]
pub struct DnsPlanPreview {
    pub created: Vec<String>,
    pub repointed: Vec<String>,
    pub unchanged: Vec<String>,
    pub skipped: Vec<String>,
}

#[utoipa::path(
    post,
    path = "/settings/domain/preflight",
    description = "Check a candidate platform domain: DNS control, conflicts, TLS reachability; returns errors and warnings.",
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
    let sslip = crate::platform::PlatformSettings::is_tryout_domain(&domain);
    let outcome = run_domain_preflight(&state, &domain).await;
    let ok = outcome.errors.is_empty();
    Ok(Json(DomainPreflightResponse {
        ok,
        domain,
        errors: outcome.errors,
        warnings: outcome.warnings,
        checks: outcome.checks,
        detected_public_ip: outcome.detected_public_ip,
        sslip,
        plan: outcome.plan,
    }))
}

struct PreflightOutcome {
    errors: Vec<String>,
    warnings: Vec<String>,
    checks: Vec<PreflightCheck>,
    detected_public_ip: Option<String>,
    plan: Option<DnsPlanPreview>,
}

fn check(id: &str, status: &str, detail: Option<String>) -> PreflightCheck {
    PreflightCheck { id: id.into(), status: status.into(), detail }
}

async fn run_domain_preflight(state: &AppState, domain: &str) -> PreflightOutcome {
    use crate::cloudflare_router::dns::{desired_dns_records, resolved_dns_ip, zone_for_name};

    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut checks = Vec::new();

    let current = state.platform.domain();
    if domain == current {
        errors.push("new domain is the same as the current domain".into());
        return PreflightOutcome { errors, warnings, checks, detected_public_ip: None, plan: None };
    }

    let sslip = crate::platform::PlatformSettings::is_tryout_domain(domain);
    if sslip {
        warnings.push(
            "This looks like tryout DNS (sslip.io / nip.io). Let's Encrypt may fail; not recommended for production."
                .into(),
        );
    }

    // The IP records must point at: dns_target setting, else PUBLIC_IP, else detect.
    let mut public_ip = resolved_dns_ip(&state.db, &state.config.public_ip).await;
    if public_ip.is_empty() {
        public_ip = litebin_common::net::detect_public_ip().await.unwrap_or_default();
    }
    if public_ip.is_empty() {
        errors.push("Could not determine this server's public IP. Set the DNS target in Settings or PUBLIC_IP.".into());
        checks.push(check("public_ip", "error", Some("unknown — set dns target or PUBLIC_IP".into())));
    } else {
        checks.push(check("public_ip", "ok", Some(public_ip.clone())));
    }

    let routing_mode = get_setting(&state.db, "routing_mode")
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| state.config.routing_mode.to_string());

    let mut plan = None;
    if routing_mode == "cloudflare_dns" {
        let cf_token = get_setting(&state.db, "cloudflare_api_token").await.ok().flatten().unwrap_or_default();
        let cf_zone = get_setting(&state.db, "cloudflare_zone_id").await.ok().flatten().unwrap_or_default();
        if cf_token.is_empty() || cf_zone.is_empty() {
            errors.push("Cloudflare API token and Zone ID must be configured for cloudflare_dns mode".into());
            checks.push(check("cf_credentials", "error", Some("token or zone id missing".into())));
        } else {
            let cf = CloudflareClient::new(&cf_token, &cf_zone);
            match cf.get_zone_name().await {
                Ok(zone_name) => {
                    if !CloudflareClient::zone_covers_hostname(&zone_name, domain) {
                        errors.push(format!(
                            "Cloudflare zone '{zone_name}' does not cover '{domain}'. Use a subdomain of the zone or change the zone."
                        ));
                        checks.push(check(
                            "cf_zone",
                            "error",
                            Some(format!("zone '{zone_name}' does not cover the domain")),
                        ));
                    } else if let Err(e) = cf.list_records_by_suffix(&format!(".{domain}"), "A").await {
                        errors.push(format!("Cloudflare token cannot list DNS records: {e}"));
                        checks.push(check("cf_zone", "error", Some(format!("cannot list records: {e}"))));
                    } else {
                        checks.push(check("cf_zone", "ok", Some(zone_name.clone())));
                    }

                    // Plan preview: per-record outcome of a sync, read-only.
                    let desired = desired_dns_records(
                        &state.db,
                        &public_ip,
                        domain,
                        &state.platform.dashboard_subdomain(),
                        &state.platform.poke_subdomain(),
                    )
                    .await;
                    let account_zones = cf.list_zones().await.unwrap_or_default();
                    let mut preview = DnsPlanPreview::default();
                    for (name, ip) in &desired {
                        let Some(zone) = zone_for_name(name, &cf_zone, &zone_name, &account_zones).map(str::to_string)
                        else {
                            preview.skipped.push(name.clone());
                            continue;
                        };
                        match cf.list_records_in_zone(&zone, name, "A").await {
                            Ok(records) => match records.first() {
                                Some(r) if &r.content == ip => preview.unchanged.push(name.clone()),
                                Some(_) => preview.repointed.push(name.clone()),
                                None => preview.created.push(name.clone()),
                            },
                            Err(_) => preview.skipped.push(name.clone()),
                        }
                    }
                    checks.push(check(
                        "dns_plan",
                        "ok",
                        Some(format!(
                            "{} to create, {} to re-point, {} unchanged, {} skipped",
                            preview.created.len(),
                            preview.repointed.len(),
                            preview.unchanged.len(),
                            preview.skipped.len()
                        )),
                    ));
                    plan = Some(preview);
                }
                Err(e) => {
                    errors.push(format!("Cannot read Cloudflare zone (check token and zone ID): {e}"));
                    checks.push(check("cf_zone", "error", Some(e.to_string())));
                }
            }
        }
    } else {
        // Master proxy: the user owns DNS.
        let dashboard = state.platform.dashboard_subdomain();
        if sslip {
            checks.push(check("dns_dash", "ok", Some("sslip.io hostnames resolve automatically".into())));
            checks.push(check("wildcard", "skipped", None));
        } else if public_ip.is_empty() {
            checks.push(check("dns_dash", "warn", Some("cannot verify without the public IP".into())));
            checks.push(check("wildcard", "skipped", None));
        } else {
            let check_host = format!("{dashboard}.{domain}");
            match tokio::net::lookup_host((check_host.clone(), 80)).await {
                Ok(addrs) => {
                    let resolved: Vec<String> = addrs.map(|a| a.ip().to_string()).collect();
                    if resolved.iter().any(|ip| ip == &public_ip) {
                        checks.push(check("dns_dash", "ok", Some(check_host)));
                    } else {
                        warnings.push(format!(
                            "{check_host} resolves to [{}], expected {public_ip}. Update DNS before or after applying.",
                            resolved.join(", ")
                        ));
                        checks.push(check("dns_dash", "warn", Some(format!("resolves to {}", resolved.join(", ")))));
                    }
                }
                Err(_) => {
                    warnings.push(format!(
                        "{check_host} does not resolve yet. Point DNS (wildcard *.{domain} or {check_host}) to {public_ip} before traffic will work."
                    ));
                    checks.push(check("dns_dash", "warn", Some("does not resolve yet".into())));
                }
            }

            // Wildcard probe: a random label resolving means *.domain exists.
            let probe = format!("probe-{}.{}", uuid::Uuid::new_v4().simple(), domain);
            let wildcard_ok = match tokio::net::lookup_host((probe.as_str(), 80)).await {
                Ok(addrs) => addrs.map(|a| a.ip().to_string()).any(|ip| ip == public_ip),
                Err(_) => false,
            };
            if wildcard_ok {
                checks.push(check("wildcard", "ok", Some(format!("*.{domain} covers project subdomains"))));
            } else {
                warnings.push(format!(
                    "No wildcard *.{domain} — every project subdomain needs its own record pointing to {public_ip}."
                ));
                checks.push(check("wildcard", "warn", Some("no wildcard — add records per subdomain".into())));
            }
        }
        warnings.push(
            "The old domain keeps working for 24h after the change; reopen the dashboard on the new host and sign in again."
                .into(),
        );
    }

    PreflightOutcome {
        errors,
        warnings,
        checks,
        detected_public_ip: (!public_ip.is_empty()).then_some(public_ip),
        plan,
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct DomainApplyRequest {
    pub domain: String,
    /// Compute the domain from this server's public IP (`<ip>.sslip.io`),
    /// master-proxy mode — the domain-expiry fallback.
    #[serde(default)]
    pub sslip: bool,
    #[serde(default)]
    pub acknowledge_dns: bool,
    /// Routing settings persisted before preflight so validation uses them.
    pub routing_mode: Option<String>,
    pub cloudflare_api_token: Option<String>,
    pub cloudflare_zone_id: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct DomainApplyResponse {
    pub job_id: String,
}

#[utoipa::path(
    get,
    path = "/settings/domain/sslip",
    description = "The sslip.io domain this server's public IP maps to (null when the IP is unknown).",
    responses((status = 200, body = SslipDomainResponse)),
    tag = "global-settings",
    security(("session_auth" = []))
)]
pub async fn sslip_domain_preview(
    State(state): State<AppState>,
) -> Result<Json<SslipDomainResponse>, (StatusCode, String)> {
    let mut ip = crate::cloudflare_router::dns::resolved_dns_ip(&state.db, &state.config.public_ip).await;
    if ip.is_empty() {
        ip = litebin_common::net::detect_public_ip().await.unwrap_or_default();
    }
    Ok(Json(SslipDomainResponse { domain: (!ip.is_empty()).then(|| crate::platform::sslip_domain(&ip)) }))
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct SslipDomainResponse {
    pub domain: Option<String>,
}

#[utoipa::path(
    post,
    path = "/settings/domain/apply",
    description = "Switch the platform domain as a background job (routes, agent Caddy, DNS cutover); poll the jobs endpoint.",
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
    let routing_mode = if payload.sslip { Some("master_proxy".to_string()) } else { payload.routing_mode.clone() };
    let domain = if payload.sslip {
        let mut ip = crate::cloudflare_router::dns::resolved_dns_ip(&state.db, &state.config.public_ip).await;
        if ip.is_empty() {
            ip = litebin_common::net::detect_public_ip().await.unwrap_or_default();
        }
        if ip.is_empty() {
            return Err((StatusCode::BAD_REQUEST, "could not determine this server's public IP".into()));
        }
        crate::platform::sslip_domain(&ip)
    } else {
        payload.domain.clone()
    };
    let domain = normalize_domain(&domain).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    super::apply_routing_settings(
        &state,
        routing_mode.as_deref(),
        payload.cloudflare_api_token.as_deref(),
        payload.cloudflare_zone_id.as_deref(),
    )
    .await?;
    let preflight = run_domain_preflight(&state, &domain).await;
    let (errors, warnings) = (preflight.errors, preflight.warnings);
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
    description = "Domain-change job progress: per-step status, errors, and the resulting dashboard URL.",
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
    description = "Retry a failed domain-change job, resuming from the failed step.",
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

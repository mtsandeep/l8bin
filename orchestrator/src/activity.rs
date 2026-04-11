use std::collections::HashSet;

use tracing::{debug, error, info};

use litebin_common::heartbeat;

use crate::AppState;

/// Background task that tails Caddy container logs via Docker,
/// collects unique hosts from access logs,
/// and updates `last_active_at` for projects that received traffic.
pub async fn run_activity_tracker(state: AppState) {
    info!("activity tracker: starting");
    let domain = state.config.domain.clone();
    let dashboard_host = format!("{}.{}", state.config.dashboard_subdomain, domain);
    let poke_host = format!("{}.{}", state.config.poke_subdomain, domain);

    let caddy_container = std::env::var("CADDY_CONTAINER_NAME")
        .unwrap_or_else(|_| "litebin-caddy".into());

    heartbeat::run_docker_log_tailer(
        state.docker.as_ref().clone(),
        caddy_container,
        state.config.flush_interval_secs,
        move |hosts| {
            let state = state.clone();
            let dashboard_host = dashboard_host.clone();
            let poke_host = poke_host.clone();
            async move {
                update_active_projects(&state, hosts, &dashboard_host, &poke_host).await;
            }
        },
    )
    .await;
}

/// Update `last_active_at` for running projects that match the given hosts.
async fn update_active_projects(
    state: &AppState,
    hosts: HashSet<String>,
    dashboard_host: &str,
    poke_host: &str,
) {
    let now = chrono::Utc::now().timestamp();
    let domain_suffix = format!(".{}", state.config.domain);
    let mut subdomain_ids: Vec<String> = Vec::new();
    let mut custom_domains: Vec<String> = Vec::new();

    for host in &hosts {
        // Skip non-project hosts
        if host == dashboard_host || host == poke_host {
            continue;
        }
        // Check if this is a subdomain of our domain
        if let Some(subdomain) = host.strip_suffix(&domain_suffix) {
            if !subdomain.is_empty() && !subdomain.contains('.') {
                subdomain_ids.push(subdomain.to_string());
            } else {
                custom_domains.push(host.clone());
            }
        } else {
            custom_domains.push(host.clone());
        }
    }

    if subdomain_ids.is_empty() && custom_domains.is_empty() {
        return;
    }

    let mut qb: sqlx::QueryBuilder<sqlx::Sqlite> = sqlx::QueryBuilder::new(
        "UPDATE projects SET last_active_at = "
    );
    qb.push_bind(now);
    qb.push(", updated_at = ");
    qb.push_bind(now);
    qb.push(" WHERE status = 'running' AND auto_stop_enabled = 1 AND (");

    if !subdomain_ids.is_empty() {
        qb.push("id IN (");
        let mut separated = qb.separated(", ");
        for id in &subdomain_ids {
            separated.push_bind(id.as_str());
        }
        qb.push(")");
    }

    if !custom_domains.is_empty() {
        if !subdomain_ids.is_empty() {
            qb.push(" OR ");
        }
        qb.push("custom_domain IN (");
        let mut separated = qb.separated(", ");
        for cd in &custom_domains {
            separated.push_bind(cd.as_str());
        }
        qb.push(")");
    }

    qb.push(")");

    match qb.build().execute(&state.db).await {
        Ok(result) => {
            if result.rows_affected() > 0 {
                info!(
                    rows = result.rows_affected(),
                    hosts = hosts.len(),
                    "activity tracker: updated last_active_at for active projects"
                );
            } else {
                debug!(hosts = hosts.len(), "activity tracker: no matching running projects");
            }
        }
        Err(e) => {
            error!(error = %e, "activity tracker: DB update failed");
        }
    }
}

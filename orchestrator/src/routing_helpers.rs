use std::sync::Arc;
use std::time::Duration;

use sqlx::SqlitePool;
use tokio::sync::RwLock;

use crate::config::Config;
use litebin_common::routing::{ProjectRoute, RoutingProvider};
use litebin_common::types::Project;

/// Resolve the upstream address for each project by looking up its assigned node.
/// Local projects use `host.docker.internal:{port}`, remote projects use `{node_host}:{port}`.
pub async fn resolve_routes(
    projects: &[Project],
    db: &SqlitePool,
    domain: &str,
) -> anyhow::Result<Vec<ProjectRoute>> {
    let mut routes = Vec::with_capacity(projects.len());

    for project in projects {
        let Some(_mapped_port) = project.mapped_port else {
            continue;
        };
        if project.status != "running" {
            continue;
        }

        let Some(internal_port) = project.internal_port else {
            continue;
        };

        let (upstream, node_public_ip) = match project.node_id.as_deref() {
            Some(node_id) if node_id != "local" => {
                // Remote node — look up the node's host IP and public IP
                let row: Option<(String, Option<String>)> = sqlx::query_as(
                    "SELECT host, public_ip FROM nodes WHERE id = ?",
                )
                .bind(node_id)
                .fetch_optional(db)
                .await?;

                match row {
                    Some((host, public_ip)) => (format!("{}:443", host), public_ip),
                    None => {
                        tracing::warn!(
                            project_id = %project.id,
                            node_id = %node_id,
                            "node not found in DB, skipping route"
                        );
                        continue;
                    }
                }
            }
            _ => {
                // Local node — route directly via Docker network by container name
                let local_ip: Option<String> = sqlx::query_scalar(
                    "SELECT public_ip FROM nodes WHERE id = 'local'",
                )
                .fetch_optional(db)
                .await?
                .flatten();
                (format!("litebin-{}:{}", project.id, internal_port), local_ip)
            }
        };

        // Container-level upstream for agent Caddy in cloudflare_dns mode
        let container_upstream =
            Some(format!("litebin-{}:{}", project.id, internal_port));

        routes.push(ProjectRoute {
            project_id: project.id.clone(),
            subdomain_host: format!("{}.{}", project.id, domain),
            upstream,
            custom_domain: project.custom_domain.clone(),
            node_id: project.node_id.clone(),
            node_public_ip,
            host_rewrite: None,
            upstream_tls: project.node_id.as_deref() != Some("local")
                && project.node_id.is_some(),
            container_upstream,
        });
    }

    Ok(routes)
}

/// Resolve routes for sleeping projects that have custom domains.
/// These routes point to the orchestrator waker so that visiting a custom domain
/// of a sleeping app triggers the wake (same as the `*.{domain}` catch-all does for subdomains).
async fn resolve_sleeping_custom_domain_routes(
    db: &SqlitePool,
    domain: &str,
    orchestrator_upstream: &str,
) -> anyhow::Result<Vec<ProjectRoute>> {
    let sleeping = sqlx::query_as::<_, Project>(
        "SELECT * FROM projects WHERE status IN ('stopped', 'stopping') AND custom_domain IS NOT NULL AND custom_domain != ''",
    )
    .fetch_all(db)
    .await?;

    let mut routes = Vec::with_capacity(sleeping.len());
    for project in &sleeping {
        let node_public_ip = match project.node_id.as_deref() {
            Some(node_id) if node_id != "local" => {
                let row: Option<(Option<String>,)> = sqlx::query_as(
                    "SELECT public_ip FROM nodes WHERE id = ?",
                )
                .bind(node_id)
                .fetch_optional(db)
                .await?;
                row.and_then(|(ip,)| ip)
            }
            _ => None,
        };

        routes.push(ProjectRoute {
            project_id: project.id.clone(),
            subdomain_host: format!("{}.{}", project.id, domain),
            upstream: orchestrator_upstream.to_string(),
            custom_domain: project.custom_domain.clone(),
            node_id: project.node_id.clone(),
            node_public_ip,
            host_rewrite: Some(format!("{}.{}", project.id, domain)),
            upstream_tls: false, // sleeping routes proxy to local orchestrator
            container_upstream: None, // sleeping projects have no running container
        });
    }

    Ok(routes)
}

/// Resolve all routes: running projects + sleeping custom domain routes.
/// This is the single entry point that every call site should use.
pub async fn resolve_all_routes(
    db: &SqlitePool,
    domain: &str,
    orchestrator_upstream: &str,
) -> anyhow::Result<Vec<ProjectRoute>> {
    let all_running = sqlx::query_as::<_, Project>(
        "SELECT * FROM projects WHERE status = 'running'",
    )
    .fetch_all(db)
    .await?;

    let mut routes = resolve_routes(&all_running, db, domain).await?;

    match resolve_sleeping_custom_domain_routes(db, domain, orchestrator_upstream).await {
        Ok(sleeping_cd_routes) => routes.extend(sleeping_cd_routes),
        Err(e) => tracing::warn!(error = %e, "failed to resolve sleeping custom domain routes"),
    }

    Ok(routes)
}

/// Background task that debounces route sync signals.
/// Receives signals via the channel, waits 500ms after the first signal
/// to batch any rapid-fire completions, then performs a single sync.
pub async fn run_route_sync(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<()>,
    db: SqlitePool,
    router: Arc<RwLock<Arc<dyn RoutingProvider>>>,
    config: Arc<Config>,
) {
    loop {
        // Wait for the first signal
        if rx.recv().await.is_none() {
            break;
        }

        // Debounce: wait for more signals to accumulate
        tokio::time::sleep(Duration::from_millis(500)).await;
        while rx.try_recv().is_ok() {}

        // Perform a single route sync for the entire batch
        let orchestrator_upstream = format!("litebin-orchestrator:{}", config.port);
        let routes = match resolve_all_routes(&db, &config.domain, &orchestrator_upstream).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "route sync: failed to resolve routes");
                continue;
            }
        };

        let r = router.read().await.clone();
        if let Err(e) = r
            .sync_routes(
                &routes,
                &config.domain,
                &orchestrator_upstream,
                &config.dashboard_subdomain,
                &config.poke_subdomain,
                true,
            )
            .await
        {
            tracing::error!(error = %e, "route sync: failed to push routes");
        }
    }
}

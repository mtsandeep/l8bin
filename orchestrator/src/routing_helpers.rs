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
        let Some(mapped_port) = project.mapped_port else {
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
                    Some((host, public_ip)) => (format!("{}:{}", host, mapped_port), public_ip),
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
                (format!("litebin-{}:{}", project.id, internal_port), None)
            }
        };

        routes.push(ProjectRoute {
            project_id: project.id.clone(),
            subdomain_host: format!("{}.{}", project.id, domain),
            upstream,
            custom_domain: project.custom_domain.clone(),
            node_id: project.node_id.clone(),
            node_public_ip,
        });
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
        let all_running = match sqlx::query_as::<_, crate::db::models::Project>(
            "SELECT * FROM projects WHERE status = 'running'",
        )
        .fetch_all(&db)
        .await
        {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, "route sync: failed to fetch running projects");
                continue;
            }
        };

        let orchestrator_upstream = format!("litebin-orchestrator:{}", config.port);
        let routes = match resolve_routes(&all_running, &db, &config.domain).await {
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
            )
            .await
        {
            tracing::error!(error = %e, "route sync: failed to push routes");
        }
    }
}

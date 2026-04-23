use std::sync::Arc;
use std::time::Duration;

use sqlx::SqlitePool;
use tokio::sync::RwLock;

use crate::config::Config;
use litebin_common::routing::{ProjectCustomRoute, ProjectRoute, RoutingProvider};
use litebin_common::types::{container_name, Project, ProjectService};

/// Bulk-load custom routes for a set of project IDs. Returns a map of project_id -> Vec<ProjectCustomRoute>.
async fn resolve_custom_routes(
    db: &SqlitePool,
    project_ids: &[String],
) -> anyhow::Result<std::collections::HashMap<String, Vec<ProjectCustomRoute>>> {
    if project_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }

    let placeholders = project_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let query = format!(
        "SELECT id, project_id, route_type, path, subdomain, upstream, priority, created_at FROM project_routes WHERE project_id IN ({}) ORDER BY priority, created_at",
        placeholders
    );

    let mut builder = sqlx::query_as::<_, (String, String, String, Option<String>, Option<String>, String, i64, i64)>(&query);
    for pid in project_ids {
        builder = builder.bind(pid);
    }

    let rows = builder.fetch_all(db).await?;

    let mut map: std::collections::HashMap<String, Vec<ProjectCustomRoute>> = std::collections::HashMap::new();
    for (id, project_id, route_type, path, subdomain, upstream, priority, _created_at) in rows {
        map.entry(project_id.clone()).or_default().push(ProjectCustomRoute {
            id,
            project_id,
            route_type,
            path,
            subdomain,
            upstream,
            priority,
        });
    }

    Ok(map)
}

/// Resolve the upstream address for each project by looking up its assigned node.
/// Local projects use `host.docker.internal:{port}`, remote projects use `{node_host}:{port}`.
pub async fn resolve_routes(
    projects: &[Project],
    db: &SqlitePool,
    domain: &str,
) -> anyhow::Result<Vec<ProjectRoute>> {
    // Bulk-load custom routes for all projects
    let project_ids: Vec<String> = projects.iter().map(|p| p.id.clone()).collect();
    let custom_routes_map = resolve_custom_routes(db, &project_ids).await?;

    let mut routes = Vec::with_capacity(projects.len());

    for project in projects {
        let Some(_mapped_port) = project.mapped_port else {
            continue;
        };
        if project.status != "running" {
            continue;
        }

        // For multi-service projects, look up the public service from project_services
        let is_local = project.node_id.as_deref().map(|n| n == "local").unwrap_or(true);
        let (upstream_name, internal_port, container_upstream) = if project.service_count.unwrap_or(1) > 1 {
            let public_svc: Option<ProjectService> = sqlx::query_as(
                "SELECT * FROM project_services WHERE project_id = ? AND is_public = 1 AND status = 'running' LIMIT 1",
            )
            .bind(&project.id)
            .fetch_optional(db)
            .await?;

            match public_svc {
                Some(svc) => {
                    let port = svc.port.unwrap_or(project.internal_port.unwrap_or(0)) as u16;
                    let cname = container_name(&project.id, &svc.service_name, None);
                    let cu = if is_local { Some(format!("{}:{}", cname, port)) } else { None };
                    (cname, port as i64, cu)
                }
                None => {
                    // Fallback to single-service behavior
                    let port = project.internal_port.unwrap_or(0);
                    let cu = if is_local { Some(format!("litebin-{}:{}", project.id, port)) } else { None };
                    (format!("litebin-{}", project.id), port, cu)
                }
            }
        } else {
            let port = project.internal_port.unwrap_or(0);
            let cu = if is_local { Some(format!("litebin-{}:{}", project.id, port)) } else { None };
            (format!("litebin-{}", project.id), port, cu)
        };

        let (upstream, node_public_ip) = match project.node_id.as_deref() {
            Some(node_id) if node_id != "local" => {
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
                let local_ip: Option<String> = sqlx::query_scalar(
                    "SELECT public_ip FROM nodes WHERE id = 'local'",
                )
                .fetch_optional(db)
                .await?
                .flatten();
                (format!("{}:{}", upstream_name, internal_port), local_ip)
            }
        };

        let custom_routes = custom_routes_map.get(&project.id).cloned().unwrap_or_default();

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
            custom_routes,
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
            custom_routes: vec![], // sleeping projects don't need custom routes
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
    // Running single-service projects get direct container routes
    let all_running = sqlx::query_as::<_, Project>(
        "SELECT * FROM projects WHERE status = 'running' AND (service_count IS NULL OR service_count <= 1)",
    )
    .fetch_all(db)
    .await?;

    let mut routes = resolve_routes(&all_running, db, domain).await?;

    // Multi-service running projects always route through the orchestrator waker,
    // which health-checks all services on every request (throttled) and proxies to
    // the container when healthy. This ensures crashed backend services are detected
    // and recovered without depending on dashboard stats polling.
    let multi_running = sqlx::query_as::<_, Project>(
        "SELECT * FROM projects WHERE status = 'running' AND service_count > 1",
    )
    .fetch_all(db)
    .await?;

    // Load custom routes for multi-service projects
    let multi_ids: Vec<String> = multi_running.iter().map(|p| p.id.clone()).collect();
    let multi_custom_routes = resolve_custom_routes(db, &multi_ids).await?;

    for project in &multi_running {
        let is_local = project.node_id.as_deref().map(|n| n == "local").unwrap_or(true);
        let node_public_ip = if is_local {
            sqlx::query_scalar::<_, Option<String>>("SELECT public_ip FROM nodes WHERE id = 'local'")
                .fetch_optional(db)
                .await?
                .flatten()
        } else {
            let node_id = project.node_id.as_deref().unwrap_or("local");
            sqlx::query_scalar::<_, Option<String>>("SELECT public_ip FROM nodes WHERE id = ?")
                .bind(node_id)
                .fetch_optional(db)
                .await?
                .flatten()
        };

        routes.push(ProjectRoute {
            project_id: project.id.clone(),
            subdomain_host: format!("{}.{}", project.id, domain),
            upstream: if is_local { orchestrator_upstream.to_string() } else { format!("{}:443", project.node_id.as_deref().unwrap_or("localhost")) },
            custom_domain: project.custom_domain.clone(),
            node_id: project.node_id.clone(),
            node_public_ip,
            host_rewrite: None,
            upstream_tls: !is_local && project.node_id.is_some(),
            container_upstream: None,
            custom_routes: multi_custom_routes.get(&project.id).cloned().unwrap_or_default(),
        });
    }

    // Degraded projects (some services stopped) — route to orchestrator so waker can recover
    let degraded = sqlx::query_as::<_, Project>(
        "SELECT * FROM projects WHERE status = 'degraded'",
    )
    .fetch_all(db)
    .await?;

    for project in &degraded {
        let is_local = project.node_id.as_deref().map(|n| n == "local").unwrap_or(true);
        let node_public_ip = if is_local {
            sqlx::query_scalar::<_, Option<String>>("SELECT public_ip FROM nodes WHERE id = 'local'")
                .fetch_optional(db)
                .await?
                .flatten()
        } else {
            let node_id = project.node_id.as_deref().unwrap_or("local");
            sqlx::query_scalar::<_, Option<String>>("SELECT public_ip FROM nodes WHERE id = ?")
                .bind(node_id)
                .fetch_optional(db)
                .await?
                .flatten()
        };

        routes.push(ProjectRoute {
            project_id: project.id.clone(),
            subdomain_host: format!("{}.{}", project.id, domain),
            upstream: if is_local { orchestrator_upstream.to_string() } else { format!("{}:443", project.node_id.as_deref().unwrap_or("localhost")) },
            custom_domain: project.custom_domain.clone(),
            node_id: project.node_id.clone(),
            node_public_ip,
            host_rewrite: if is_local { None } else { Some(format!("{}.{}", project.id, domain)) },
            upstream_tls: !is_local && project.node_id.is_some(),
            container_upstream: None,
            custom_routes: vec![],
        });
    }

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

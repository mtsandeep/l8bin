use std::collections::HashMap;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use axum_login::AuthSession;
use litebin_common::{
    scan::{ScanGroup, ScanResult},
    types::{COMPOSE_FILE_NAMES, DeployType, Node, ProjectStatus, container_name, project_network_name},
};
use serde::{Deserialize, Serialize};

use crate::{
    AppState,
    auth::backend::PasswordBackend,
    nodes,
    routes::manage::{
        agent_base_url, ensure_project_dir_and_env, get_node_from_db,
        start_services, StartServicesOpts,
    },
    status,
};

// ── Scan ──────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ScanQuery {
    pub node_id: Option<String>,
}

/// GET /scan?node_id={local|all|<id>}
///
/// Returns foreign container groups from the requested node(s).
/// Default (no node_id) = "all".
pub async fn scan_containers(
    State(state): State<AppState>,
    Query(q): Query<ScanQuery>,
) -> impl IntoResponse {
    let node_id = q.node_id.as_deref().unwrap_or("all");

    // ── Local scan ──────────────────────────────────────────────────────────
    let local_groups = if node_id == "local" || node_id == "all" {
        match state.docker.scan_foreign_containers().await {
            Ok(groups) => groups,
            Err(e) => {
                tracing::error!(error = %e, "scan: local scan failed");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    // ── Remote agent scans ──────────────────────────────────────────────────
    let agent_nodes: Vec<Node> = if node_id == "all" {
        sqlx::query_as::<_, Node>(
            "SELECT * FROM nodes WHERE id != 'local' AND status = 'online' ORDER BY name",
        )
        .fetch_all(&state.db)
        .await
        .unwrap_or_default()
    } else if node_id != "local" {
        // Specific agent node
        match get_node_from_db(&state.db, node_id).await {
            Ok(n) => vec![n],
            Err((_, msg)) => {
                return (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": msg })))
                    .into_response();
            }
        }
    } else {
        Vec::new()
    };

    // Fan-out to all agent nodes in parallel
    let mut agent_futures = Vec::new();
    for node in &agent_nodes {
        let base_url = agent_base_url(&state.config, node);
        let node_id_owned = node.id.clone();
        let client = match nodes::client::get_node_client(&state.node_clients, &node.id) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(node_id = %node.id, error = %e, "scan: no client for node");
                continue;
            }
        };
        let fut = async move {
            let url = format!("{}/containers/scan", base_url);
            match client.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    match resp.json::<Vec<ScanGroup>>().await {
                        Ok(groups) => (node_id_owned, groups),
                        Err(e) => {
                            tracing::warn!(error = %e, "scan: failed to parse agent scan response");
                            (node_id_owned, Vec::new())
                        }
                    }
                }
                Ok(resp) => {
                    tracing::warn!(status = %resp.status(), "scan: agent returned error");
                    (node_id_owned, Vec::new())
                }
                Err(e) => {
                    tracing::warn!(error = %e, "scan: agent request failed");
                    (node_id_owned, Vec::new())
                }
            }
        };
        agent_futures.push(fut);
    }

    let agent_results: Vec<(String, Vec<ScanGroup>)> =
        futures_util::future::join_all(agent_futures).await;

    let mut nodes_map: HashMap<String, Vec<ScanGroup>> = HashMap::new();
    for (nid, groups) in agent_results {
        nodes_map.insert(nid, groups);
    }

    (StatusCode::OK, Json(ScanResult { local: local_groups, nodes: nodes_map })).into_response()
}

// ── Import ────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ImportGroupRequest {
    pub node_id: String,
    pub project_id: String,
    #[allow(dead_code)]
    pub group_key: String,
    pub public_service: Option<String>,
    pub setup_routing: bool,
    /// Full container data echoed back from the scan response.
    pub containers: Vec<litebin_common::scan::ScanContainer>,
    pub deploy_type: DeployType,
    pub compose_working_dir: Option<String>,
    pub compose_file_found: bool,
    #[allow(dead_code)]
    pub env_file_found: bool,
    pub name: Option<String>,
    pub description: Option<String>,
    pub allow_docker_access: Option<bool>,
}

#[derive(Deserialize)]
pub struct ImportRequest {
    pub groups: Vec<ImportGroupRequest>,
}

#[derive(Serialize)]
pub struct ImportedGroup {
    pub project_id: String,
    pub node_id: String,
    pub containers_imported: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Serialize)]
pub struct ImportResponse {
    pub imported: Vec<ImportedGroup>,
    pub errors: Vec<String>,
}

/// POST /scan/import
///
/// For each group:
/// 1. Write DB rows (projects, project_services, project_volumes)
/// 2. Resolve compose.yaml (copy from working_dir or reconstruct)
/// 3. Rewrite relative bind mounts to absolute paths
/// 4. Execute Docker import (local or via agent)
/// 5. Trigger route sync if setup_routing=true
pub async fn import_containers(
    State(state): State<AppState>,
    auth_session: AuthSession<PasswordBackend>,
    Json(req): Json<ImportRequest>,
) -> impl IntoResponse {
    let user_id = match auth_session.user {
        Some(u) => u.id.clone(),
        None => {
            return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "not authenticated" }))).into_response();
        }
    };
    let mut imported = Vec::new();
    let mut global_errors = Vec::new();

    // Pre-validate: no duplicate project_ids in the batch
    let mut seen_ids = std::collections::HashSet::new();
    for group in &req.groups {
        if !seen_ids.insert(group.project_id.clone()) {
            global_errors.push(format!("duplicate project_id '{}' in import request", group.project_id));
        }
    }
    // If there were duplicates, return early — don't import any
    if !global_errors.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(ImportResponse { imported, errors: global_errors })).into_response();
    }

    for group in req.groups {
        match import_single_group(&state, &user_id, group).await {
            Ok((result, setup_routing)) => {
                if setup_routing {
                    let _ = state.route_sync_tx.send(());
                }
                imported.push(result);
            }
            Err(e) => {
                tracing::error!(error = %e, "import: group failed");
                global_errors.push(e);
            }
        }
    }

    (StatusCode::OK, Json(ImportResponse { imported, errors: global_errors })).into_response()
}

async fn import_single_group(
    state: &AppState,
    user_id: &str,
    group: ImportGroupRequest,
) -> Result<(ImportedGroup, bool), String> {
    let project_id = &group.project_id;
    let mut warnings: Vec<String> = Vec::new();
    let now = chrono::Utc::now().timestamp();

    // Validate project ID doesn't already exist
    let exists: bool = sqlx::query_scalar("SELECT COUNT(*) FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_one(&state.db)
        .await
        .map(|n: i64| n > 0)
        .unwrap_or(false);
    if exists {
        return Err(format!("project '{}' already exists", project_id));
    }

    // Determine primary image + port from the public service container
    let public_container = group
        .containers
        .iter()
        .find(|c| group.public_service.as_deref() == Some(&c.service_name))
        .or_else(|| group.containers.iter().find(|c| c.suggested_public))
        .or_else(|| group.containers.first());

    let primary_image = public_container.map(|c| c.image.clone()).unwrap_or_default();
    let primary_port: Option<i64> = public_container
        .and_then(|c| c.ports.first())
        .map(|p| p.internal as i64);

    let service_count = group.containers.len() as i64;
    let service_summary = group
        .containers
        .iter()
        .map(|c| c.service_name.clone())
        .collect::<Vec<_>>()
        .join(",");

    // ── 1. Resolve compose.yaml content ────────────────────────────────────
    // Fetch the original compose.yaml (from agent for remote nodes, from local
    // filesystem for local node) and resolve `build:` → `image:` using the
    // actual image from each running container — same transformation as `l8b ship`.
    let (compose_yaml, env_content): (Option<String>, Option<String>) =
        if matches!(group.deploy_type, DeployType::Compose) && group.compose_file_found {
            if let Some(ref dir) = group.compose_working_dir {
                match fetch_compose_file_from_agent(state, &group.node_id, dir).await {
                    Ok((Some(raw), env)) => {
                        let resolved = resolve_compose_yaml(raw, &group.containers);
                        (Some(resolved), env)
                    }
                    Ok((None, env)) => {
                        warnings.push("compose.yaml not readable on agent; reconstructing from inspect data".into());
                        (Some(reconstruct_compose_yaml(&group.containers)), env)
                    }
                    Err(e) => {
                        warnings.push(format!("failed to fetch compose.yaml from agent: {e}; reconstructing"));
                        (Some(reconstruct_compose_yaml(&group.containers)), None)
                    }
                }
            } else {
                (Some(reconstruct_compose_yaml(&group.containers)), None)
            }
        } else if matches!(group.deploy_type, DeployType::Compose) {
            (Some(reconstruct_compose_yaml(&group.containers)), None)
        } else {
            (None, None)
        };

    // ── 2. Write DB rows ────────────────────────────────────────────────────
    let node_id_for_db = if group.node_id == "local" {
        None::<String>
    } else {
        Some(group.node_id.clone())
    };

    sqlx::query(
        "INSERT INTO projects (id, user_id, image, internal_port, status, node_id, \
         service_count, service_summary, deploy_type, name, description, allow_docker_access, \
         created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(project_id)
    .bind(user_id)
    .bind(&primary_image)
    .bind(primary_port)
    .bind(ProjectStatus::Importing.to_string())
    .bind(&node_id_for_db)
    .bind(service_count)
    .bind(&service_summary)
    .bind(group.deploy_type.to_string())
    .bind(&group.name)
    .bind(&group.description)
    .bind(group.allow_docker_access.unwrap_or(false) as i64)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await
    .map_err(|e| format!("db insert projects: {e}"))?;

    for container in &group.containers {
        let is_public = group.public_service.as_deref() == Some(&container.service_name)
            || (group.public_service.is_none() && container.suggested_public);
        let port: Option<i64> = container.ports.first().map(|p| p.internal as i64);

        sqlx::query(
            "INSERT INTO project_services \
             (project_id, service_name, image, port, is_public, status, container_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(project_id)
        .bind(&container.service_name)
        .bind(&container.image)
        .bind(port)
        .bind(is_public as i64)
        .bind(ProjectStatus::Running.to_string())
        .bind(&container.container_id)
        .execute(&state.db)
        .await
        .map_err(|e| format!("db insert project_services: {e}"))?;

        for vol in &container.volumes {
            sqlx::query(
                "INSERT OR IGNORE INTO project_volumes \
                 (project_id, service_name, volume_name, container_path) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(project_id)
            .bind(&container.service_name)
            .bind(&vol.source)
            .bind(&vol.destination)
            .execute(&state.db)
            .await
            .map_err(|e| format!("db insert project_volumes: {e}"))?;
        }
    }

    // ── 3. Build agent import payload ───────────────────────────────────────
    let network_name = project_network_name(project_id, None);
    let container_specs: Vec<serde_json::Value> = group
        .containers
        .iter()
        .map(|c| {
            let new_name = container_name(project_id, &c.service_name, None);
            serde_json::json!({
                "container_id": c.container_id,
                "new_name": new_name,
            })
        })
        .collect();

    let import_payload = serde_json::json!({
        "project_id": project_id,
        "network_name": network_name,
        "containers": container_specs,
        "compose_yaml": compose_yaml,
        "env_content": env_content,
        "allow_docker_access": group.allow_docker_access.unwrap_or(false),
    });

    let mut migrated_ids: Vec<String> = Vec::new();

    // ── 3b. Warn if docker.sock present without allow_docker_access ───────
    if !group.allow_docker_access.unwrap_or(false) {
        let has_sock = group.containers.iter().any(|c| {
            c.volumes.iter().any(|v| {
                v.source.contains("/docker.sock") || v.destination.contains("/docker.sock")
            })
        });
        if has_sock {
            warnings.push("Containers have Docker socket mounts but 'Allow Docker access' is disabled — the socket will not be available after redeploy".into());
        }
    }

    // ── 4. Docker import ─────────────────────────────────────────────────
    if group.node_id == "local" {
        // Local: perform Docker ops directly
        match do_local_import(state, project_id, &group.containers, &compose_yaml, &env_content).await {
            Ok((ids, local_warnings)) => {
                migrated_ids = ids;
                warnings.extend(local_warnings);
            }
            Err(e) => {
                warnings.push(format!("local docker import partial failure: {e}"));
            }
        }
    } else {
        // Remote agent
        let node = get_node_from_db(&state.db, &group.node_id)
            .await
            .map_err(|(_, msg)| msg)?;
        let base_url = agent_base_url(&state.config, &node);
        let client = nodes::client::get_node_client(&state.node_clients, &group.node_id)
            .map_err(|e| format!("no client for node {}: {e}", group.node_id))?;

        let url = format!("{}/containers/import", base_url);
        match client.post(&url).json(&import_payload).send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    migrated_ids = body["results"]
                        .as_array()
                        .unwrap_or(&vec![])
                        .iter()
                        .filter(|r| r["ok"].as_bool().unwrap_or(false))
                        .filter_map(|r| r["container_id"].as_str().map(|s| s.to_string()))
                        .collect();
                    if let Some(errs) = body["errors"].as_array() {
                        for err in errs {
                            if let Some(s) = err.as_str() {
                                warnings.push(s.to_string());
                            }
                        }
                    }
                }
            }
            Ok(resp) => {
                warnings.push(format!("agent import returned status {}", resp.status()));
            }
            Err(e) => {
                warnings.push(format!("agent import request failed: {e}"));
            }
        }
    }

    // Transition project status from Importing → derived status (typically Running)
    // since all services were inserted as Running above.
    status::derive_and_set_project_status(&state.db, project_id).await;

    // If allow_docker_access is enabled, start services to create the docker-proxy
    // sidecar. Existing containers will use the fast-path (already running).
    if group.allow_docker_access.unwrap_or(false) && group.node_id == "local" {
        let project = match sqlx::query_as::<_, crate::db::models::Project>(
            "SELECT * FROM projects WHERE id = ?",
        )
        .bind(project_id)
        .fetch_one(&state.db)
        .await
        {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(project_id = %project_id, error = %e, "import: failed to fetch project for proxy creation");
                return Ok((
                    ImportedGroup {
                        project_id: project_id.clone(),
                        node_id: group.node_id.clone(),
                        containers_imported: migrated_ids,
                        warnings,
                    },
                    group.setup_routing,
                ));
            }
        };
        if let Err(e) = start_services(&state, &project, StartServicesOpts {
            force_recreate: false,
            pull_images: false,
            force_pull: false,
            services: None,
            connect_orchestrator: true,
            rollback_on_failure: false,
        }).await {
            tracing::warn!(project_id = %project_id, error = ?e, "import: failed to create docker-proxy sidecar");
            warnings.push("docker-proxy sidecar creation failed after import".into());
        }
    }

    Ok((
        ImportedGroup {
            project_id: project_id.clone(),
            node_id: group.node_id.clone(),
            containers_imported: migrated_ids,
            warnings,
        },
        group.setup_routing,
    ))
}

/// Perform the Docker-side import for a local node:
/// rename containers, create network, connect them, write files.
async fn do_local_import(
    state: &AppState,
    project_id: &str,
    containers: &[litebin_common::scan::ScanContainer],
    compose_yaml: &Option<String>,
    env_content: &Option<String>,
) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let network_name = project_network_name(project_id, None);
    let mut warnings = Vec::new();

    // Create per-project network
    state.docker.ensure_project_network(project_id, None).await?;

    let mut migrated = Vec::new();

    for c in containers {
        let new_name = container_name(project_id, &c.service_name, None);

        // Rename (live — zero downtime)
        if let Err(e) = state.docker.rename_container(&c.container_id, &new_name).await {
            tracing::warn!(
                error = %e,
                container_id = %c.container_id,
                "local import: rename failed"
            );
            warnings.push(format!("rename {} -> {}: {}", c.container_id, new_name, e));
            continue;
        }

        // Connect to litebin project network
        if let Err(e) = state.docker
            .connect_container_to_network(&c.container_id, &network_name)
            .await
        {
            tracing::warn!(
                error = %e,
                container = %new_name,
                "local import: network connect failed"
            );
        }

        migrated.push(c.container_id.clone());
    }

    // Connect orchestrator itself to the new network
    let orchestrator_name = std::env::var("ORCHESTRATOR_CONTAINER_NAME")
        .unwrap_or_else(|_| "litebin-orchestrator".into());
    let _ = state.docker
        .connect_container_to_network(&orchestrator_name, &network_name)
        .await;

    // Write files to projects/{id}/
    ensure_project_dir_and_env(project_id);
    let project_dir = std::path::PathBuf::from("projects").join(project_id);

    if let Some(content) = compose_yaml {
        let _ = std::fs::write(project_dir.join("compose.yaml"), content);
    }
    if let Some(content) = env_content {
        let _ = std::fs::write(project_dir.join(".env"), content);
    }

    Ok((migrated, warnings))
}

/// Resolve a compose.yaml by replacing `build:` directives with `image:` using the
/// actual image from each running container — the same transformation `l8b ship` does.
///
/// For services that have `build:` but no `image:`, we look up the container's
/// current image (from Docker inspect) and inject it. The `build:` key is removed.
/// Services that already have `image:` are left untouched.
fn resolve_compose_yaml(
    raw: String,
    containers: &[litebin_common::scan::ScanContainer],
) -> String {
    // Build lookup maps from scan data
    let image_map: std::collections::HashMap<&str, &str> = containers
        .iter()
        .map(|c| (c.service_name.as_str(), c.image.as_str()))
        .collect();

    // Map: (service_name, container_destination) → absolute host source
    // Used to rewrite relative bind mounts to absolute paths.
    let bind_mount_map: std::collections::HashMap<(&str, &str), &str> = containers
        .iter()
        .flat_map(|c| {
            c.volumes.iter().filter_map(move |v| {
                if v.volume_type == "bind" {
                    Some((c.service_name.as_str(), v.destination.as_str(), v.source.as_str()))
                } else {
                    None
                }
            })
        })
        .map(|(svc, dest, src)| ((svc, dest), src))
        .collect();

    let mut compose: serde_yaml::Value = match serde_yaml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            // On Windows, compose files may have unescaped backslashes in quoted
            // strings (e.g. "D:\dev\foo:/bar").  Retry with backslashes normalized.
            let normalized = raw.replace('\\', "/");
            match serde_yaml::from_str(&normalized) {
                Ok(v) => v,
                Err(e2) => {
                    tracing::warn!(error = %e, retry_error = %e2, "resolve: failed to parse compose YAML, returning as-is");
                    return raw;
                }
            }
        }
    };

    let services = match compose.get_mut("services").and_then(|s| s.as_mapping_mut()) {
        Some(m) => m,
        None => return serde_yaml::to_string(&compose).unwrap_or(raw),
    };

    for (svc_key, svc_val) in services.iter_mut() {
        let svc_name = svc_key.as_str().unwrap_or_default();
        let svc_map = match svc_val.as_mapping_mut() {
            Some(m) => m,
            None => continue,
        };

        // ── Replace build: → image: ────────────────────────────────────────
        let has_build = svc_map.contains_key(&serde_yaml::Value::String("build".into()));
        let has_image = svc_map.contains_key(&serde_yaml::Value::String("image".into()));

        if has_build && !has_image {
            if let Some(&image) = image_map.get(svc_name) {
                svc_map.remove(&serde_yaml::Value::String("build".into()));
                svc_map.insert(
                    serde_yaml::Value::String("image".into()),
                    serde_yaml::Value::String(image.to_string()),
                );
                tracing::info!(
                    service = svc_name,
                    image = image,
                    "resolve: replaced build: with image: from running container"
                );
            }
        }

        // ── Rewrite relative bind mounts → absolute paths ──────────────────
        // The original compose may have `./data:/var/lib/data` relative to its
        // own working dir.  When stored under projects/<id>/, those relatives
        // would point to the wrong place.  Docker inspect gives us the resolved
        // absolute host path, so we substitute it in.
        if let Some(volumes_val) = svc_map.get_mut(&serde_yaml::Value::String("volumes".into())) {
            if let Some(vols) = volumes_val.as_sequence_mut() {
                for vol_entry in vols.iter_mut() {
                    // Handle both string form ("src:dst") and mapping form
                    let source_key = serde_yaml::Value::String("source".into());
                    let is_bind = vol_entry.as_mapping()
                        .map(|m| m.get(&serde_yaml::Value::String("type".into()))
                            .and_then(|t| t.as_str()) == Some("bind"))
                        .unwrap_or(false);

                    if is_bind {
                        // Long-form: { type: bind, source: "./data", target: "/var/lib/data" }
                        if let Some(m) = vol_entry.as_mapping_mut() {
                            let src = m.get(&source_key).and_then(|v| v.as_str()).map(|s| s.to_string());
                            let target = m.get(&serde_yaml::Value::String("target".into())).and_then(|t| t.as_str()).map(|s| s.to_string());
                            if let Some(ref src_str) = src {
                                if (src_str.starts_with('.') || src_str.starts_with("..")) && let Some(ref dest_str) = target {
                                    if let Some(&abs_source) = bind_mount_map.get(&(svc_name, dest_str.as_str())) {
                                        let normalized = abs_source.replace('\\', "/");
                                        m.insert(source_key.clone(), serde_yaml::Value::String(normalized));
                                        tracing::debug!(
                                            service = svc_name,
                                            old = src_str,
                                            new = abs_source,
                                            "resolve: rewrote relative bind mount to absolute"
                                        );
                                    }
                                }
                            }
                        }
                    } else {
                        // Short-form: "./data:/var/lib/data" or "./data:/var/lib/data:rw"
                        let vol_str = vol_entry.as_str().map(|s| s.to_string());
                        if let Some(ref vol_s) = vol_str {
                            let parts: Vec<&str> = vol_s.splitn(2, ':').collect();
                            if parts.len() == 2 {
                                let src = parts[0];
                                if src.starts_with('.') || src.starts_with("..") {
                                    let dest_parts: Vec<&str> = parts[1].split(':').collect();
                                    let dest = dest_parts[0];
                                    if let Some(&abs_source) = bind_mount_map.get(&(svc_name, dest)) {
                                        let normalized = abs_source.replace('\\', "/");
                                        let new_vol = format!("{}:{}", normalized, parts[1]);
                                        *vol_entry = serde_yaml::Value::String(new_vol);
                                        tracing::debug!(
                                            service = svc_name,
                                            old = src,
                                            new = abs_source,
                                            "resolve: rewrote relative bind mount to absolute"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    serde_yaml::to_string(&compose).unwrap_or(raw)
}

/// Reconstruct a minimal compose.yaml from container inspect data.
/// Used as fallback when no compose.yaml is available.
fn reconstruct_compose_yaml(containers: &[litebin_common::scan::ScanContainer]) -> String {
    let mut yaml = String::from("services:\n");
    for c in containers {
        yaml.push_str(&format!("  {}:\n", c.service_name));
        yaml.push_str(&format!("    image: {}\n", c.image));

        // Ports (include all ports, not just externally published ones)
        if !c.ports.is_empty() {
            yaml.push_str("    ports:\n");
            for p in &c.ports {
                if let Some(ext) = p.external {
                    yaml.push_str(&format!("      - \"{}:{}\"\n", ext, p.internal));
                } else {
                    yaml.push_str(&format!("      - \"{}\"\n", p.internal));
                }
            }
        }

        // Volumes — normalize Windows backslashes to forward slashes and quote
        // paths that contain colons (e.g. Windows paths like D:/foo:/bar)
        if !c.volumes.is_empty() {
            yaml.push_str("    volumes:\n");
            for v in &c.volumes {
                let source = v.source.replace('\\', "/");
                let spec = format!("{}:{}", source, v.destination);
                if spec.contains(':') && !spec.starts_with('"') {
                    yaml.push_str(&format!("      - \"{}\"\n", spec));
                } else {
                    yaml.push_str(&format!("      - {}\n", spec));
                }
            }
        }
    }
    yaml
}

/// Fetch compose.yaml and .env content for a node.
///
/// For **local** nodes, reads directly from the host filesystem (the orchestrator
/// has access via the Docker socket mount). For **remote agent** nodes, asks the
/// agent to read from its host filesystem via HTTP.
///
/// Returns `(compose_yaml, env_content)` — either may be None if the file wasn't found.
async fn fetch_compose_file_from_agent(
    state: &AppState,
    node_id: &str,
    working_dir: &str,
) -> anyhow::Result<(Option<String>, Option<String>)> {
    if node_id == "local" {
        // Local node: read directly from the filesystem
        let dir = std::path::Path::new(working_dir);
        if !dir.is_absolute() || !dir.is_dir() {
            anyhow::bail!("working dir '{}' is not an absolute path or does not exist", working_dir);
        }
        let compose_yaml = COMPOSE_FILE_NAMES
            .iter()
            .find_map(|name| std::fs::read_to_string(dir.join(name)).ok());
        let env_content = std::fs::read_to_string(dir.join(".env")).ok();
        return Ok((compose_yaml, env_content));
    }

    // Remote agent: fetch via HTTP
    let node = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
        .bind(node_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("node '{}' not found", node_id))?;

    let base_url = agent_base_url(&state.config, &node);
    let client = nodes::client::get_node_client(&state.node_clients, node_id)
        .map_err(|e| anyhow::anyhow!("no client for node {}: {}", node_id, e))?;

    // URL-encode the directory path (percent-encode UTF-8 bytes)
    let encoded_dir: String = working_dir
        .bytes()
        .flat_map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b':' | b'\\') {
                vec![b as char]
            } else {
                format!("%{:02X}", b).chars().collect()
            }
        })
        .collect();

    let url = format!("{}/containers/compose-file?dir={}", base_url, encoded_dir);

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("compose-file request failed: {e}"))?;

    if !resp.status().is_success() {
        anyhow::bail!("agent returned {} for compose-file", resp.status());
    }

    let body: serde_json::Value = resp
        .json::<serde_json::Value>()
        .await
        .map_err(|e| anyhow::anyhow!("compose-file parse failed: {e}"))?;

    let compose_yaml = body["compose_yaml"].as_str().map(|s: &str| s.to_string());
    let env_content = body["env_content"].as_str().map(|s: &str| s.to_string());

    Ok((compose_yaml, env_content))
}

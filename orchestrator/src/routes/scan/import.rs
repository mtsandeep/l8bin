use litebin_common::types::{ProjectStatus, container_name, project_network_name};

use crate::{
    AppState, nodes,
    routes::manage::{agent_base_url, ensure_project_dir_and_env, get_node_from_db},
    status,
};

use super::ImportGroupRequest;
use super::ImportedGroup;
use super::compose_transform::{reconstruct_compose_yaml, resolve_compose_yaml};

pub(super) async fn import_single_group(
    state: &AppState,
    user_id: &str,
    group: ImportGroupRequest,
) -> Result<(ImportedGroup, bool), String> {
    let project_id = &group.project_id;
    let mut warnings: Vec<String> = Vec::new();
    let now = chrono::Utc::now().timestamp();
    if group.containers.iter().any(|container| {
        container.volumes.iter().any(|volume| litebin_common::docker::bind_source_exposes_docker_socket(&volume.source))
    }) {
        return Err(
            "cannot import a running container whose host mounts expose the Docker daemon socket; redeploy it from sanitized Compose instead"
                .into(),
        );
    }
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
    let primary_port: Option<i64> = public_container.and_then(|c| c.ports.first()).map(|p| p.internal as i64);

    let service_count = group.containers.len() as i64;
    let service_summary = group.containers.iter().map(|c| c.service_name.clone()).collect::<Vec<_>>().join(",");

    // ── 1. Resolve compose.yaml content ────────────────────────────────────
    // Fetch the original compose.yaml (from agent for remote nodes, from local
    // filesystem for local node) and resolve `build:` → `image:` using the
    // actual image from each running container — same transformation as `l8b ship`.
    let (compose_yaml, env_content): (Option<String>, Option<String>) =
        if matches!(group.deploy_type, litebin_common::types::DeployType::Compose) && group.compose_file_found {
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
        } else if matches!(group.deploy_type, litebin_common::types::DeployType::Compose) {
            (Some(reconstruct_compose_yaml(&group.containers)), None)
        } else {
            (None, None)
        };

    // ── 2. Write DB rows ────────────────────────────────────────────────────
    let node_id_for_db = if group.node_id == "local" { None::<String> } else { Some(group.node_id.clone()) };

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
    .bind(0_i64)
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
    });

    let mut migrated_ids: Vec<String> = Vec::new();

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
        let node = get_node_from_db(&state.db, &group.node_id).await.map_err(|(_, msg)| msg)?;
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
        if let Err(e) = state.docker.connect_container_to_network(&c.container_id, &network_name).await {
            tracing::warn!(
                error = %e,
                container = %new_name,
                "local import: network connect failed"
            );
        }

        migrated.push(c.container_id.clone());
    }

    // Connect orchestrator itself to the new network
    let orchestrator_name =
        std::env::var("ORCHESTRATOR_CONTAINER_NAME").unwrap_or_else(|_| "litebin-orchestrator".into());
    let _ = state.docker.connect_container_to_network(&orchestrator_name, &network_name).await;

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
        let compose_yaml =
            litebin_common::types::find_compose_file(dir).and_then(|name| std::fs::read_to_string(dir.join(name)).ok());
        let env_content = std::fs::read_to_string(dir.join(".env")).ok();
        return Ok((compose_yaml, env_content));
    }

    // Remote agent: fetch via HTTP
    let node = sqlx::query_as::<_, litebin_common::types::Node>("SELECT * FROM nodes WHERE id = ?")
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

    let resp = client.get(&url).send().await.map_err(|e| anyhow::anyhow!("compose-file request failed: {e}"))?;

    if !resp.status().is_success() {
        anyhow::bail!("agent returned {} for compose-file", resp.status());
    }

    let body: serde_json::Value =
        resp.json::<serde_json::Value>().await.map_err(|e| anyhow::anyhow!("compose-file parse failed: {e}"))?;

    let compose_yaml = body["compose_yaml"].as_str().map(|s: &str| s.to_string());
    let env_content = body["env_content"].as_str().map(|s: &str| s.to_string());

    Ok((compose_yaml, env_content))
}

use axum::{extract::State, http::{HeaderMap, StatusCode}, response::IntoResponse, Json};
use axum::extract::Multipart;
use axum_login::AuthSession;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Semaphore;

use crate::auth::backend::PasswordBackend;
use crate::nodes;
use crate::routes::manage::agent_base_url;
use crate::AppState;
use litebin_common::types::{ProjectStatus, DeployType};
use crate::status::{self, ProjectUpdateFields};

#[utoipa::path(
    post,
    path = "/deploy/compose",
    request_body(content = String, description = "Multipart form with project_id and compose file"),
    responses(
        (status = 200, description = "Compose deployment started"),
        (status = 400, description = "Missing project_id"),
        (status = 401, description = "Authentication required"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "deploy",
    security(("session_auth" = []), ("bearer_token" = [])),
)]
/// POST /deploy/compose — Deploy a multi-service project via compose file.
///
/// Accepts multipart form data with:
/// - `project_id` (text field)
/// - `name` (optional text field)
/// - `description` (optional text field)
/// - `node_id` (optional text field)
/// - `auto_stop_enabled` (optional text field, "true"/"false")
/// - `auto_stop_timeout_mins` (optional text field)
/// - `auto_start_enabled` (optional text field, "true"/"false")
/// - `is_background` (optional text field, "true"/"false")
/// - `custom_domain` (optional text field)
/// - `compose` (file field — the docker-compose.yaml content)
pub async fn deploy_compose(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> impl IntoResponse {
    // Parse multipart fields
    let mut project_id = None;
    let mut name = None;
    let mut description = None;
    let mut node_id = None;
    let mut auto_stop_enabled = None;
    let mut auto_stop_timeout_mins = None;
    let mut auto_start_enabled = None;
    let mut is_background = None;
    let mut custom_domain = None;
    let mut allow_raw_ports = None;
    let mut grant_capabilities_raw = None;
    let mut compose_content = None;
    let mut target_services_raw = None;
    let mut stage_only_requested = false;

    while let Ok(Some(field)) = multipart.next_field().await {
        let field_name = match field.name() {
            Some(n) => n.to_string(),
            None => continue,
        };

        match field_name.as_str() {
            "project_id" => {
                project_id = field.text().await.ok();
            }
            "name" => {
                name = field.text().await.ok();
            }
            "description" => {
                description = field.text().await.ok();
            }
            "node_id" => {
                node_id = field.text().await.ok();
            }
            "auto_stop_enabled" => {
                auto_stop_enabled = field.text().await.ok().and_then(|v| v.parse::<bool>().ok());
            }
            "auto_stop_timeout_mins" => {
                auto_stop_timeout_mins = field.text().await.ok().and_then(|v| v.parse::<i64>().ok());
            }
            "auto_start_enabled" => {
                auto_start_enabled = field.text().await.ok().and_then(|v| v.parse::<bool>().ok());
            }
            "is_background" => {
                is_background = field.text().await.ok().and_then(|v| v.parse::<bool>().ok());
            }
            "custom_domain" => {
                custom_domain = field.text().await.ok();
            }
            "allow_raw_ports" => {
                allow_raw_ports = field.text().await.ok().and_then(|v| v.parse::<bool>().ok());
            }
            "grant_capabilities" => {
                grant_capabilities_raw = field.text().await.ok();
            }
            "compose" => {
                compose_content = field.bytes().await.ok();
            }
            "target_services" => {
                target_services_raw = field.text().await.ok();
            }
            "stage_only" => {
                stage_only_requested = field.text().await.ok()
                    .and_then(|v| v.parse::<bool>().ok())
                    .unwrap_or(false);
            }
            _ => {
                tracing::debug!(field = %field_name, "ignoring unknown multipart field");
            }
        }
    }

    let project_id = match project_id {
        Some(id) if !id.is_empty() => id,
        _ => return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "project_id is required"})),
        ).into_response(),
    };

    let compose_bytes = match compose_content {
        Some(b) if !b.is_empty() => b,
        _ => return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "compose file is required"})),
        ).into_response(),
    };

    let compose_yaml = match String::from_utf8(compose_bytes.to_vec()) {
        Ok(s) => s,
        Err(e) => return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("compose file is not valid UTF-8: {e}")})),
        ).into_response(),
    };

    // Authenticate
    let user_id = match auth_session.user {
        Some(u) => u.id.clone(),
        None => {
            match crate::auth::extract_deploy_token(&state, &headers, &project_id).await {
                Some(uid) => uid,
                None => return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error": "Authentication required. Use session login or provide a deploy token."})),
                ).into_response(),
            }
        }
    };

    // Basic validation
    if project_id == state.config.dashboard_subdomain || project_id == state.config.poke_subdomain {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "This ID is reserved"})),
        ).into_response();
    }
    if !crate::validation::is_valid_project_id(&project_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Project ID must be 1-63 lowercase letters, digits, or hyphens"})),
        ).into_response();
    }

    // Parse compose file with variable interpolation for validation.
    // The original YAML (with ${VAR} references) is stored to disk so env changes
    // take effect on restart; interpolation happens again at container start time.
    let compose = match compose_bollard::ComposeParser::parse_with_interpolation(&compose_yaml, &[]) {
        Ok(c) => c,
        Err(e) => return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("invalid compose YAML: {e}")})),
        ).into_response(),
    };

    if compose.services.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "compose file has no services"})),
        ).into_response();
    }

    // 4 validation checks
    // 1. Ghost deps
    let ghosts = compose.validate_ghost_deps();
    if !ghosts.is_empty() {
        let msg = ghosts.iter()
            .map(|(svc, dep)| format!("service '{svc}' depends on unknown service '{dep}'"))
            .collect::<Vec<_>>()
            .join("; ");
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("invalid dependencies: {msg}")})),
        ).into_response();
    }

    // 2. Cycles
    if let Some(cycle) = compose.detect_cycles() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("dependency cycle detected: {}", cycle.join(" -> "))})),
        ).into_response();
    }

    // 3. Topological sort (also validates DAG)
    let start_order = match compose.topological_sort() {
        Ok(order) => order,
        Err(e) => return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("invalid service graph: {e}")})),
        ).into_response(),
    };

    let existing_background: Option<bool> = sqlx::query_scalar("SELECT is_background FROM projects WHERE id = ?")
        .bind(&project_id).fetch_optional(&state.db).await.ok().flatten();
    let is_background = is_background.or(existing_background).unwrap_or(false);

    let public_service = if is_background {
        None
    } else {
        match compose.detect_public_service() {
            Ok(s) => s,
            Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("public service conflict: {e}")}))).into_response(),
        }
    };

    // 5. Compatibility report — reject unsupported fields; require capability grants
    let compat_report = match compose_bollard::analyze_compose_yaml_for_workload(
        &compose_yaml,
        public_service.as_deref(),
        Some(&project_id),
        is_background,
    ) {
        Ok((_, report)) => report,
        Err(e) => return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("compose compatibility error: {e}")})),
        ).into_response(),
    };
    if !compat_report.ok {
        let unsupported: Vec<_> = compat_report
            .unsupported()
            .map(|f| json!({
                "path": f.path,
                "service": f.service,
                "message": f.message,
            }))
            .collect();
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "compose file has unsupported fields",
                "unsupported": unsupported,
                "report": compat_report,
            })),
        ).into_response();
    }

    // Apply capability grants from the explicit list and the legacy raw-ports flag.
    let pending_grants = {
        use litebin_common::capabilities::ProjectCapability;
        let mut to_grant = Vec::new();
        if let Some(raw) = grant_capabilities_raw.as_deref() {
            for part in raw.split(',') {
                let id = part.trim();
                if id.is_empty() {
                    continue;
                }
                match ProjectCapability::parse(id) {
                    Some(c) => {
                        if !to_grant.contains(&c) {
                            to_grant.push(c);
                        }
                    }
                    None => {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({"error": format!("unknown capability '{id}'")})),
                        ).into_response();
                    }
                }
            }
        }
        if allow_raw_ports == Some(true) && !to_grant.contains(&ProjectCapability::RawPorts) {
            to_grant.push(ProjectCapability::RawPorts);
        }
        to_grant
    };
    // Enforce required capabilities before mutating the project row.
    {
        let existing_grants = match crate::capabilities::granted_ids(&state.db, &project_id).await {
            Ok(g) => g,
            Err(e) => {
                // Table may not exist yet on brand-new DBs mid-migration — treat as empty.
                tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to read capabilities");
                std::collections::HashSet::new()
            }
        };
        let mut effective = existing_grants;
        for c in &pending_grants {
            effective.insert(c.id().to_string());
        }
        // The legacy raw-ports flag also counts as approval for this request.
        if allow_raw_ports == Some(true) {
            effective.insert("raw-ports".into());
        }
        let missing = crate::capabilities::missing_capabilities(
            &compat_report.required_capabilities,
            &effective,
        );
        if !missing.is_empty() {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "error": "missing required capabilities",
                    "missing_capabilities": missing,
                    "report": compat_report,
                })),
            ).into_response();
        }
    }

    let now = chrono::Utc::now().timestamp();
    let auto_stop = if is_background { false } else { auto_stop_enabled.unwrap_or(true) };
    let auto_stop_mins = auto_stop_timeout_mins.unwrap_or(state.config.default_auto_stop_mins);
    let auto_start = if is_background { false } else { auto_start_enabled.unwrap_or(true) };

    // On redeploy, preserve existing sleep settings unless explicitly provided
    let existing_status: Option<ProjectStatus> = match sqlx::query_scalar(
        "SELECT status FROM projects WHERE id = ?"
    )
    .bind(&project_id)
    .fetch_optional(&state.db)
    .await
    {
        Ok(status) => status,
        Err(e) => {
            tracing::error!(project_id = %project_id, error = %e, "compose deploy: failed to check project existence");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "database error"}))).into_response();
        }
    };
    let is_update = existing_status.is_some();
    // First-deploy staging applies while pending or to legacy unstaged projects.
    let stage_only = stage_only_requested
        && matches!(existing_status, None | Some(ProjectStatus::Pending | ProjectStatus::Unconfigured));

    let (auto_stop, auto_stop_mins, auto_start) = if is_background {
        (false, auto_stop_mins, false)
    } else if is_update && auto_stop_enabled.is_none() && auto_start_enabled.is_none() {
        let existing = sqlx::query_as::<_, (bool, i64, bool)>(
            "SELECT auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled FROM projects WHERE id = ?"
        )
        .bind(&project_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
        match existing {
            Some((s, t, a)) => (s, t, a),
            None => (auto_stop, auto_stop_mins, auto_start),
        }
    } else {
        (auto_stop, auto_stop_mins, auto_start)
    };

    // Parse target_services from comma-separated string (sent by CLI on partial redeploy)
    let target_services: Option<Vec<String>> = target_services_raw.map(|s| {
        s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
    });

    tracing::info!(
        project_id = %project_id,
        services = start_order.len(),
        public = ?public_service,
        is_background,
        "compose deploy request received"
    );

    // Acquire deploy lock
    let semaphore = state
        .project_locks
        .entry(project_id.clone())
        .or_insert_with(|| Arc::new(Semaphore::new(1)))
        .clone();
    let _permit = semaphore.acquire().await.unwrap();

    // Capture old per-service image digests for cleanup after redeploy
    let existing_node_id: Option<String> = if is_update {
        sqlx::query_scalar("SELECT node_id FROM projects WHERE id = ?")
            .bind(&project_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
    } else {
        None
    };

    let old_service_digests = if is_update {
        crate::routes::manage::capture_service_digests(
            &state, &project_id, existing_node_id.as_deref(), None,
        ).await
    } else {
        std::collections::HashMap::new()
    };

    // Ensure project directory exists and write compose.yaml to disk
    crate::routes::manage::ensure_project_dir_and_env(&project_id);

    let compose_path = std::path::PathBuf::from("projects").join(&project_id).join("compose.yaml");
    if let Err(e) = std::fs::write(&compose_path, &compose_yaml) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("failed to write compose.yaml: {e}")})),
        ).into_response();
    }

    // Determine the public service's port for the projects row
    let public_svc = public_service.as_deref().map(|name| &compose.services[name]);
    let public_port: Option<i64> = public_svc.and_then(|svc| svc.exposed_ports().first().map(|(p, _)| *p as i64));
    let public_image: Option<String> = public_svc.and_then(|svc| svc.image.clone());

    // Build service_count and service_summary
    let service_count = compose.services.len() as i64;
    let service_summary = start_order.join(":");

    // On partial redeploy, project stays running (we're only updating a subset of services).
    // First-deploy staging remains pending until artifacts and runtime config are ready.
    let project_status = if stage_only {
        ProjectStatus::Pending
    } else if target_services.is_some() {
        ProjectStatus::Running
    } else {
        ProjectStatus::Deploying
    };

    // On redeploy, preserve existing raw-port access unless explicitly provided.
    let db_allow_raw_ports = if is_update && allow_raw_ports.is_none() {
        let existing = match sqlx::query_scalar::<_, bool>(
            "SELECT allow_raw_ports FROM projects WHERE id = ?"
        )
        .bind(&project_id)
        .fetch_optional(&state.db)
        .await
        {
            Ok(row) => row,
            Err(e) => {
                tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to read allow_raw_ports, using defaults");
                None
            }
        };
        existing.or(allow_raw_ports)
    } else {
        allow_raw_ports
    };

    let allow_raw_ports = db_allow_raw_ports.unwrap_or(false);

    // Upsert project row
    let result = sqlx::query(
        r#"
        INSERT INTO projects (id, user_id, name, description, is_background, image, internal_port, status, auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled, custom_domain, allow_raw_ports, allow_docker_access, service_count, service_summary, deploy_type, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(id) DO UPDATE SET
            user_id = excluded.user_id,
            is_background = excluded.is_background,
            image = excluded.image,
            internal_port = excluded.internal_port,
            container_id = CASE WHEN excluded.is_background = 1 THEN NULL ELSE projects.container_id END,
            mapped_port = CASE WHEN excluded.is_background = 1 THEN NULL ELSE projects.mapped_port END,
            status = CASE WHEN excluded.status = 'running' THEN projects.status ELSE excluded.status END,
            name = CASE WHEN excluded.name IS NOT NULL THEN excluded.name ELSE COALESCE(projects.name, excluded.name) END,
            description = CASE WHEN excluded.description IS NOT NULL THEN excluded.description ELSE COALESCE(projects.description, excluded.description) END,
            auto_stop_enabled = excluded.auto_stop_enabled,
            auto_stop_timeout_mins = excluded.auto_stop_timeout_mins,
            auto_start_enabled = excluded.auto_start_enabled,
            custom_domain = CASE WHEN excluded.custom_domain IS NOT NULL THEN excluded.custom_domain ELSE COALESCE(projects.custom_domain, excluded.custom_domain) END,
            allow_raw_ports = excluded.allow_raw_ports,
            allow_docker_access = excluded.allow_docker_access,
            service_count = excluded.service_count,
            service_summary = excluded.service_summary,
            deploy_type = excluded.deploy_type,
            updated_at = excluded.updated_at
        "#,
    )
    .bind(&project_id)
    .bind(&user_id)
    .bind(&name)
    .bind(&description)
    .bind(is_background)
    .bind(&public_image)
    .bind(public_port)
    .bind(project_status)
    .bind(auto_stop)
    .bind(auto_stop_mins)
    .bind(auto_start)
    .bind(&custom_domain)
    .bind(allow_raw_ports)
    .bind(false)
    .bind(service_count)
    .bind(&service_summary)
    .bind(DeployType::Compose)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await;

    if let Err(e) = result {
        let is_conflict = crate::validation::is_unique_constraint(&e);
        let status = if is_conflict {
            StatusCode::CONFLICT
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        return (
            status,
            Json(json!({"error": if is_conflict { format!("project '{}' already exists", project_id) } else { format!("database error: {e}") } })),
        ).into_response();
    }

    // Persist any newly approved capabilities (syncs legacy allow_* columns).
    if let Err(e) = crate::capabilities::grant_many(
        &state.db,
        &project_id,
        &pending_grants,
        Some(&user_id),
    )
    .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("failed to grant capabilities: {e}")})),
        ).into_response();
    }

    // Read project back from DB
    let project = match sqlx::query_as::<_, crate::db::models::Project>(
        "SELECT * FROM projects WHERE id = ?"
    )
    .bind(&project_id)
    .fetch_one(&state.db)
    .await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("database error: {e}")})),
            ).into_response();
        }
    };
    let docker_observe = match crate::capabilities::has_capability(
        &state.db,
        &project_id,
        litebin_common::capabilities::ProjectCapability::DockerObserve,
    )
    .await
    {
        Ok(granted) => granted,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("failed to read docker-observe grant: {e}")})),
            )
                .into_response();
        }
    };
    let host_network = match crate::capabilities::has_capability(
        &state.db,
        &project_id,
        litebin_common::capabilities::ProjectCapability::HostNetwork,
    )
    .await
    {
        Ok(granted) => granted,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("failed to read host-network grant: {e}")})),
            )
                .into_response();
        }
    };

    // Seed project_services rows for each service in the compose file
    let target_set: Option<std::collections::HashSet<String>> = target_services.as_ref()
        .map(|ts| ts.iter().cloned().collect());
    let oneshot_names = compose.oneshot_service_names();
    for svc_name in &start_order {
        let svc = &compose.services[svc_name];
        let image = svc.image.clone().unwrap_or_default();
        let port: Option<i64> = svc.ports.as_ref()
            .and_then(|p| p.first())
            .and_then(|p| p.split(':').last()?.parse().ok())
            .map(|p: u16| p as i64);
        let is_public = !is_background && public_service.as_deref() == Some(svc_name.as_str());
        let is_oneshot = oneshot_names.contains(svc_name);
        let depends_on = svc.depends_on.as_ref()
            .and_then(|d| serde_json::to_string(d).ok());
        let compose_mem: Option<i64> = svc.memory_bytes()
            .map(|bytes| (bytes / (1024 * 1024)) as i64);
        let compose_cpu: Option<f64> = svc.cpus.as_ref()
            .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok())));

        // On redeploy, preserve DB overrides when compose file doesn't specify memory/CPU
        let existing_override: Option<(Option<i64>, Option<f64>)> = sqlx::query_as(
            "SELECT memory_limit_mb, cpu_limit FROM project_services WHERE project_id = ? AND service_name = ?"
        )
        .bind(&project_id)
        .bind(svc_name)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
        let memory_limit_mb = compose_mem.or_else(|| existing_override.as_ref().and_then(|(m, _)| *m));
        let cpu_limit = compose_cpu.or_else(|| existing_override.as_ref().and_then(|(_, c)| *c));

        // On partial redeploy, only mark targeted services as 'deploying'
        let status = if stage_only {
            ProjectStatus::Pending
        } else if target_set.as_ref().map_or(true, |ts| ts.contains(svc_name)) {
            ProjectStatus::Deploying
        } else {
            // Preserve current status for non-targeted services
            ProjectStatus::Running
        };
        if let Err(e) = sqlx::query(
            "INSERT OR REPLACE INTO project_services (project_id, service_name, image, port, is_public, depends_on, memory_limit_mb, cpu_limit, status, is_oneshot)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(&project_id)
        .bind(svc_name)
        .bind(&image)
        .bind(port)
        .bind(is_public)
        .bind(&depends_on)
        .bind(memory_limit_mb)
        .bind(cpu_limit)
        .bind(status)
        .bind(is_oneshot)
        .execute(&state.db)
        .await
        {
            tracing::warn!(project_id = %project_id, service = %svc_name, error = %e, "compose deploy: failed to upsert project_services row");
        }
    }

    // Seed project_volumes rows from compose volume definitions
    if let Err(e) = sqlx::query("DELETE FROM project_volumes WHERE project_id = ?")
        .bind(&project_id)
        .execute(&state.db)
        .await
    {
        tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to delete existing volumes");
    }
    for svc_name in &start_order {
        let svc = &compose.services[svc_name];
        if let Some(ref vols) = svc.volumes {
            for vol_str in vols {
                // Parse "source:target[:mode]" format
                let parts: Vec<&str> = vol_str.splitn(3, ':').collect();
                if parts.len() >= 2 {
                    let volume_name = if !parts[0].is_empty() {
                        Some(litebin_common::types::scope_volume_source(parts[0], &project_id))
                    } else { None };
                    let container_path = parts[1].to_string();
                    if let Err(e) = sqlx::query(
                        "INSERT OR IGNORE INTO project_volumes (project_id, service_name, volume_name, container_path)
                         VALUES (?, ?, ?, ?)"
                    )
                    .bind(&project_id)
                    .bind(svc_name)
                    .bind(&volume_name)
                    .bind(&container_path)
                    .execute(&state.db)
                    .await
                    {
                        tracing::warn!(project_id = %project_id, service = %svc_name, error = %e, "compose deploy: failed to insert volume row");
                    }
                }
            }
        }
    }

    let target_node_id = match nodes::selector::select_node(&state.db, &project, node_id.clone()).await {
        Ok(id) => id,
        Err(e) => {
            if let Err(e) = status::transition(&state.db, &project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None).await {
                tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to transition to Error");
            }
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": format!("{:?}", e)})),
            ).into_response();
        }
    };
    if compose.services.values().any(|service| service.uses_host_network()) {
        let eligibility = if target_node_id == "local" {
            let host = state.docker.host_info().await.ok();
            litebin_common::docker::require_host_network_eligible(
                host.as_ref().and_then(|info| info.os_type.as_deref()),
                host.as_ref()
                    .and_then(|info| info.operating_system.as_deref()),
                host.as_ref().and_then(|info| info.rootless),
                Some(3),
            )
        } else {
            match crate::routes::manage::get_node_from_db(&state.db, &target_node_id).await {
                Ok(node) => match nodes::client::get_node_client(
                    &state.node_clients,
                    &target_node_id,
                ) {
                    Ok(client) => {
                        let health_url =
                            format!("{}/health", agent_base_url(&state.config, &node));
                        match client.get(health_url).send().await {
                            Ok(response) if response.status().is_success() => {
                                match response.json::<litebin_common::types::HealthReport>().await {
                                    Ok(health) => {
                                        litebin_common::docker::require_host_network_eligible(
                                            health.docker_os_type.as_deref(),
                                            health.docker_operating_system.as_deref(),
                                            health.docker_rootless,
                                            Some(health.protocol_version as i64),
                                        )
                                    }
                                    Err(error) => Err(anyhow::anyhow!(
                                        "failed to read selected agent health: {error}"
                                    )),
                                }
                            }
                            Ok(response) => Err(anyhow::anyhow!(
                                "selected agent health check returned {}",
                                response.status()
                            )),
                            Err(error) => Err(anyhow::anyhow!(
                                "failed to contact selected agent for host-network eligibility: {error}"
                            )),
                        }
                    }
                    Err(error) => Err(anyhow::anyhow!(
                        "selected agent client is unavailable: {error:?}"
                    )),
                },
                Err(error) => Err(anyhow::anyhow!("failed to load selected node: {error:?}")),
            }
        };
        if let Err(error) = eligibility {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
    }

    // Persist sticky node selection even for staged first deploys.
    if let Err(e) = sqlx::query(
        "UPDATE projects SET node_id = ?, updated_at = ? WHERE id = ?"
    )
    .bind(&target_node_id)
    .bind(now)
    .bind(&project_id)
    .execute(&state.db)
    .await
    {
        tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to persist node_id");
    }

    // First-deploy staging: prepare compose + runtime .env, do not start containers.
    if stage_only {
        if target_node_id != "local" {
            let node = match crate::routes::manage::get_node_from_db(&state.db, &target_node_id).await {
                Ok(n) => n,
                Err(e) => {
                    if let Err(e) = status::transition(&state.db, &project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None).await {
                        tracing::warn!(project_id = %project_id, error = %e, "compose stage: failed to transition to Error");
                    }
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"error": format!("{:?}", e)})),
                    ).into_response();
                }
            };

            let client = match nodes::client::get_node_client(&state.node_clients, &target_node_id) {
                Ok(c) => c,
                Err(e) => {
                    if let Err(e) = status::transition(&state.db, &project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None).await {
                        tracing::warn!(project_id = %project_id, error = %e, "compose stage: failed to transition to Error");
                    }
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"error": format!("node client unavailable: {:?}", e)})),
                    ).into_response();
                }
            };

            let base_url = agent_base_url(&state.config, &node);
            let stage_resp = match client
                .post(&format!("{}/containers/batch-run", base_url))
                .json(&json!({
                    "project_id": project_id,
                    "compose_yaml": compose_yaml,
                    "service_order": start_order,
                    "is_background": is_background,
                    "docker_observe": docker_observe,
                    "host_network": host_network,
                    "stage_only": true,
                }))
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(error = %e, "remote compose stage request failed");
                    if let Err(e) = status::transition(&state.db, &project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None).await {
                        tracing::warn!(project_id = %project_id, error = %e, "compose stage: failed to transition to Error");
                    }
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"error": format!("agent unreachable: {e}")})),
                    ).into_response();
                }
            };

            if !stage_resp.status().is_success() {
                let body = stage_resp.text().await.unwrap_or_default();
                tracing::error!(body = %body, "remote compose stage failed");
                if let Err(e) = status::transition(&state.db, &project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None).await {
                    tracing::warn!(project_id = %project_id, error = %e, "compose stage: failed to transition to Error");
                }
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": format!("remote stage failed: {body}")})),
                ).into_response();
            }
        }

        if let Err(e) = status::transition(
            &state.db,
            &project_id,
            ProjectStatus::Unconfigured,
            &ProjectUpdateFields::default(),
            None,
        ).await {
            tracing::error!(project_id = %project_id, error = %e, "compose stage: failed to mark project unconfigured");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "failed to persist staged deployment status"})),
            ).into_response();
        }

        tracing::info!(
            project_id = %project_id,
            node_id = %target_node_id,
            "compose deployment staged; awaiting runtime configuration"
        );

        return (
            StatusCode::OK,
            Json(json!({
                "status": "unconfigured",
                "project_id": project_id,
                "node_id": target_node_id,
                "url": if is_background { serde_json::Value::Null } else { json!(format!("https://{}.{}", project_id, state.config.domain)) },
                "message": "Deployment staged. Configure runtime secrets, then start the project.",
            })),
        ).into_response();
    }

    // Local vs remote deploy path
    if target_node_id != "local" {
        // Remote multi-service deploy via agent batch-run
        let node = match crate::routes::manage::get_node_from_db(&state.db, &target_node_id).await {
            Ok(n) => n,
            Err(e) => {
                if let Err(e) = status::transition(&state.db, &project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None).await {
                tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to transition to Error");
            }
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": format!("{:?}", e)})),
                ).into_response();
            }
        };

        let client = match nodes::client::get_node_client(&state.node_clients, &target_node_id) {
            Ok(c) => c,
            Err(e) => {
                if let Err(e) = status::transition(&state.db, &project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None).await {
                tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to transition to Error");
            }
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": format!("node client unavailable: {:?}", e)})),
                ).into_response();
            }
        };

        let base_url = agent_base_url(&state.config, &node);
        // Read per-service resource overrides and global defaults to send to agent
        let service_resources: std::collections::HashMap<String, serde_json::Value> = match sqlx::query_as::<_, (String, Option<i64>, Option<f64>)>(
            "SELECT service_name, memory_limit_mb, cpu_limit FROM project_services WHERE project_id = ?",
        )
        .bind(&project_id)
        .fetch_all(&state.db)
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to fetch service resource overrides");
                Vec::new()
            }
        }
        .into_iter()
        .filter_map(|(name, mem, cpu)| {
            if mem.is_some() || cpu.is_some() {
                Some((name, json!({ "memory_limit_mb": mem, "cpu_limit": cpu })))
            } else {
                None
            }
        })
        .collect();

        let default_mem: i64 = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'default_memory_limit_mb'")
            .fetch_one(&state.db).await.ok().and_then(|v: String| v.parse().ok()).unwrap_or(256);
        let default_cpu: f64 = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'default_cpu_limit'")
            .fetch_one(&state.db).await.ok().and_then(|v: String| v.parse().ok()).unwrap_or(0.5);

        let batch_resp = match client
            .post(&format!("{}/containers/batch-run", base_url))
            .json(&json!({
                "project_id": project_id,
                "compose_yaml": compose_yaml,
                "service_order": start_order,
                "target_services": target_services,
                "allow_raw_ports": project.allow_raw_ports,
                "docker_observe": docker_observe,
                "host_network": host_network,
                "is_background": project.is_background,
                "service_resources": service_resources,
                "default_memory_limit_mb": default_mem,
                "default_cpu_limit": default_cpu,
                "force_pull": true,
            }))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "remote batch-run request failed");
                if let Err(e) = status::transition(&state.db, &project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None).await {
                tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to transition to Error");
            }
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": format!("agent unreachable: {e}")})),
                ).into_response();
            }
        };

        if !batch_resp.status().is_success() {
            let status_code = batch_resp.status();
            let body = batch_resp.text().await.unwrap_or_default();
            tracing::error!(status = %status_code, body = %body, "remote batch-run failed");
            if let Err(e) = status::transition(&state.db, &project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None).await {
                tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to transition to Error");
            }
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": format!("remote batch-run failed: {body}")})),
            ).into_response();
        }

        let batch_result: serde_json::Value = match batch_resp.json().await {
            Ok(v) => v,
            Err(e) => {
                if let Err(e) = status::transition(&state.db, &project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None).await {
                tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to transition to Error");
            }
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("failed to parse batch-run response: {e}")})),
                ).into_response();
            }
        };

        let service_errors: Vec<String> = batch_result["services"].as_array().into_iter().flatten()
            .filter_map(|svc| svc["error"].as_str().map(|error| format!("{}: {}", svc["service_name"].as_str().unwrap_or("unknown"), error)))
            .collect();

        // Update project_services with container IDs and ports from agent response
        if let Some(services) = batch_result["services"].as_array() {
            for svc in services {
                let svc_name = svc["service_name"].as_str().unwrap_or("");
                let container_id = svc["container_id"].as_str();
                let mapped_port = svc["mapped_port"].as_u64().map(|p| p as i64);

                if let Some(cid) = container_id {
                    if let Err(e) = status::set_service_running(&state.db, &project_id, svc_name, cid, mapped_port).await {
                        tracing::warn!(project_id = %project_id, service = %svc_name, error = %e, "compose deploy: failed to set service running");
                    }
                } else {
                    if let Err(e) = status::set_service_stopped(&state.db, &project_id, svc_name).await {
                        tracing::warn!(project_id = %project_id, service = %svc_name, error = %e, "compose deploy: failed to set service stopped");
                    }
                }
            }

            // Set project's denormalized container_id to the public service
            let public_result = public_service.as_deref().and_then(|name| services.iter().find(|s| s["service_name"].as_str() == Some(name)));

            if let Some(pub_svc) = public_result {
                let cid = pub_svc["container_id"].as_str().unwrap_or("").to_string();
                let port = pub_svc["mapped_port"].as_u64().map(|p| p as i64);
                if let Err(e) = status::transition(
                    &state.db,
                    &project_id,
                    ProjectStatus::Running,
                    &ProjectUpdateFields {
                        container_id: Some(Some(cid)),
                        mapped_port: Some(Some(port.unwrap_or(0))),
                        node_id: Some(target_node_id.clone()),
                        last_active_at: Some(now),
                    },
                    None,
                ).await {
                    tracing::error!(project_id = %project_id, error = %e, "compose deploy: failed to transition to Running");
                }
            }
        }
        if !service_errors.is_empty() {
            let _ = status::transition(&state.db, &project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None).await;
            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "one or more services failed to start", "service_errors": service_errors}))).into_response();
        }
        status::derive_and_set_project_status(&state.db, &project_id).await;

        // Trigger route sync
        let _ = state.route_sync_tx.send(());

        // Clean up old per-service images by digest
        for (svc_name, digest) in &old_service_digests {
            let should_cleanup = target_services.as_ref()
                .map_or(true, |targets| targets.contains(svc_name));
            if should_cleanup {
                crate::routes::manage::cleanup_unused_image(
                    &state, existing_node_id.as_deref(), digest,
                ).await;
            }
        }

        // Collect warnings from agent response
        let agent_warnings: Vec<String> = batch_result["warnings"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        return (
            StatusCode::OK,
            Json(json!({
                "status": "deployed",
                "project_id": project_id,
                "url": if is_background { serde_json::Value::Null } else { json!(format!("https://{}.{}", project_id, state.config.domain)) },
                "warnings": agent_warnings,
            })),
        ).into_response();
    }

    // --- Local path: spawn background task for heavy lifting ---
    let state_clone = state.clone();
    let project_id_clone = project_id.clone();
    let project_clone = project.clone();
    let compose_clone = compose.clone();
    let start_order_clone = start_order.clone();
    let target_node_id_clone = target_node_id.clone();
    let target_services_clone = target_services.clone();
    let _public_service_clone = public_service.clone();
    let _custom_domain_clone = custom_domain.clone();
    let old_service_digests_clone = old_service_digests.clone();
    let existing_node_id_clone = existing_node_id.clone();

    tokio::spawn(async move {
        crate::routes::deploy::logs::push_deploy_log(&state_clone, &project_id_clone, "Compose deployment started");

        let result: Result<(), anyhow::Error> = async {
            // Partial redeploy: only recreate targeted services
            if let Some(ref targets) = target_services_clone {
                tracing::info!(project = %project_id_clone, targets = ?targets, "partial compose redeploy");

                let target_set: std::collections::HashSet<String> = targets.iter().cloned().collect();

                // Stop and remove targeted service containers
                let prefix = format!("litebin-{}.", project_id_clone);
                if let Ok(all_containers) = state_clone.docker.list_containers_by_prefix(&prefix).await {
                    for cid in &all_containers {
                        if let Ok(inspect) = state_clone.docker.inspect_container(cid).await {
                            if let Some(ref name) = inspect.name {
                                let trimmed = name.trim_start_matches('/');
                                if let Some(svc_name) = trimmed.strip_prefix(&prefix) {
                                    if target_set.contains(svc_name) {
                                        let _ = state_clone.docker.stop_container(cid).await;
                                        let _ = state_clone.docker.remove_container(cid).await;
                                        if let Err(e) = status::set_service_stopped(&state_clone.db, &project_id_clone, svc_name).await {
                                            tracing::warn!(project_id = %project_id_clone, service = %svc_name, error = %e, "compose partial redeploy: failed to set service stopped");
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Start only the targeted services
                if let Err((_, msg)) = crate::routes::manage::start_services(
                    &state_clone,
                    &project_clone,
                    crate::routes::manage::StartServicesOpts {
                        force_recreate: true,
                        pull_images: false,
                        force_pull: false,
                        services: Some(target_set),
                        connect_orchestrator: true,
                        rollback_on_failure: true,
                    },
                ).await {
                    anyhow::bail!("start_services failed: {}", msg);
                }
            } else {
                // Full deploy: clean up existing containers, pull images, start all services
                let prefix = format!("litebin-{}.", project_id_clone);
                if let Ok(all_containers) = state_clone.docker.list_containers_by_prefix(&prefix).await {
                    for cid in &all_containers {
                        let _ = state_clone.docker.stop_container(cid).await;
                        let _ = state_clone.docker.remove_container(cid).await;
                    }
                }

                // Pull all images before starting (fail on any pull error)
                let images: Vec<String> = start_order_clone.iter()
                    .filter_map(|name| compose_clone.services[name].image.clone())
                    .collect();
                let mut pull_errors = Vec::new();
                for image in &images {
                    if !image.starts_with("sha256:") {
                        let log_state = state_clone.clone();
                        let log_project_id = project_id_clone.clone();
                        let on_progress: Box<dyn Fn(&str) + Send + Sync> = Box::new(move |msg: &str| {
                            crate::routes::deploy::logs::push_deploy_log(&log_state, &log_project_id, msg);
                        });
                        if let Err(e) = state_clone.docker.pull_image_with_progress(image, false, Some(on_progress)).await {
                            pull_errors.push(format!("{}: {}", image, e));
                        }
                    }
                }
                if !pull_errors.is_empty() {
                    let msg = pull_errors.join("; ");
                    anyhow::bail!("failed to pull images: {}", msg);
                }

                // Start all services using the unified function
                if let Err((_, msg)) = crate::routes::manage::start_services(
                    &state_clone,
                    &project_clone,
                    crate::routes::manage::StartServicesOpts {
                        force_recreate: true,
                        pull_images: false, // already pulled above
                        force_pull: false,
                        services: None,
                        connect_orchestrator: true,
                        rollback_on_failure: true,
                    },
                ).await {
                    anyhow::bail!("start_services failed: {}", msg);
                }
            }

            // Persist node_id for sticky scheduling on redeploys
            if let Err(e) = sqlx::query(
                "UPDATE projects SET node_id = ?, updated_at = ? WHERE id = ?"
            )
            .bind(&target_node_id_clone)
            .bind(chrono::Utc::now().timestamp())
            .bind(&project_id_clone)
            .execute(&state_clone.db)
            .await
            {
                tracing::warn!(project_id = %project_id_clone, error = %e, "compose deploy: failed to persist node_id");
            }

            // Full route sync after deploy
            crate::routes::deploy::logs::push_deploy_log(&state_clone, &project_id_clone, "Syncing routes...");
            let orchestrator_upstream = format!("litebin-orchestrator:{}", state_clone.config.port);
            let route_entries = crate::routing_helpers::resolve_all_routes(&state_clone.db, &state_clone.config.domain, &orchestrator_upstream).await?;
            let _ = state_clone
                .router
                .read()
                .await
                .sync_routes(&route_entries, &state_clone.config.domain, &orchestrator_upstream, &state_clone.config.dashboard_subdomain, &state_clone.config.poke_subdomain, true)
                .await;

            tracing::info!(
                project_id = %project_id_clone,
                services = start_order_clone.len(),
                "compose deploy complete"
            );

            crate::routes::deploy::logs::push_deploy_log(&state_clone, &project_id_clone, "Routes synced");
            crate::routes::deploy::logs::push_deploy_log(&state_clone, &project_id_clone, "Deployment complete");
            crate::routes::deploy::logs::clear_deploy_logs(&state_clone, &project_id_clone);

            // Trigger route sync for downstream consumers
            let _ = state_clone.route_sync_tx.send(());

            // Clean up old per-service images by digest
            for (svc_name, digest) in &old_service_digests_clone {
                let should_cleanup = target_services_clone.as_ref()
                    .map_or(true, |targets| targets.contains(svc_name));
                if should_cleanup {
                    crate::routes::manage::cleanup_unused_image(
                        &state_clone, existing_node_id_clone.as_deref(), digest,
                    ).await;
                }
            }

            Ok(())
        }.await;

        if let Err(e) = result {
            tracing::error!(project_id = %project_id_clone, error = %e, "background compose deploy failed");
            crate::routes::deploy::logs::push_deploy_log(&state_clone, &project_id_clone, &format!("Deploy failed: {}", e));
            if let Err(e) = status::transition(&state_clone.db, &project_id_clone, ProjectStatus::Error, &ProjectUpdateFields::default(), None).await {
                tracing::warn!(project_id = %project_id_clone, error = %e, "compose deploy: failed to transition to Error in background task");
            }
        }
    });

    // Explain the fail-closed translation when observation was not granted.
    let sock_warnings: Vec<String> = if !docker_observe && compose_yaml.contains("/docker.sock") {
        vec!["Docker socket declaration found without docker-observe — the raw socket was removed".into()]
    } else {
        vec![]
    };

    (
        StatusCode::OK,
        Json(json!({
            "status": "deploying",
            "project_id": project_id,
            "url": if is_background { serde_json::Value::Null } else { json!(format!("https://{}.{}", project_id, state.config.domain)) },
            "message": "Compose deployment started in background",
            "warnings": sock_warnings,
        })),
    )
        .into_response()
}

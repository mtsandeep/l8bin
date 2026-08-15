use axum::{Json, http::StatusCode, response::IntoResponse};
use serde_json::json;

use crate::AppState;
use litebin_common::types::ProjectStatus;

use super::form::ComposeForm;

/// Everything the later deploy phases need from a validated compose request.
pub(super) struct ValidatedCompose {
    pub compose: compose_bollard::ComposeFile,
    pub start_order: Vec<String>,
    pub public_service: Option<String>,
    pub is_background: bool,
    pub is_update: bool,
    pub stage_only: bool,
    pub auto_stop: bool,
    pub auto_stop_mins: i64,
    pub auto_start: bool,
    pub target_services: Option<Vec<String>>,
    pub pending_grants: Vec<litebin_common::capabilities::ProjectCapability>,
}

/// Validate the compose request: reserved IDs, YAML parse, graph checks
/// (ghost deps / cycles / topo sort), background + public-service detection,
/// the compatibility report, capability grant enforcement, sleep settings
/// resolution, and target_services parsing.
pub(super) async fn validate_compose_request(
    state: &AppState,
    form: &ComposeForm,
) -> Result<ValidatedCompose, axum::response::Response> {
    let project_id = &form.project_id;

    // Basic validation
    if *project_id == state.platform.dashboard_subdomain() || *project_id == state.platform.poke_subdomain() {
        return Err((StatusCode::BAD_REQUEST, Json(json!({"error": "This ID is reserved"}))).into_response());
    }
    if !crate::validation::is_valid_project_id(project_id) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Project ID must be 1-63 lowercase letters, digits, or hyphens"})),
        )
            .into_response());
    }

    // Parse compose file with variable interpolation for validation.
    // The original YAML (with ${VAR} references) is stored to disk so env changes
    // take effect on restart; interpolation happens again at container start time.
    let compose = match compose_bollard::ComposeParser::parse_with_interpolation(&form.compose_yaml, &[], false) {
        Ok(c) => c,
        Err(e) => {
            return Err(
                (StatusCode::BAD_REQUEST, Json(json!({"error": format!("invalid compose YAML: {e}")}))).into_response()
            );
        }
    };

    if compose.services.is_empty() {
        return Err((StatusCode::BAD_REQUEST, Json(json!({"error": "compose file has no services"}))).into_response());
    }

    // 4 validation checks
    // 1. Ghost deps
    let ghosts = compose.validate_ghost_deps();
    if !ghosts.is_empty() {
        let msg = ghosts
            .iter()
            .map(|(svc, dep)| format!("service '{svc}' depends on unknown service '{dep}'"))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(
            (StatusCode::BAD_REQUEST, Json(json!({"error": format!("invalid dependencies: {msg}")}))).into_response()
        );
    }

    // 2. Cycles
    if let Some(cycle) = compose.detect_cycles() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("dependency cycle detected: {}", cycle.join(" -> "))})),
        )
            .into_response());
    }

    // 3. Topological sort (also validates DAG)
    let start_order = match compose.topological_sort() {
        Ok(order) => order,
        Err(e) => {
            return Err((StatusCode::BAD_REQUEST, Json(json!({"error": format!("invalid service graph: {e}")})))
                .into_response());
        }
    };

    let existing_background: Option<bool> = sqlx::query_scalar("SELECT is_background FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let is_background = form.is_background.or(existing_background).unwrap_or(false);

    let public_service = if is_background {
        None
    } else {
        match compose.detect_public_service() {
            Ok(s) => s,
            Err(e) => {
                return Err((StatusCode::BAD_REQUEST, Json(json!({"error": format!("public service conflict: {e}")})))
                    .into_response());
            }
        }
    };

    // 5. Compatibility report — reject unsupported fields; require capability grants
    let compat_report = match compose_bollard::analyze_compose_yaml_for_workload(
        &form.compose_yaml,
        public_service.as_deref(),
        Some(project_id),
        is_background,
    ) {
        Ok((_, report)) => report,
        Err(e) => {
            return Err((StatusCode::BAD_REQUEST, Json(json!({"error": format!("compose compatibility error: {e}")})))
                .into_response());
        }
    };
    if !compat_report.ok {
        let unsupported: Vec<_> = compat_report
            .unsupported()
            .map(|f| {
                json!({
                    "path": f.path,
                    "service": f.service,
                    "message": f.message,
                })
            })
            .collect();
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "compose file has unsupported fields",
                "unsupported": unsupported,
                "report": compat_report,
            })),
        )
            .into_response());
    }

    // Apply capability grants from the explicit list and the legacy raw-ports flag.
    let pending_grants = {
        use litebin_common::capabilities::ProjectCapability;
        let mut to_grant = Vec::new();
        if let Some(raw) = form.grant_capabilities_raw.as_deref() {
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
                        return Err((
                            StatusCode::BAD_REQUEST,
                            Json(json!({"error": format!("unknown capability '{id}'")})),
                        )
                            .into_response());
                    }
                }
            }
        }
        if form.allow_raw_ports == Some(true) && !to_grant.contains(&ProjectCapability::RawPorts) {
            to_grant.push(ProjectCapability::RawPorts);
        }
        to_grant
    };
    // Enforce required capabilities before mutating the project row.
    let effective_grants = {
        let existing_grants = match crate::capabilities::granted_ids(&state.db, project_id).await {
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
        if form.allow_raw_ports == Some(true) {
            effective.insert("raw-ports".into());
        }
        let missing = crate::capabilities::missing_capabilities(&compat_report.required_capabilities, &effective);
        if !missing.is_empty() {
            return Err((
                StatusCode::FORBIDDEN,
                Json(json!({
                    "error": "missing required capabilities",
                    "missing_capabilities": missing,
                    "report": compat_report,
                })),
            )
                .into_response());
        }
        effective
    };
    let requests_host_network = compose.services.values().any(|service| service.uses_host_network());
    debug_assert!(
        !requests_host_network || effective_grants.contains("host-network"),
        "required host-network capability was checked above"
    );

    let auto_stop = if is_background { false } else { form.auto_stop_enabled.unwrap_or(true) };
    let auto_stop_mins = form.auto_stop_timeout_mins.unwrap_or(state.config.default_auto_stop_mins);
    let auto_start = if is_background { false } else { form.auto_start_enabled.unwrap_or(true) };

    // On redeploy, preserve existing sleep settings unless explicitly provided
    let existing_status: Option<ProjectStatus> = match sqlx::query_scalar("SELECT status FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
    {
        Ok(status) => status,
        Err(e) => {
            tracing::error!(project_id = %project_id, error = %e, "compose deploy: failed to check project existence");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "database error"}))).into_response());
        }
    };
    let is_update = existing_status.is_some();
    // First-deploy staging applies while pending or to legacy unstaged projects.
    let stage_only = form.stage_only_requested
        && matches!(existing_status, None | Some(ProjectStatus::Pending | ProjectStatus::Unconfigured));

    let (auto_stop, auto_stop_mins, auto_start) = if is_background {
        (false, auto_stop_mins, false)
    } else if is_update && form.auto_stop_enabled.is_none() && form.auto_start_enabled.is_none() {
        let existing = sqlx::query_as::<_, (bool, i64, bool)>(
            "SELECT auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled FROM projects WHERE id = ?",
        )
        .bind(project_id)
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
    let target_services: Option<Vec<String>> = form
        .target_services_raw
        .as_ref()
        .map(|s| s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect());

    tracing::info!(
        project_id = %project_id,
        services = start_order.len(),
        public = ?public_service,
        is_background,
        "compose deploy request received"
    );

    Ok(ValidatedCompose {
        compose,
        start_order,
        public_service,
        is_background,
        is_update,
        stage_only,
        auto_stop,
        auto_stop_mins,
        auto_start,
        target_services,
        pending_grants,
    })
}

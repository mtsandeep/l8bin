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
    /// Required capabilities the user chose not to grant; start stays blocked
    /// until they are.
    pub deferred_capabilities: Vec<litebin_common::capabilities::ProjectCapability>,
    /// Effective compose YAML to persist (post host-port strip when requested).
    pub compose_yaml: String,
    /// (service, original spec) of the host bindings removed by the strip.
    pub stripped_ports: Vec<(String, String)>,
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

    // Optional host-port strip: rewrite explicit HOST:CONTAINER mappings to
    // container-only so the compose stops requiring raw-ports.
    let (compose_yaml, stripped_ports) = if form.strip_host_ports {
        match compose_bollard::strip_host_port_bindings(&form.compose_yaml) {
            Ok(strip) => (strip.yaml, strip.removed),
            Err(e) => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": format!("failed to strip host ports: {e}")})),
                )
                    .into_response());
            }
        }
    } else {
        (form.compose_yaml.clone(), Vec::new())
    };

    // Parse compose file with variable interpolation for validation.
    // The original YAML (with ${VAR} references) is stored to disk so env changes
    // take effect on restart; interpolation happens again at container start time.
    let compose = match compose_bollard::ComposeParser::parse_with_interpolation(&compose_yaml, &[], false) {
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
        &compose_yaml,
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

    // Parse target_services early — deferral rejects partial deploys below.
    let target_services: Option<Vec<String>> = form
        .target_services_raw
        .as_ref()
        .map(|s| s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect());

    // Deferrals stage the deploy without granting; start stays blocked until
    // the capability is granted later.
    let (pending_grants, deferred_capabilities) = {
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
        let mut to_defer = Vec::new();
        if let Some(raw) = form.defer_capabilities_raw.as_deref() {
            for part in raw.split(',') {
                let id = part.trim();
                if id.is_empty() {
                    continue;
                }
                match ProjectCapability::parse(id) {
                    Some(c) => {
                        if !to_defer.contains(&c) {
                            to_defer.push(c);
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
        if !to_defer.is_empty() && target_services.as_ref().is_some_and(|ts| !ts.is_empty()) {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "deferring capabilities is not supported for partial (target_services) deploys"})),
            )
                .into_response());
        }
        // Enforce required capabilities before mutating the project row.
        let effective = {
            let existing_grants = match crate::capabilities::granted_ids(&state.db, project_id).await {
                Ok(g) => g,
                Err(e) => {
                    // Table may not exist yet on brand-new DBs mid-migration — treat as empty.
                    tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to read capabilities");
                    std::collections::HashSet::new()
                }
            };
            let mut effective = existing_grants;
            for c in &to_grant {
                effective.insert(c.id().to_string());
            }
            // The legacy raw-ports flag also counts as approval for this request.
            if form.allow_raw_ports == Some(true) {
                effective.insert("raw-ports".into());
            }
            effective
        };
        match resolve_capability_decision(&compat_report.required_capabilities, &effective, &to_grant, &to_defer) {
            Ok((grants, deferred)) => (grants, deferred),
            Err(missing) => {
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
        }
    };

    let requested_stop = if is_background { false } else { form.auto_stop_enabled.unwrap_or(true) };
    let requested_mins = form.auto_stop_timeout_mins.unwrap_or(state.config.default_auto_stop_mins);
    let requested_start = if is_background { false } else { form.auto_start_enabled.unwrap_or(true) };

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
    // Deferral is only safe when no running workload depends on the outcome.
    let defer_eligible = matches!(
        existing_status,
        None | Some(ProjectStatus::Pending | ProjectStatus::Unconfigured | ProjectStatus::Stopped)
    );
    if !deferred_capabilities.is_empty() && !defer_eligible {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "cannot defer capabilities on a project with a running workload — grant them or abort",
                "deferred_capabilities": deferred_capabilities.iter().map(|c| c.id()).collect::<Vec<_>>(),
            })),
        )
            .into_response());
    }
    // First-deploy staging applies while pending or to legacy unstaged projects.
    let stage_only = !deferred_capabilities.is_empty()
        || (form.stage_only_requested
            && matches!(existing_status, None | Some(ProjectStatus::Pending | ProjectStatus::Unconfigured)));

    // Sleep settings: fields omitted from the form keep their stored values on
    // redeploy; first deploys fall back to the form value or platform default.
    let (stored_stop, stored_mins, stored_start) = if is_update {
        sqlx::query_as::<_, (bool, i64, bool)>(
            "SELECT auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled FROM projects WHERE id = ?",
        )
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or((requested_stop, requested_mins, requested_start))
    } else {
        (requested_stop, requested_mins, requested_start)
    };

    let (auto_stop, auto_stop_mins, auto_start) = if is_background {
        (false, form.auto_stop_timeout_mins.unwrap_or(stored_mins), false)
    } else {
        (
            form.auto_stop_enabled.unwrap_or(stored_stop),
            form.auto_stop_timeout_mins.unwrap_or(stored_mins),
            form.auto_start_enabled.unwrap_or(stored_start),
        )
    };

    tracing::info!(
        project_id = %project_id,
        services = start_order.len(),
        public = ?public_service,
        is_background,
        deferred = deferred_capabilities.len(),
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
        deferred_capabilities,
        compose_yaml,
        stripped_ports,
    })
}

/// One-line summary of removed host bindings, e.g. `backend 8080:8080, ui 3000:80`.
pub(super) fn strip_summary(stripped: &[(String, String)]) -> String {
    stripped.iter().map(|(service, spec)| format!("{service} {spec}")).collect::<Vec<_>>().join(", ")
}

/// Split a deploy's capability requirements into grants and deferrals.
/// Deferring an already-granted capability is a no-op; `Err` carries the
/// capability ids that are still missing.
pub(super) fn resolve_capability_decision(
    required: &[String],
    effective_grants: &std::collections::HashSet<String>,
    grant_req: &[litebin_common::capabilities::ProjectCapability],
    defer_req: &[litebin_common::capabilities::ProjectCapability],
) -> Result<
    (Vec<litebin_common::capabilities::ProjectCapability>, Vec<litebin_common::capabilities::ProjectCapability>),
    Vec<String>,
> {
    let missing: Vec<&String> = required.iter().filter(|c| !effective_grants.contains(c.as_str())).collect();
    if missing.is_empty() {
        return Ok((grant_req.to_vec(), Vec::new()));
    }
    let uncovered: Vec<String> = missing
        .iter()
        .filter(|c| !grant_req.iter().any(|g| g.id() == c.as_str()) && !defer_req.iter().any(|d| d.id() == c.as_str()))
        .map(|c| (*c).clone())
        .collect();
    if !uncovered.is_empty() {
        return Err(uncovered);
    }
    // Deferrals only matter for capabilities that are actually missing.
    let deferred: Vec<_> = defer_req.iter().copied().filter(|d| missing.iter().any(|m| m.as_str() == d.id())).collect();
    Ok((grant_req.to_vec(), deferred))
}

#[cfg(test)]
mod tests {
    use super::resolve_capability_decision;
    use litebin_common::capabilities::ProjectCapability;

    fn ids(caps: &[ProjectCapability]) -> Vec<&str> {
        caps.iter().map(|c| c.id()).collect()
    }

    fn granted_set(ids: &[&str]) -> std::collections::HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn fully_granted_requires_nothing() {
        let required = vec!["raw-ports".to_string()];
        let decision = resolve_capability_decision(&required, &granted_set(&["raw-ports"]), &[], &[]).unwrap();
        assert!(decision.0.is_empty() && decision.1.is_empty());
    }

    #[test]
    fn missing_without_defer_is_rejected() {
        let required = vec!["raw-ports".to_string(), "docker-observe".to_string()];
        let err = resolve_capability_decision(&required, &granted_set(&[]), &[], &[]).unwrap_err();
        assert_eq!(err, vec!["raw-ports".to_string(), "docker-observe".to_string()]);
    }

    #[test]
    fn partial_defer_still_rejects_the_rest() {
        let required = vec!["raw-ports".to_string(), "docker-observe".to_string()];
        let err =
            resolve_capability_decision(&required, &granted_set(&[]), &[], &[ProjectCapability::RawPorts]).unwrap_err();
        assert_eq!(err, vec!["docker-observe".to_string()]);
    }

    #[test]
    fn full_defer_defers_only_the_missing() {
        let required = vec!["raw-ports".to_string()];
        // host-network is deferred but granted/required nowhere — a no-op.
        let (grants, deferred) = resolve_capability_decision(
            &required,
            &granted_set(&[]),
            &[],
            &[ProjectCapability::RawPorts, ProjectCapability::HostNetwork],
        )
        .unwrap();
        assert!(grants.is_empty());
        assert_eq!(ids(&deferred), vec!["raw-ports"]);
    }

    #[test]
    fn grants_are_passed_through_alongside_defers() {
        let required = vec!["raw-ports".to_string(), "host-network".to_string()];
        let (grants, deferred) = resolve_capability_decision(
            &required,
            &granted_set(&[]),
            &[ProjectCapability::RawPorts],
            &[ProjectCapability::HostNetwork],
        )
        .unwrap();
        assert_eq!(ids(&grants), vec!["raw-ports"]);
        assert_eq!(ids(&deferred), vec!["host-network"]);
    }
}

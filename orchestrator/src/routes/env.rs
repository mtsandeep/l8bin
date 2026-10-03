//! Runtime env API. GET returns masked keys only (values are write-only);
//! PUT writes the node-local `projects/<id>/.env`, applied on container start.

use std::collections::{BTreeMap, HashMap};

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use axum_login::AuthSession;
use serde::{Deserialize, Serialize};

use litebin_common::agent_api::EnvWriteRequest;

use crate::AppState;
use crate::auth::backend::PasswordBackend;
use crate::auth::guard::TokenContext;
use crate::db::models::Project;
use crate::nodes;
use crate::routes::manage::helpers::{ensure_node_reachable, read_local_env_raw};

const MAX_ENV_BYTES: usize = 64 * 1024;

// ── Schemas ──────────────────────────────────────────────────────────────────

#[derive(Deserialize, utoipa::ToSchema)]
pub struct UpdateEnvRequest {
    /// KEY → value pairs to set (overwrites existing). Single-line values.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Keys to remove (applied after sets).
    #[serde(default)]
    pub delete: Vec<String>,
    /// `merge` (default) keeps unmentioned keys; `replace` keeps exactly the sets.
    #[serde(default)]
    pub mode: EnvUpdateMode,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum EnvUpdateMode {
    #[default]
    Merge,
    Replace,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct EnvVarInfo {
    pub key: String,
    /// Masked preview: first character + `•••` (fully hidden for short values).
    pub masked: String,
    /// Value length in characters.
    pub length: usize,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct EnvResponse {
    pub project_id: String,
    pub vars: Vec<EnvVarInfo>,
    /// `.env` differs from what the container was last started with.
    pub pending_apply: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct UpdateEnvResponse {
    pub project_id: String,
    pub vars: Vec<EnvVarInfo>,
    pub pending_apply: bool,
    pub message: String,
}

// ── Pure helpers (unit-tested) ───────────────────────────────────────────────

/// Masked preview: first char for long values; fully hidden when short.
pub(crate) fn mask_value(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    match chars.len() {
        0 => String::new(),
        n if n <= 4 => "*".repeat(n),
        _ => format!("{}•••", chars[0]),
    }
}

/// True for valid env var names: `[A-Za-z_][A-Za-z0-9_]*`.
fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn valid_value(value: &str) -> bool {
    !value.contains(['\n', '\r', '\0'])
}

/// Double-quoted so dotenvy round-trips values with spaces, `=` or `#`.
fn env_line(key: &str, value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("{key}=\"{escaped}\"")
}

/// Ordered pairs; malformed lines skipped, like the injection path.
fn parse_env(raw: &str) -> Vec<(String, String)> {
    dotenvy::Iter::new(raw.as_bytes()).filter_map(|item| item.ok()).collect()
}

/// New file content: merge rewrites lines in place (keeps comments);
/// replace writes exactly the sets.
fn apply_env_update(
    current_raw: &str,
    sets: &BTreeMap<String, String>,
    deletes: &[String],
    mode: EnvUpdateMode,
) -> String {
    if mode == EnvUpdateMode::Replace {
        return sets.iter().map(|(k, v)| env_line(k, v)).collect::<Vec<_>>().join("\n") + "\n";
    }

    let delete_set: std::collections::HashSet<&str> = deletes.iter().map(|s| s.as_str()).collect();
    let mut applied: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();

    for line in current_raw.lines() {
        let stripped = line.trim_start();
        let assignment = stripped.strip_prefix("export ").unwrap_or(stripped);
        let current_key = assignment.split('=').next().unwrap_or("").trim();
        let is_assignment = !current_key.is_empty() && assignment.contains('=');

        if is_assignment && delete_set.contains(current_key) {
            continue; // deleted
        }
        if is_assignment && let Some(value) = sets.get(current_key) {
            applied.insert(current_key);
            out.push(env_line(current_key, value));
            continue;
        }
        out.push(line.to_string());
    }

    let mut appended = false;
    for (k, v) in sets {
        if !applied.contains(k.as_str()) {
            if !appended && !out.is_empty() && !out.last().is_some_and(|l| l.is_empty()) {
                out.push(String::new());
            }
            appended = true;
            out.push(env_line(k, v));
        }
    }
    out.join("\n") + "\n"
}

fn masked_vars(raw: &str) -> Vec<EnvVarInfo> {
    parse_env(raw)
        .into_iter()
        .map(|(key, value)| EnvVarInfo { masked: mask_value(&value), length: value.chars().count(), key })
        .collect()
}

fn validate(payload: &UpdateEnvRequest) -> Result<(), (StatusCode, String)> {
    if payload.env.is_empty() && payload.delete.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "nothing to update: provide `env` and/or `delete`".into()));
    }
    for key in payload.env.keys() {
        if !valid_key(key) {
            return Err((StatusCode::BAD_REQUEST, format!("invalid key '{key}': must match [A-Za-z_][A-Za-z0-9_]*")));
        }
    }
    for key in &payload.delete {
        if !valid_key(key) {
            return Err((StatusCode::BAD_REQUEST, format!("invalid delete key '{key}'")));
        }
    }
    for value in payload.env.values() {
        if !valid_value(value) {
            return Err((StatusCode::BAD_REQUEST, "values must be single-line strings (no newlines)".into()));
        }
    }
    Ok(())
}

// ── Handlers ─────────────────────────────────────────────────────────────────

async fn load_project(state: &AppState, project_id: &str) -> Result<Project, (StatusCode, String)> {
    sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("database error: {e}")))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("project '{project_id}' not found")))
}

/// Current `.env` content + pending flag (local FS or node agent).
async fn read_env(state: &AppState, project: &Project) -> Result<(String, bool), (StatusCode, String)> {
    let node_id = project.node_id.as_deref().unwrap_or("local");
    if node_id == "local" {
        Ok((read_local_env_raw(&project.id), crate::routes::manage::helpers::local_env_has_changed(&project.id)))
    } else {
        ensure_node_reachable(state, node_id).await?;
        let agent = nodes::client::AgentClient::resolve(state, node_id)
            .await
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {e}")))?;
        let resp = agent
            .get_env_file(&project.id)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, format!("failed to read env from node: {e}")))?;
        Ok((resp.content, resp.has_pending_changes))
    }
}

#[utoipa::path(
    get,
    path = "/projects/{project_id}/env",
    params(("project_id" = String, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Env keys with masked previews (never plaintext values)", body = EnvResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden (insufficient token scope / project binding)"),
        (status = 404, description = "Project not found"),
        (status = 503, description = "Node offline"),
    ),
    tag = "env",
    security(
        ("session_auth" = []),
        ("bearer_token" = []),
    ),
)]
pub async fn get_project_env(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<EnvResponse>, (StatusCode, String)> {
    let project = load_project(&state, &project_id).await?;
    let (raw, pending_apply) = read_env(&state, &project).await?;
    Ok(Json(EnvResponse { project_id: project.id, vars: masked_vars(&raw), pending_apply }))
}

#[utoipa::path(
    put,
    path = "/projects/{project_id}/env",
    request_body = UpdateEnvRequest,
    params(("project_id" = String, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Env updated; applies on next container start", body = UpdateEnvResponse),
        (status = 400, description = "Invalid keys/values"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden (manage scope required)"),
        (status = 404, description = "Project not found"),
        (status = 503, description = "Node offline"),
    ),
    tag = "env",
    security(
        ("session_auth" = []),
        ("bearer_token" = []),
    ),
)]
pub async fn update_project_env(
    auth_session: AuthSession<PasswordBackend>,
    token: Option<Extension<TokenContext>>,
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Json(payload): Json<UpdateEnvRequest>,
) -> Result<Json<UpdateEnvResponse>, (StatusCode, String)> {
    validate(&payload)?;
    let project = load_project(&state, &project_id).await?;

    // Deterministic order for written keys.
    let sets: BTreeMap<String, String> = payload.env.into_iter().collect();
    let (current_raw, _) = read_env(&state, &project).await?;
    let new_content = apply_env_update(&current_raw, &sets, &payload.delete, payload.mode);
    if new_content.len() > MAX_ENV_BYTES {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, format!("env file would exceed {MAX_ENV_BYTES} bytes")));
    }

    let node_id = project.node_id.as_deref().unwrap_or("local");
    if node_id == "local" {
        crate::routes::manage::helpers::ensure_project_dir_and_env(&project.id);
        let env_path = std::path::PathBuf::from("projects").join(&project.id).join(".env");
        std::fs::write(&env_path, &new_content)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to write .env: {e}")))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&env_path, std::fs::Permissions::from_mode(0o600));
        }
    } else {
        let agent = nodes::client::AgentClient::resolve(&state, node_id)
            .await
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {e}")))?;
        agent
            .write_env_file(&EnvWriteRequest { project_id: project.id.clone(), content: new_content.clone() })
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, format!("failed to write env on node: {e}")))?;
    }

    // Audit: keys only, never values.
    let actor = if let Some(u) = &auth_session.user {
        format!("session user '{}'", u.username)
    } else if let Some(Extension(ctx)) = &token {
        format!("deploy-token(user {}, scope {})", ctx.user_id, ctx.scope)
    } else {
        "unknown".to_string()
    };
    tracing::info!(
        project = %project.id,
        actor = %actor,
        set_keys = ?sets.keys().collect::<Vec<_>>(),
        deleted_keys = ?payload.delete,
        mode = ?payload.mode,
        "runtime env updated"
    );

    let pending_apply = if node_id == "local" {
        crate::routes::manage::helpers::local_env_has_changed(&project.id)
    } else {
        // Assume pending; the next GET confirms against the snapshot.
        true
    };

    Ok(Json(UpdateEnvResponse {
        project_id: project.id,
        vars: masked_vars(&new_content),
        pending_apply,
        message: "Updated. Changes apply on the next container start or recreate.".to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_hide_all_but_first_char() {
        assert_eq!(mask_value("ab"), "**");
        assert_eq!(mask_value("abcd"), "****");
        assert_eq!(mask_value("abcde"), "a•••");
        assert_eq!(mask_value("super-secret-token"), "s•••");
        assert_eq!(mask_value(""), "");
    }

    #[test]
    fn merge_preserves_comments_and_replaces_values() {
        let current = "# comment\nA=\"1\"\nB=\"2\"\n";
        let mut sets = BTreeMap::new();
        sets.insert("B".to_string(), "two".to_string());
        sets.insert("C".to_string(), "three".to_string());
        let out = apply_env_update(current, &sets, &[], EnvUpdateMode::Merge);
        assert!(out.contains("# comment"));
        assert!(out.contains("A=\"1\""));
        assert!(out.contains("B=\"two\""));
        assert!(out.contains("C=\"three\""));
        let parsed: std::collections::HashMap<String, String> = parse_env(&out).into_iter().collect();
        assert_eq!(parsed.get("B").map(String::as_str), Some("two"));
        assert_eq!(parsed.get("C").map(String::as_str), Some("three"));
    }

    #[test]
    fn merge_deletes_keys() {
        let current = "A=\"1\"\nB=\"2\"\n";
        let out = apply_env_update(current, &BTreeMap::new(), &["A".to_string()], EnvUpdateMode::Merge);
        assert!(!out.contains("A="));
        assert!(out.contains("B=\"2\""));
    }

    #[test]
    fn replace_mode_writes_exactly_the_sets() {
        let current = "# gone\nA=\"1\"\n";
        let mut sets = BTreeMap::new();
        sets.insert("X".to_string(), "1".to_string());
        let out = apply_env_update(current, &sets, &[], EnvUpdateMode::Replace);
        assert_eq!(out, "X=\"1\"\n");
    }

    #[test]
    fn quoted_values_round_trip_special_characters() {
        let line = env_line("K", "a=b #not-a-comment \"quoted\" \\slash");
        let parsed = parse_env(&line);
        assert_eq!(parsed[0].1, "a=b #not-a-comment \"quoted\" \\slash");
    }

    #[test]
    fn masked_vars_never_contain_values() {
        let raw = "SECRET_TOKEN=\"hunter2hunter2\"\n";
        let vars = masked_vars(raw);
        assert_eq!(vars[0].key, "SECRET_TOKEN");
        assert_eq!(vars[0].masked, "h•••");
        assert_eq!(vars[0].length, 14);
        assert!(!serde_json::to_string(&vars).unwrap().contains("hunter2"));
    }
}

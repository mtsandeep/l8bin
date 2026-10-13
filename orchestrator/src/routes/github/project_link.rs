//! Per-project GitHub links: link/unlink, auto-deploy toggle, dispatch, rotate.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::github::workflow::{self, DeployKind, WorkflowSpec};
use crate::github::{self, GitHubApiError};

use super::connect::{gh_error, mark_degraded};
use super::{mint_project_token, split_repo};

/// Workflow inputs persisted as JSON in `workflow_inputs`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WorkflowInputs {
    pub port: Option<u16>,
    pub cmd: Option<String>,
    pub dockerfile: Option<String>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
    pub node_id: Option<String>,
    pub path: Option<String>,
    pub compose_services: Vec<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct LinkRequest {
    pub repo_full_name: String,
    pub repo_id: Option<i64>,
    /// App mode: installation the repo lives under (PAT mode: none).
    pub installation_id: Option<i64>,
    /// PAT mode: connection serving this repo.
    pub connection_id: Option<i64>,
    /// Empty/omitted = repo default branch.
    pub branch: Option<String>,
    /// "single" (default) | "compose".
    pub deploy_kind: Option<String>,
    pub compose_file: Option<String>,
    pub auto_deploy: Option<bool>,
    pub port: Option<u16>,
    pub cmd: Option<String>,
    pub dockerfile: Option<String>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
    pub node_id: Option<String>,
    /// Subdirectory of the repo the app lives in (empty = repo root).
    pub path: Option<String>,
    /// Commit message for the workflow file; defaults when empty.
    pub commit_message: Option<String>,
    pub compose_services: Option<Vec<String>>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ProjectGithubConfigResponse {
    pub project_id: String,
    pub repo_full_name: String,
    pub branch: String,
    pub workflow_path: String,
    pub deploy_kind: String,
    pub compose_file: Option<String>,
    pub auto_deploy: bool,
    pub inputs: WorkflowInputs,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct PatchConfigRequest {
    pub auto_deploy: bool,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct DeployRequest {
    /// Branch/ref to deploy; defaults to the linked branch.
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct UnlinkQuery {
    pub remove_files: Option<bool>,
}

/// Row shape shared by handlers.
struct LinkRow {
    project_id: String,
    repo_full_name: String,
    branch: String,
    workflow_path: String,
    deploy_kind: String,
    compose_file: Option<String>,
    workflow_inputs: Option<String>,
    auto_deploy: i64,
    deploy_token_id: Option<String>,
    installation_id: Option<i64>,
    connection_id: Option<i64>,
}

fn row_to_response(row: LinkRow) -> ProjectGithubConfigResponse {
    let inputs = row.workflow_inputs.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    ProjectGithubConfigResponse {
        project_id: row.project_id,
        repo_full_name: row.repo_full_name,
        branch: row.branch,
        workflow_path: row.workflow_path,
        deploy_kind: row.deploy_kind,
        compose_file: row.compose_file,
        auto_deploy: row.auto_deploy != 0,
        inputs,
    }
}

const LINK_COLUMNS: &str = "project_id, repo_full_name, branch, workflow_path, deploy_kind, compose_file, workflow_inputs, auto_deploy, deploy_token_id, installation_id, connection_id";

async fn load_link(state: &AppState, project_id: &str) -> Result<LinkRow, (StatusCode, Json<serde_json::Value>)> {
    let row = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            i64,
            Option<String>,
            Option<i64>,
            Option<i64>,
        ),
    >(format!("SELECT {LINK_COLUMNS} FROM project_github_config WHERE project_id = ?").as_str())
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": format!("database error: {e}")})))
    })?;
    row.map(
        |(
            project_id,
            repo_full_name,
            branch,
            workflow_path,
            deploy_kind,
            compose_file,
            workflow_inputs,
            auto_deploy,
            deploy_token_id,
            installation_id,
            connection_id,
        )| LinkRow {
            project_id,
            repo_full_name,
            branch,
            workflow_path,
            deploy_kind,
            compose_file,
            workflow_inputs,
            auto_deploy,
            deploy_token_id,
            installation_id,
            connection_id,
        },
    )
    .ok_or((
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error": format!("project '{project_id}' has no GitHub link")})),
    ))
}

/// Project lookup; returns the owner id for token minting.
async fn load_project(state: &AppState, project_id: &str) -> Result<String, (StatusCode, Json<serde_json::Value>)> {
    let row: Option<(String,)> = sqlx::query_as("SELECT user_id FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": format!("database error: {e}")})))
        })?;
    row.map(|(user_id,)| user_id).ok_or((
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error": format!("project '{project_id}' not found — stage it first")})),
    ))
}

fn spec_from(row: &LinkRow, inputs: &WorkflowInputs, auto_deploy: bool) -> WorkflowSpec {
    WorkflowSpec {
        project_id: row.project_id.clone(),
        branch: row.branch.clone(),
        auto_deploy,
        deploy_kind: if row.deploy_kind == "compose" { DeployKind::Compose } else { DeployKind::Single },
        port: inputs.port,
        cmd: inputs.cmd.clone(),
        dockerfile: inputs.dockerfile.clone(),
        memory_mb: inputs.memory_limit_mb,
        cpu: inputs.cpu_limit,
        node_id: inputs.node_id.clone(),
        path: inputs.path.clone(),
        compose_services: inputs.compose_services.clone(),
    }
}

/// The workflow's L8B_SERVER value.
fn dashboard_url(state: &AppState) -> String {
    let p = state.platform.snapshot();
    format!("https://{}.{}", p.dashboard_subdomain, p.domain)
}

/// Mark degraded on auth failures, then map the error.
async fn gh_error_degraded(
    state: &AppState,
    connection_id: Option<i64>,
    e: GitHubApiError,
) -> (StatusCode, Json<serde_json::Value>) {
    if matches!(e, GitHubApiError::AuthExpired) {
        mark_degraded(&state.db, connection_id, "GitHub rejected the stored credential").await;
    }
    gh_error(e)
}

#[utoipa::path(
    get,
    path = "/projects/{id}/github/config",
    description = "Read a project's GitHub link: repo, branch, path, workflow, and deploy token status.",
    params(("id" = String, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Link config", body = ProjectGithubConfigResponse),
        (status = 404, description = "No link for this project"),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn get_config(State(state): State<AppState>, Path(project_id): Path<String>) -> impl IntoResponse {
    match load_link(&state, &project_id).await {
        Ok(row) => (StatusCode::OK, Json(row_to_response(row))).into_response(),
        Err(e) => e.into_response(),
    }
}

#[utoipa::path(
    put,
    path = "/projects/{id}/github/config",
    description = "Create or replace a project's GitHub link (full config upsert).",
    params(("id" = String, Path, description = "Project ID")),
    request_body = LinkRequest,
    responses(
        (status = 200, description = "Linked — workflow + secrets written, token minted", body = ProjectGithubConfigResponse),
        (status = 404, description = "Project not found"),
        (status = 403, description = "Branch protected or missing workflow permissions"),
        (status = 503, description = "GitHub not connected"),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn put_config(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Json(body): Json<LinkRequest>,
) -> impl IntoResponse {
    let user_id = match load_project(&state, &project_id).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    let (owner, repo) = match split_repo(&body.repo_full_name) {
        Ok(v) => v,
        Err(e) => return (e.0, Json(serde_json::json!({"error": e.1}))).into_response(),
    };
    let client = match github::client_for_link(&state.db, body.connection_id, body.installation_id).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(project = %project_id, repo = %body.repo_full_name, error = %e, "github link failed: no usable credential");
            return gh_error(e).into_response();
        }
    };

    let deploy_kind = match body.deploy_kind.as_deref() {
        None | Some("single") => DeployKind::Single,
        Some("compose") => DeployKind::Compose,
        Some(other) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("deploy_kind must be 'single' or 'compose', got '{other}'")})),
            )
                .into_response();
        }
    };

    // Resolve branch + repo id (one API call), defaulting branch to the repo default.
    let detail = match client.repo_detail(&owner, &repo, body.path.as_deref()).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(project = %project_id, repo = %body.repo_full_name, error = %e, "github link failed: repo detail");
            return gh_error_degraded(&state, body.connection_id, e).await.into_response();
        }
    };
    let branch = body.branch.clone().filter(|b| !b.is_empty()).unwrap_or(detail.default_branch);
    let repo_id = body.repo_id.or(Some(detail.id));

    let inputs = WorkflowInputs {
        port: body.port,
        cmd: body.cmd,
        dockerfile: body.dockerfile,
        memory_limit_mb: body.memory_limit_mb,
        cpu_limit: body.cpu_limit,
        node_id: body.node_id,
        path: body.path.as_deref().filter(|p| !p.is_empty() && *p != ".").map(str::to_string),
        compose_services: body.compose_services.unwrap_or_default(),
    };
    let spec = WorkflowSpec {
        project_id: project_id.clone(),
        branch: branch.clone(),
        auto_deploy: body.auto_deploy.unwrap_or(false),
        deploy_kind,
        port: inputs.port,
        cmd: inputs.cmd.clone(),
        dockerfile: inputs.dockerfile.clone(),
        memory_mb: inputs.memory_limit_mb,
        cpu: inputs.cpu_limit,
        node_id: inputs.node_id.clone(),
        path: inputs.path.clone(),
        compose_services: inputs.compose_services.clone(),
    };

    let token_name = format!("github:{owner}/{repo}");
    let (token_id, token_plaintext) = match mint_project_token(&state.db, &user_id, &project_id, &token_name).await {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("failed to create deploy token: {e}")})),
            )
                .into_response();
        }
    };

    // Secrets first, then the workflow file; roll the token back on failure.
    let link_result = async {
        client.put_secret(&owner, &repo, workflow::SERVER_SECRET, &dashboard_url(&state)).await?;
        client.put_secret(&owner, &repo, &workflow::token_secret(&project_id), &token_plaintext).await?;
        // Skip the commit when the rendered workflow is unchanged — GitHub's
        // contents API would happily commit identical content again.
        let rendered = spec.render();
        if client.file_content(&owner, &repo, &workflow::workflow_path(&project_id), &branch).await?.as_deref()
            == Some(rendered.as_str())
        {
            return Ok(());
        }
        let commit_message = body
            .commit_message
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .unwrap_or("LiteBin: add deploy workflow")
            .chars()
            .take(100)
            .collect::<String>();
        let sha = client.file_sha(&owner, &repo, &workflow::workflow_path(&project_id), &branch).await?;
        client
            .put_file(&owner, &repo, &workflow::workflow_path(&project_id), &branch, &commit_message, &rendered, sha)
            .await?;
        Ok::<(), GitHubApiError>(())
    }
    .await;

    if let Err(e) = link_result {
        tracing::warn!(project = %project_id, repo = %body.repo_full_name, error = %e, "github link failed: secrets/workflow write");
        let _ = sqlx::query("DELETE FROM deploy_tokens WHERE id = ?").bind(&token_id).execute(&state.db).await;
        return gh_error_degraded(&state, body.connection_id, e).await.into_response();
    }

    let now = chrono::Utc::now().timestamp();
    let inputs_json = serde_json::to_string(&inputs).unwrap_or_default();

    // A previous link's token (if any) is superseded — capture it before the upsert.
    let old_token: Option<Option<String>> =
        sqlx::query_scalar("SELECT deploy_token_id FROM project_github_config WHERE project_id = ?")
            .bind(&project_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();

    if let Err(e) = sqlx::query(
        "INSERT INTO project_github_config
            (project_id, repo_full_name, repo_id, branch, workflow_path, deploy_kind, compose_file,
             workflow_inputs, auto_deploy, deploy_token_id, installation_id, connection_id, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(project_id) DO UPDATE SET
            repo_full_name = excluded.repo_full_name, repo_id = excluded.repo_id,
            branch = excluded.branch, deploy_kind = excluded.deploy_kind,
            compose_file = excluded.compose_file, workflow_inputs = excluded.workflow_inputs,
            auto_deploy = excluded.auto_deploy, deploy_token_id = excluded.deploy_token_id,
            updated_at = excluded.updated_at",
    )
    .bind(&project_id)
    .bind(format!("{owner}/{repo}"))
    .bind(repo_id)
    .bind(&branch)
    .bind(workflow::workflow_path(&project_id))
    .bind(if deploy_kind == DeployKind::Compose { "compose" } else { "single" })
    .bind(&body.compose_file)
    .bind(&inputs_json)
    .bind(body.auto_deploy.unwrap_or(false) as i64)
    .bind(&token_id)
    .bind(body.installation_id)
    .bind(body.connection_id)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await
    {
        let _ = sqlx::query("DELETE FROM deploy_tokens WHERE id = ?").bind(&token_id).execute(&state.db).await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("failed to save link: {e}")})),
        )
            .into_response();
    }

    // Drop the superseded token.
    if let Some(Some(old)) = old_token
        && old != token_id
    {
        let _ = sqlx::query("DELETE FROM deploy_tokens WHERE id = ?").bind(&old).execute(&state.db).await;
    }

    tracing::info!(project = %project_id, repo = %format!("{owner}/{repo}"), branch = %branch, "GitHub link created");
    (
        StatusCode::OK,
        Json(ProjectGithubConfigResponse {
            workflow_path: workflow::workflow_path(&project_id),
            project_id,
            repo_full_name: format!("{owner}/{repo}"),
            branch,
            deploy_kind: if deploy_kind == DeployKind::Compose { "compose" } else { "single" }.to_string(),
            compose_file: body.compose_file,
            auto_deploy: body.auto_deploy.unwrap_or(false),
            inputs,
        }),
    )
        .into_response()
}

#[utoipa::path(
    patch,
    path = "/projects/{id}/github/config",
    description = "Update selected fields of a project's GitHub link.",
    params(("id" = String, Path, description = "Project ID")),
    request_body = PatchConfigRequest,
    responses(
        (status = 200, description = "Toggled — workflow rewritten", body = ProjectGithubConfigResponse),
        (status = 404, description = "No link for this project"),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn patch_config(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Json(body): Json<PatchConfigRequest>,
) -> impl IntoResponse {
    let row = match load_link(&state, &project_id).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let (owner, repo) = match split_repo(&row.repo_full_name) {
        Ok(v) => v,
        Err(e) => return (e.0, Json(serde_json::json!({"error": e.1}))).into_response(),
    };
    let connection_for_error = row.connection_id;
    let client = match github::client_for_link(&state.db, row.connection_id, row.installation_id).await {
        Ok(c) => c,
        Err(e) => return gh_error(e).into_response(),
    };
    let inputs: WorkflowInputs =
        row.workflow_inputs.as_deref().and_then(|s| serde_json::from_str(s).ok()).unwrap_or_default();

    if let Err(e) = rewrite_workflow(&client, &owner, &repo, &row, &inputs, body.auto_deploy).await {
        return gh_error_degraded(&state, connection_for_error, e).await.into_response();
    }

    let now = chrono::Utc::now().timestamp();
    if let Err(e) = sqlx::query("UPDATE project_github_config SET auto_deploy = ?, updated_at = ? WHERE project_id = ?")
        .bind(body.auto_deploy as i64)
        .bind(now)
        .bind(&project_id)
        .execute(&state.db)
        .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": format!("database error: {e}")})))
            .into_response();
    }

    let resp = row_to_response(LinkRow { auto_deploy: body.auto_deploy as i64, ..row });
    (StatusCode::OK, Json(resp)).into_response()
}

async fn rewrite_workflow(
    client: &github::GitHubClient,
    owner: &str,
    repo: &str,
    row: &LinkRow,
    inputs: &WorkflowInputs,
    auto_deploy: bool,
) -> Result<(), GitHubApiError> {
    let spec = spec_from(row, inputs, auto_deploy);
    let rendered = spec.render();
    // Skip no-op commits on auto-deploy toggles back to the same state.
    if client.file_content(owner, repo, &row.workflow_path, &row.branch).await?.as_deref() == Some(rendered.as_str()) {
        return Ok(());
    }
    let sha = client.file_sha(owner, repo, &row.workflow_path, &row.branch).await?;
    client
        .put_file(
            owner,
            repo,
            &row.workflow_path,
            &row.branch,
            if auto_deploy { "LiteBin: enable auto-deploy on push" } else { "LiteBin: disable auto-deploy" },
            &spec.render(),
            sha,
        )
        .await
}

#[utoipa::path(
    delete,
    path = "/projects/{id}/github/config",
    description = "Unlink a project from GitHub: revoke its deploy token and optionally clean the workflow file and secrets from the repo.",
    params(
        ("id" = String, Path, description = "Project ID"),
        ("remove_files" = Option<bool>, Query, description = "Also remove the workflow file + secrets from the repo (default true)"),
    ),
    responses(
        (status = 204, description = "Unlinked — token revoked, repo cleaned up best-effort"),
        (status = 404, description = "No link for this project"),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn delete_config(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Query(params): Query<UnlinkQuery>,
) -> impl IntoResponse {
    let row = match load_link(&state, &project_id).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let (owner, repo) = match split_repo(&row.repo_full_name) {
        Ok(v) => v,
        Err(e) => return (e.0, Json(serde_json::json!({"error": e.1}))).into_response(),
    };

    // Best-effort repo cleanup; 404 is success.
    if params.remove_files.unwrap_or(true)
        && let Ok(client) = github::client_for_link(&state.db, row.connection_id, row.installation_id).await
    {
        let _ = client.delete_secret(&owner, &repo, &workflow::token_secret(&project_id)).await;
        // L8B_SERVER is shared by every project linked to this repo — drop it
        // only when this was the last one.
        let other_links: Option<(i64,)> =
            sqlx::query_as("SELECT COUNT(*) FROM project_github_config WHERE repo_full_name = ? AND project_id != ?")
                .bind(&row.repo_full_name)
                .bind(&project_id)
                .fetch_optional(&state.db)
                .await
                .ok()
                .flatten();
        if other_links.map(|(n,)| n).unwrap_or(0) == 0 {
            let _ = client.delete_secret(&owner, &repo, workflow::SERVER_SECRET).await;
        }
        let _ = client.delete_file(&owner, &repo, &row.workflow_path, &row.branch).await;
    }

    if let Err(e) =
        sqlx::query("DELETE FROM project_github_config WHERE project_id = ?").bind(&project_id).execute(&state.db).await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": format!("database error: {e}")})))
            .into_response();
    }
    if let Some(token_id) = &row.deploy_token_id {
        let _ = sqlx::query("DELETE FROM deploy_tokens WHERE id = ?").bind(token_id).execute(&state.db).await;
    }

    tracing::info!(project = %project_id, repo = %row.repo_full_name, "GitHub link removed");
    (StatusCode::NO_CONTENT, "").into_response()
}

#[utoipa::path(
    post,
    path = "/projects/{id}/github/rotate-token",
    description = "Rotate the project's GitHub deploy token; the repo secret is overwritten before the old token is revoked.",
    params(("id" = String, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Token rotated — repo secret overwritten first"),
        (status = 404, description = "No link for this project"),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn rotate_token(State(state): State<AppState>, Path(project_id): Path<String>) -> impl IntoResponse {
    let row = match load_link(&state, &project_id).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let user_id = match load_project(&state, &project_id).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    let (owner, repo) = match split_repo(&row.repo_full_name) {
        Ok(v) => v,
        Err(e) => return (e.0, Json(serde_json::json!({"error": e.1}))).into_response(),
    };
    let connection_for_error = row.connection_id;
    let client = match github::client_for_link(&state.db, row.connection_id, row.installation_id).await {
        Ok(c) => c,
        Err(e) => return gh_error(e).into_response(),
    };

    let (token_id, token_plaintext) =
        match mint_project_token(&state.db, &user_id, &project_id, &format!("github:{owner}/{repo}")).await {
            Ok(v) => v,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("failed to create deploy token: {e}")})),
                )
                    .into_response();
            }
        };

    // Secret first so in-flight runs keep auth.
    if let Err(e) = client.put_secret(&owner, &repo, &workflow::token_secret(&project_id), &token_plaintext).await {
        let _ = sqlx::query("DELETE FROM deploy_tokens WHERE id = ?").bind(&token_id).execute(&state.db).await;
        return gh_error_degraded(&state, connection_for_error, e).await.into_response();
    }

    let now = chrono::Utc::now().timestamp();
    if let Err(e) =
        sqlx::query("UPDATE project_github_config SET deploy_token_id = ?, updated_at = ? WHERE project_id = ?")
            .bind(&token_id)
            .bind(now)
            .bind(&project_id)
            .execute(&state.db)
            .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": format!("database error: {e}")})))
            .into_response();
    }
    if let Some(old) = &row.deploy_token_id
        && old != &token_id
    {
        let _ = sqlx::query("DELETE FROM deploy_tokens WHERE id = ?").bind(old).execute(&state.db).await;
    }

    tracing::info!(project = %project_id, "GitHub link token rotated");
    (StatusCode::OK, Json(serde_json::json!({"message": "token rotated"}))).into_response()
}

#[utoipa::path(
    post,
    path = "/projects/{id}/github/deploy",
    description = "Dispatch the linked repo's deploy workflow now (build and push image, then deploy).",
    params(("id" = String, Path, description = "Project ID")),
    request_body = DeployRequest,
    responses(
        (status = 202, description = "Workflow dispatched — watch the runs endpoint"),
        (status = 404, description = "No link for this project"),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn deploy_now(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    body: Option<Json<DeployRequest>>,
) -> impl IntoResponse {
    let row = match load_link(&state, &project_id).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let (owner, repo) = match split_repo(&row.repo_full_name) {
        Ok(v) => v,
        Err(e) => return (e.0, Json(serde_json::json!({"error": e.1}))).into_response(),
    };
    let connection_for_error = row.connection_id;
    let client = match github::client_for_link(&state.db, row.connection_id, row.installation_id).await {
        Ok(c) => c,
        Err(e) => return gh_error(e).into_response(),
    };

    let git_ref = body.and_then(|Json(b)| b.git_ref).filter(|r| !r.is_empty()).unwrap_or_else(|| row.branch.clone());

    if let Err(e) = client.dispatch_workflow(&owner, &repo, &workflow::workflow_file(&project_id), &git_ref).await {
        return gh_error_degraded(&state, connection_for_error, e).await.into_response();
    }

    tracing::info!(project = %project_id, repo = %row.repo_full_name, r#ref = %git_ref, "GitHub deploy dispatched");
    (StatusCode::ACCEPTED, Json(serde_json::json!({"message": "deploy dispatched"}))).into_response()
}

//! Repo browsing endpoints.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;

use crate::AppState;
use crate::github;
use crate::github::repos::{BranchSummary, RepoDetail, RepoPage};

use super::connect::gh_error;

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RepoListQuery {
    pub page: Option<u32>,
    /// App mode: list this installation's repos.
    pub installation_id: Option<i64>,
    /// PAT mode: list this connection's repos (user repos the token sees).
    pub connection_id: Option<i64>,
}

#[utoipa::path(
    get,
    path = "/github/repos",
    description = "Browse repositories visible to the active GitHub connection (paginated, searchable).",
    params(
        ("page" = Option<u32>, Query, description = "1-based page (100 per page, pushed-desc)"),
    ),
    responses(
        (status = 200, description = "Repos visible to the connected GitHub credential", body = RepoPage),
        (status = 401, description = "GitHub token expired — reconnect"),
        (status = 503, description = "GitHub not connected"),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn list_repos(State(state): State<AppState>, Query(params): Query<RepoListQuery>) -> impl IntoResponse {
    let page = params.page.unwrap_or(1).max(1);
    let client = match github::client_for_link(&state.db, params.connection_id, params.installation_id).await {
        Ok(c) => c,
        Err(e) => return gh_error(e).into_response(),
    };
    // Installations return the envelope endpoint; PATs the bare array one.
    let result = if params.installation_id.is_some() {
        client.list_installation_repos(page).await
    } else {
        client.list_repos(page).await
    };
    match result {
        Ok(page) => (StatusCode::OK, Json(page)).into_response(),
        Err(e) => gh_error(e).into_response(),
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RepoDetailQuery {
    /// Subfolder to scan for compose/Dockerfile detection (repo root when empty).
    pub path: Option<String>,
    /// App mode: scope to this installation (the one the repo was browsed under).
    pub installation_id: Option<i64>,
    /// PAT mode: scope to this connection.
    pub connection_id: Option<i64>,
}

#[utoipa::path(
    get,
    path = "/github/repos/{owner}/{repo}",
    description = "Repository details: default branch, private flag, visibility, and update time.",
    params(
        ("owner" = String, Path, description = "Repo owner (user or org)"),
        ("repo" = String, Path, description = "Repo name"),
        ("path" = Option<String>, Query, description = "Subfolder for detection (root when omitted)"),
    ),
    responses(
        (status = 200, description = "Repo detail with compose/Dockerfile detection at the given folder", body = RepoDetail),
        (status = 404, description = "Repo not found"),
        (status = 503, description = "GitHub not connected"),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn repo_detail(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    Query(params): Query<RepoDetailQuery>,
) -> impl IntoResponse {
    let client = match github::client_for_link(&state.db, params.connection_id, params.installation_id).await {
        Ok(c) => c,
        Err(e) => return gh_error(e).into_response(),
    };
    let path = params.path.as_deref().filter(|p| !p.trim().is_empty()).map(str::to_string);
    match client.repo_detail(&owner, &repo, path.as_deref()).await {
        Ok(detail) => (StatusCode::OK, Json(detail)).into_response(),
        Err(e) => gh_error(e).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/github/repos/{owner}/{repo}/branches",
    description = "List branches for a repository.",
    params(
        ("owner" = String, Path, description = "Repo owner (user or org)"),
        ("repo" = String, Path, description = "Repo name"),
    ),
    responses(
        (status = 200, description = "Branch list", body = [BranchSummary]),
        (status = 404, description = "Repo not found"),
        (status = 503, description = "GitHub not connected"),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn list_branches(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    Query(params): Query<RepoDetailQuery>,
) -> impl IntoResponse {
    let client = match github::client_for_link(&state.db, params.connection_id, params.installation_id).await {
        Ok(c) => c,
        Err(e) => return gh_error(e).into_response(),
    };
    match client.list_branches(&owner, &repo).await {
        Ok(branches) => (StatusCode::OK, Json(branches)).into_response(),
        Err(e) => gh_error(e).into_response(),
    }
}

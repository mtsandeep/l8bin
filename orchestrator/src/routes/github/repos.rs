//! Repo browsing endpoints (PAT mode in M1; installation repos join in M2).

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

#[utoipa::path(
    get,
    path = "/github/repos/{owner}/{repo}",
    params(
        ("owner" = String, Path, description = "Repo owner (user or org)"),
        ("repo" = String, Path, description = "Repo name"),
    ),
    responses(
        (status = 200, description = "Repo detail with compose/Dockerfile detection", body = RepoDetail),
        (status = 404, description = "Repo not found"),
        (status = 503, description = "GitHub not connected"),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn repo_detail(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
) -> impl IntoResponse {
    // Any usable connection can read public repo metadata; prefer an
    // installation if one exists (private repos need it).
    let installation: Option<i64> =
        sqlx::query_scalar("SELECT id FROM github_installations WHERE status = 'active' LIMIT 1")
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    let client = match github::client_for_link(&state.db, None, installation).await {
        Ok(c) => c,
        Err(e) => return gh_error(e).into_response(),
    };
    match client.repo_detail(&owner, &repo).await {
        Ok(detail) => (StatusCode::OK, Json(detail)).into_response(),
        Err(e) => gh_error(e).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/github/repos/{owner}/{repo}/branches",
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
) -> impl IntoResponse {
    let installation: Option<i64> =
        sqlx::query_scalar("SELECT id FROM github_installations WHERE status = 'active' LIMIT 1")
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    let client = match github::client_for_link(&state.db, None, installation).await {
        Ok(c) => c,
        Err(e) => return gh_error(e).into_response(),
    };
    match client.list_branches(&owner, &repo).await {
        Ok(branches) => (StatusCode::OK, Json(branches)).into_response(),
        Err(e) => gh_error(e).into_response(),
    }
}

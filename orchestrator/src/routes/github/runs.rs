//! Deploy status: per-project run history + bulk badge snapshot.

use std::collections::HashMap;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Serialize;

use crate::AppState;
use crate::github::actions::WorkflowRun;
use crate::github::workflow;
use crate::github::{self, GitHubApiError};

use super::connect::gh_error;
use super::split_repo;

#[derive(Serialize, utoipa::ToSchema)]
pub struct ProjectRuns {
    pub project_id: String,
    pub repo_full_name: String,
    pub runs: Vec<WorkflowRun>,
}

/// Parse an ISO-8601 GitHub timestamp into unix seconds (best-effort).
fn gh_time_to_unix(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts).ok().map(|t| t.timestamp())
}

/// Refresh the denormalized last-run columns.
async fn persist_last_run(state: &AppState, project_id: &str, latest: Option<&WorkflowRun>) {
    let Some(run) = latest else { return };
    let now = chrono::Utc::now().timestamp();
    let _ = sqlx::query(
        "UPDATE project_github_config SET last_run_id = ?, last_run_status = ?, last_run_conclusion = ?, last_run_url = ?, last_run_at = ?, updated_at = ? WHERE project_id = ?",
    )
    .bind(run.id)
    .bind(&run.status)
    .bind(&run.conclusion)
    .bind(&run.html_url)
    .bind(run.created_at.as_deref().and_then(gh_time_to_unix))
    .bind(now)
    .bind(project_id)
    .execute(&state.db)
    .await;
}

#[utoipa::path(
    get,
    path = "/projects/{id}/github/runs",
    description = "Recent GitHub Actions workflow runs for the project's linked repo.",
    params(("id" = String, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Recent runs of the linked workflow, newest first", body = ProjectRuns),
        (status = 404, description = "No link for this project"),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn project_runs(State(state): State<AppState>, Path(project_id): Path<String>) -> impl IntoResponse {
    let link: Option<(String, String, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT repo_full_name, branch, installation_id, connection_id FROM project_github_config WHERE project_id = ?",
    )
    .bind(&project_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    let Some((repo_full_name, branch, installation_id, connection_id)) = link else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("project '{project_id}' has no GitHub link")})),
        )
            .into_response();
    };
    let (owner, repo) = match split_repo(&repo_full_name) {
        Ok(v) => v,
        Err(e) => return (e.0, Json(serde_json::json!({"error": e.1}))).into_response(),
    };
    let client = match github::client_for_link(&state.db, connection_id, installation_id).await {
        Ok(c) => c,
        Err(e) => return gh_error(e).into_response(),
    };

    match client.list_workflow_runs(&owner, &repo, &workflow::workflow_file(&project_id), Some(&branch), 10).await {
        Ok(runs) => {
            persist_last_run(&state, &project_id, runs.first()).await;
            (StatusCode::OK, Json(ProjectRuns { project_id, repo_full_name, runs })).into_response()
        }
        Err(e) => {
            if matches!(e, GitHubApiError::AuthExpired) {
                super::connect::mark_degraded(&state.db, connection_id, "GitHub rejected the stored credential").await;
            }
            gh_error(e).into_response()
        }
    }
}

#[derive(Clone, Serialize, utoipa::ToSchema)]
pub struct BulkRunStatus {
    pub project_id: String,
    pub repo_full_name: String,
    pub status: Option<String>,
    pub conclusion: Option<String>,
    pub run_url: Option<String>,
    pub run_at: Option<i64>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct BulkStatus {
    /// Latest run per linked project (cached snapshot when GitHub was unreachable).
    pub projects: Vec<BulkRunStatus>,
}

/// Response cache: badge polls fan out to GitHub per linked project.
static BULK_CACHE: std::sync::OnceLock<std::sync::Mutex<(std::time::Instant, Vec<BulkRunStatus>)>> =
    std::sync::OnceLock::new();
const BULK_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(20);

#[utoipa::path(
    get,
    path = "/github/status",
    description = "Latest workflow run status for every GitHub-linked project (badge polling).",
    responses(
        (status = 200, description = "Latest run per linked project (20s server cache)", body = BulkStatus),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn bulk_status(State(state): State<AppState>) -> impl IntoResponse {
    let cache =
        BULK_CACHE.get_or_init(|| std::sync::Mutex::new((std::time::Instant::now() - BULK_CACHE_TTL, Vec::new())));
    if let Ok(guard) = cache.lock()
        && guard.0.elapsed() < BULK_CACHE_TTL
    {
        return (StatusCode::OK, Json(BulkStatus { projects: guard.1.clone() })).into_response();
    }

    type LinkRow = (String, String, String, Option<i64>, Option<i64>);
    let links: Vec<LinkRow> =
        sqlx::query_as("SELECT project_id, repo_full_name, branch, installation_id, connection_id FROM project_github_config ORDER BY updated_at DESC")
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();

    // Denormalized snapshots fill in when GitHub is unreachable or a repo is gone.
    type RunSnapshot = (Option<String>, Option<String>, Option<String>, Option<i64>);
    let snapshots: HashMap<String, RunSnapshot> = sqlx::query_as::<
        _,
        (String, Option<String>, Option<String>, Option<String>, Option<i64>),
    >(
        "SELECT project_id, last_run_status, last_run_conclusion, last_run_url, last_run_at FROM project_github_config",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(pid, s, c, u, at)| (pid, (s, c, u, at)))
    .collect();

    let mut out = Vec::with_capacity(links.len());

    for (project_id, repo_full_name, branch, installation_id, connection_id) in links {
        let mut entry = BulkRunStatus {
            status: None,
            conclusion: None,
            run_url: None,
            run_at: None,
            project_id: project_id.clone(),
            repo_full_name: repo_full_name.clone(),
        };
        let client = github::client_for_link(&state.db, connection_id, installation_id).await.ok();
        if let (Some(client), Ok((owner, repo))) = (&client, split_repo(&repo_full_name))
            && let Ok(runs) =
                client.list_workflow_runs(&owner, &repo, &workflow::workflow_file(&project_id), Some(&branch), 1).await
            && let Some(run) = runs.first()
        {
            persist_last_run(&state, &project_id, Some(run)).await;
            entry.status = run.status.clone();
            entry.conclusion = run.conclusion.clone();
            entry.run_url = Some(run.html_url.clone());
            entry.run_at = run.created_at.as_deref().and_then(gh_time_to_unix);
            out.push(entry);
            continue;
        }
        if let Some((s, c, u, at)) = snapshots.get(&project_id) {
            entry.status = s.clone();
            entry.conclusion = c.clone();
            entry.run_url = u.clone();
            entry.run_at = *at;
        }
        out.push(entry);
    }

    if let Ok(mut guard) = cache.lock() {
        *guard = (std::time::Instant::now(), out.clone());
    }
    (StatusCode::OK, Json(BulkStatus { projects: out })).into_response()
}

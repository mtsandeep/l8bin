use axum::{extract::Query, extract::State, http::StatusCode};
use serde::Deserialize;

use crate::AppState;

#[derive(Deserialize, utoipa::ToSchema)]
pub struct AskQuery {
    pub domain: String,
}

#[utoipa::path(
    get,
    path = "/caddy/ask",
    params(
        ("domain" = String, Query, description = "Domain to check"),
    ),
    responses(
        (status = 200, description = "Domain approved"),
        (status = 404, description = "Domain not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "health",
)]
/// Caddy On-Demand TLS validation endpoint.
/// Returns 200 if the domain belongs to a known project, 404 otherwise.
/// Checks: 1) custom_domain match, 2) subdomain/alias/dashboard/poke match
/// under the current, staged, or previous platform domain.
pub async fn ask(State(state): State<AppState>, Query(query): Query<AskQuery>) -> StatusCode {
    let asked = &query.domain;

    if custom_domain_matches(&state, asked).await {
        return StatusCode::OK;
    }

    let mut candidates = vec![state.platform.domain()];
    if let Some(prev) = crate::platform::previous_domain(&state.db).await {
        candidates.push(prev);
    }
    if let Some(staged) = crate::platform::staged_domain(&state.db).await {
        candidates.push(staged);
    }
    for platform_domain in &candidates {
        match approved_under_domain(&state, asked, platform_domain).await {
            Ok(true) => return StatusCode::OK,
            Ok(false) => {}
            Err(e) => {
                tracing::error!(domain = %asked, error = %e, "caddy ask: DB error");
                return StatusCode::INTERNAL_SERVER_ERROR;
            }
        }
    }

    tracing::debug!(domain = %asked, "caddy ask: denied");
    StatusCode::NOT_FOUND
}

/// Custom domains and their www variants — platform-domain independent.
async fn custom_domain_matches(state: &AppState, asked: &str) -> bool {
    let direct: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM projects WHERE is_background = 0 AND custom_domain = ?")
        .bind(asked)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
    if direct > 0 {
        return true;
    }
    if let Some(bare) = asked.strip_prefix("www.") {
        let www: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM projects WHERE is_background = 0 AND custom_domain = ?")
                .bind(bare)
                .fetch_one(&state.db)
                .await
                .unwrap_or(0);
        if www > 0 {
            return true;
        }
    }
    false
}

/// Subdomain, alias, dashboard, and poke checks under one platform domain.
async fn approved_under_domain(state: &AppState, asked: &str, platform_domain: &str) -> anyhow::Result<bool> {
    let db = &state.db;

    // Project subdomain: {project_id}.{domain}
    if let Some(project_id) = asked.strip_suffix(&format!(".{platform_domain}")) {
        let exists: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM projects WHERE id = ?").bind(project_id).fetch_one(db).await?;
        if exists > 0 {
            return Ok(true);
        }

        // Aliases: {alias}.{project_id}.{domain} and {alias}.{domain}
        if let Some((alias, project_id)) = project_id.rsplit_once('.') {
            let exists: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM project_routes r JOIN projects p ON p.id = r.project_id \
                 WHERE p.is_background = 0 AND r.project_id = ? AND r.route_type = 'alias' AND r.subdomain = ?",
            )
            .bind(project_id)
            .bind(alias)
            .fetch_one(db)
            .await?;
            if exists > 0 {
                return Ok(true);
            }
        }
        let exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM project_routes r JOIN projects p ON p.id = r.project_id \
             WHERE p.is_background = 0 AND r.route_type = 'alias' AND r.subdomain = ?",
        )
        .bind(project_id)
        .fetch_one(db)
        .await?;
        if exists > 0 {
            return Ok(true);
        }
    }

    if asked == platform_domain || asked == format!("{}.{}", state.platform.dashboard_subdomain(), platform_domain) {
        return Ok(true);
    }
    if asked == format!("{}.{}", state.platform.poke_subdomain(), platform_domain) {
        return Ok(true);
    }
    Ok(false)
}

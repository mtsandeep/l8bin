use axum::{extract::Query, extract::State, http::StatusCode};
use serde::Deserialize;

use crate::AppState;

#[derive(Deserialize)]
pub struct AskQuery {
    pub domain: String,
}

/// Caddy On-Demand TLS validation endpoint.
/// Returns 200 if the domain belongs to a known project, 404 otherwise.
/// Checks: 1) subdomain match (project_id), 2) custom_domain match, 3) www variant of custom_domain.
pub async fn ask(
    State(state): State<AppState>,
    Query(query): Query<AskQuery>,
) -> StatusCode {
    let domain = &query.domain;

    // 1. Subdomain match: strip ".{domain}" suffix to get project_id
    let subdomain = domain
        .strip_suffix(&format!(".{}", state.config.domain))
        .unwrap_or(domain);

    let subdomain_exists =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM projects WHERE id = ?")
            .bind(subdomain)
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);

    if subdomain_exists > 0 {
        tracing::debug!(domain = %domain, subdomain = %subdomain, "caddy ask: approved (subdomain)");
        return StatusCode::OK;
    }

    // 2. Exact custom_domain match
    let custom_exists =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM projects WHERE custom_domain = ?")
            .bind(domain)
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);

    if custom_exists > 0 {
        tracing::debug!(domain = %domain, "caddy ask: approved (custom domain)");
        return StatusCode::OK;
    }

    // 3. www variant: if queried domain starts with "www.", check bare domain too
    if let Some(bare) = domain.strip_prefix("www.") {
        let www_exists =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM projects WHERE custom_domain = ?")
                .bind(bare)
                .fetch_one(&state.db)
                .await
                .unwrap_or(0);

        if www_exists > 0 {
            tracing::debug!(domain = %domain, bare = %bare, "caddy ask: approved (www variant)");
            return StatusCode::OK;
        }
    }

    // 4. Dashboard subdomain approval
    let dashboard_host = format!("{}.{}", state.config.dashboard_subdomain, state.config.domain);
    if domain == &dashboard_host {
        tracing::debug!(domain = %domain, "caddy ask: approved (dashboard subdomain)");
        return StatusCode::OK;
    }

    // 5. Poke subdomain approval
    let poke_host = format!("{}.{}", state.config.poke_subdomain, state.config.domain);
    if domain == &poke_host {
        tracing::debug!(domain = %domain, "caddy ask: approved (poke subdomain)");
        return StatusCode::OK;
    }

    tracing::debug!(domain = %domain, "caddy ask: denied");
    StatusCode::NOT_FOUND
}

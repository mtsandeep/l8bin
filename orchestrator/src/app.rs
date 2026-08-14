use axum::{
    Router,
    routing::{delete, get, patch, post, put},
};
use axum_login::login_required;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::AppState;
use crate::auth;

/// Assemble the full orchestrator application router (all route groups + middleware).
pub(crate) fn build_app(state: AppState) -> Router {
    // Routes - Auth public (no login required)
    let auth_public = Router::new()
        .route("/auth/login", post(crate::routes::auth::login))
        .route("/auth/register", post(crate::routes::auth::register))
        .route("/auth/setup", get(crate::routes::auth::setup_check));

    // Routes - Auth protected (login required)
    let auth_protected = Router::new()
        .route("/auth/logout", post(crate::routes::auth::logout))
        .route("/auth/me", get(crate::routes::auth::me))
        .route("/auth/change-password", post(crate::routes::auth::change_password))
        .route("/status", get(crate::routes::auth::status))
        .route_layer(login_required!(auth::backend::PasswordBackend, login_url = "/auth/login"));

    // Routes - Protected API (session auth only)
    let api_routes = Router::new()
        .route("/projects", post(crate::routes::projects::create_project))
        .route("/projects", get(crate::routes::projects::list_projects))
        .route("/projects/stats", get(crate::routes::stats::all_project_stats))
        .route("/projects/{id}", get(crate::routes::projects::get_project))
        .route("/projects/{id}/settings", patch(crate::routes::settings::update_project_settings))
        .route("/projects/{id}/stop", post(crate::routes::manage::handlers::stop_project))
        .route("/projects/{id}/start", post(crate::routes::manage::handlers::start_project))
        .route("/projects/{id}", delete(crate::routes::manage::handlers::delete_project))
        .route("/projects/{id}/stats", get(crate::routes::stats::project_stats))
        .route("/projects/{id}/disk-usage", get(crate::routes::stats::project_disk_usage))
        .route("/projects/{id}/logs", get(crate::routes::stats::project_logs))
        .route("/projects/{id}/deploy-logs", get(crate::routes::stats::deploy_logs))
        .route("/projects/{id}/recreate", post(crate::routes::manage::handlers::recreate_project))
        .route("/projects/{id}/services/{name}/start", post(crate::routes::manage::handlers::start_service))
        .route("/projects/{id}/services/{name}/stop", post(crate::routes::manage::handlers::stop_service))
        .route("/projects/{id}/services/{name}/restart", post(crate::routes::manage::handlers::restart_service))
        .route("/projects/{id}/services/{name}/settings", patch(crate::routes::settings::update_service_settings))
        .route("/projects/{id}/volumes/{name}", delete(crate::routes::volumes::delete_volume))
        .route("/projects/{id}/volumes", delete(crate::routes::volumes::delete_all_volumes))
        .route("/projects/{id}/routes", get(crate::routes::projects::list_routes))
        .route("/projects/{id}/routes", post(crate::routes::projects::create_route))
        .route("/projects/{id}/routes/{route_id}", delete(crate::routes::projects::delete_route))
        .route("/projects/{id}/capabilities", get(crate::routes::capabilities::list_project_capabilities))
        .route("/projects/{id}/capabilities", post(crate::routes::capabilities::grant_project_capabilities))
        .route(
            "/projects/{id}/capabilities/{capability}",
            delete(crate::routes::capabilities::revoke_project_capability),
        )
        .route("/nodes", get(crate::routes::nodes::list_nodes))
        .route("/nodes", post(crate::routes::nodes::create_node))
        .route("/nodes/{id}", delete(crate::routes::nodes::delete_node))
        .route("/nodes/{id}/connect", post(crate::routes::nodes::connect_node))
        .route("/nodes/image-stats", get(crate::routes::nodes::node_image_stats))
        .route("/nodes/{id}/images/prune", post(crate::routes::nodes::prune_node_images))
        .route("/settings", get(crate::routes::global_settings::get_settings))
        .route("/settings", patch(crate::routes::global_settings::update_settings))
        .route("/settings/cleanup-dns", post(crate::routes::global_settings::cleanup_dns))
        .route("/settings/sync-dns", post(crate::routes::global_settings::sync_dns))
        .route("/settings/domain/preflight", post(crate::routes::global_settings::domain_preflight))
        .route("/settings/domain/apply", post(crate::routes::global_settings::domain_apply))
        .route("/settings/domain/jobs/{id}", get(crate::routes::global_settings::domain_job_status))
        .route("/settings/domain/jobs/{id}/retry", post(crate::routes::global_settings::domain_job_retry))
        .route("/system/stats", get(crate::routes::health::system_stats))
        .route("/scan", get(crate::routes::scan::scan_containers))
        .route("/scan/import", post(crate::routes::scan::import_containers))
        .route_layer(login_required!(auth::backend::PasswordBackend, login_url = "/auth/login"));

    // Routes - Deploy + image upload (session OR deploy token auth)
    let deploy_routes = Router::new()
        .route("/deploy", post(crate::routes::deploy::single::deploy_create))
        .route("/deploy", put(crate::routes::deploy::single::deploy_update))
        .route("/deploy/compose", post(crate::routes::deploy::compose::deploy_compose))
        .route("/compose/validate", post(crate::routes::capabilities::validate_compose))
        .route("/images/upload", post(crate::routes::images::upload_image))
        // Chunked resumable upload (local + relay). Direct uploads are minted by
        // the agent; this broker returns the agent URL for them.
        .route("/images/upload-target", post(crate::routes::images::upload_target))
        .route(&litebin_common::upload::master_status_route(), get(crate::routes::images::chunk_status))
        .route(&litebin_common::upload::master_chunk_route(), post(crate::routes::images::chunk_upload))
        .route(&litebin_common::upload::master_commit_route(), post(crate::routes::images::chunk_commit))
        // Chunk bodies can be up to ~the chunk size; raise axum's default 2 MiB limit.
        .layer(axum::extract::DefaultBodyLimit::max(litebin_common::upload::MAX_UPLOAD_BODY));

    // Routes - Deploy token management (session auth)
    let token_routes = Router::new()
        .route("/deploy-tokens", post(crate::routes::deploy_tokens::create_token))
        .route("/deploy-tokens", get(crate::routes::deploy_tokens::list_tokens))
        .route("/deploy-tokens/{id}", delete(crate::routes::deploy_tokens::revoke_token))
        .route_layer(login_required!(auth::backend::PasswordBackend, login_url = "/auth/login"));

    Router::new()
        .merge(auth_public)
        .merge(auth_protected)
        .merge(api_routes)
        .merge(deploy_routes)
        .merge(token_routes)
        .route("/health", get(crate::routes::health::health_check))
        .route("/openapi.json", get(crate::routes::openapi::openapi_json))
        .route("/docs", get(crate::routes::docs::serve_docs))
        .route("/llms.txt", get(crate::routes::openapi::llms_txt))
        .route("/caddy/ask", get(crate::routes::caddy::ask))
        .route("/internal/wake-report", post(crate::routes::wake_report::wake_report))
        .route("/internal/heartbeat", post(crate::routes::heartbeat::heartbeat))
        .fallback(crate::routes::waker::wake)
        .layer(axum::middleware::from_fn_with_state(state.clone(), crate::routes::waker::waker_intercept))
        .layer(auth::auth_layer(state.clone()))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

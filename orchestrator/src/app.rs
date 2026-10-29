use axum::{
    Router,
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post, put},
};
use axum_login::login_required;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::AppState;
use crate::auth;

/// Panic responder: log the cause, return a generic 500 (never leak internals).
fn panic_response(err: Box<dyn std::any::Any + Send + 'static>) -> Response {
    let detail = err
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| err.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload");
    tracing::error!(panic = %detail, "handler panicked");
    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, axum::Json(serde_json::json!({ "error": "internal server error" })))
        .into_response()
}

/// Assemble the full orchestrator application router (all route groups + middleware).
pub(crate) fn build_app(state: AppState) -> Router {
    // Routes - Auth public (no login required)
    let auth_public = Router::new()
        .route("/auth/login", post(crate::routes::auth::login))
        .route("/auth/register", post(crate::routes::auth::register))
        .route("/auth/setup", get(crate::routes::auth::setup_check))
        .route("/auth/device/start", post(crate::routes::device_auth::start_device_flow))
        .route("/auth/device/token", post(crate::routes::device_auth::poll_device_token));

    // Routes - Auth protected (login required)
    let auth_protected = Router::new()
        .route("/auth/logout", post(crate::routes::auth::logout))
        .route("/auth/me", get(crate::routes::auth::me))
        .route("/auth/change-password", post(crate::routes::auth::change_password))
        .route("/auth/device", get(crate::routes::device_auth::lookup_device_request))
        .route("/auth/device/approve", post(crate::routes::device_auth::approve_device_request))
        .route("/status", get(crate::routes::auth::status))
        // POST binds a session user; deploy endpoints stage projects instead.
        .route("/projects", post(crate::routes::projects::create_project))
        .route_layer(login_required!(auth::backend::PasswordBackend, login_url = "/auth/login"));

    // Routes - Read (session OR deploy token with scope >= read)
    let read_routes = Router::new()
        .route("/projects", get(crate::routes::projects::list_projects))
        .route("/projects/stats", get(crate::routes::stats::all_project_stats))
        .route("/projects/{id}", get(crate::routes::projects::get_project))
        .route("/projects/{id}/stats", get(crate::routes::stats::project_stats))
        .route("/projects/{id}/logs", get(crate::routes::stats::project_logs))
        .route("/projects/{id}/deploy-logs", get(crate::routes::stats::deploy_logs))
        .route("/projects/{id}/disk-usage", get(crate::routes::stats::project_disk_usage))
        .route("/projects/{id}/env", get(crate::routes::env::get_project_env))
        .route("/nodes", get(crate::routes::nodes::list_nodes))
        .route("/nodes/image-stats", get(crate::routes::nodes::node_image_stats))
        .route("/meta", get(crate::routes::meta::get_meta))
        .route("/whoami", get(crate::routes::meta::whoami))
        .route("/github/status", get(crate::routes::github::runs::bulk_status))
        .route("/projects/{id}/github/config", get(crate::routes::github::project_link::get_config))
        .route("/projects/{id}/github/runs", get(crate::routes::github::runs::project_runs))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth::guard::require_read));

    // Routes - Manage (session OR deploy token with scope >= manage)
    let manage_routes = Router::new()
        .route("/projects/{id}/env", put(crate::routes::env::update_project_env))
        .route("/projects/{id}/stop", post(crate::routes::manage::handlers::stop_project))
        .route("/projects/{id}/start", post(crate::routes::manage::handlers::start_project))
        .route("/projects/{id}/migrate", post(crate::routes::manage::handlers::migrate_project))
        .route("/projects/{id}/duplicate", post(crate::routes::manage::handlers::duplicate_project))
        .route("/projects/{id}/export", post(crate::routes::manage::handlers::export_project))
        .route("/exports", get(crate::routes::exports::list_exports))
        .route(
            "/exports/{file}",
            get(crate::routes::exports::download_export).delete(crate::routes::exports::delete_export),
        )
        .route("/exports/jobs/{id}", get(crate::routes::exports::export_job_status))
        .route("/imports", get(crate::routes::imports::list_import_archives))
        .route("/imports/{file}", delete(crate::routes::imports::delete_import_archive))
        .route("/imports", post(crate::routes::imports::import_bundle))
        // Streamed raw-body upload of a bundle (large bundles: scp to the folder).
        .route(
            "/imports/upload",
            post(crate::routes::imports::upload_import_archive)
                .layer(axum::extract::DefaultBodyLimit::max(4 * 1024 * 1024 * 1024)),
        )
        .route("/projects/{id}/recreate", post(crate::routes::manage::handlers::recreate_project))
        .route("/projects/{id}/settings", patch(crate::routes::settings::update_project_settings))
        .route("/projects/{id}/services/{name}/start", post(crate::routes::manage::handlers::start_service))
        .route("/projects/{id}/services/{name}/stop", post(crate::routes::manage::handlers::stop_service))
        .route("/projects/{id}/services/{name}/restart", post(crate::routes::manage::handlers::restart_service))
        .route("/projects/{id}/services/{name}/settings", patch(crate::routes::settings::update_service_settings))
        .route("/projects/{id}/routes", get(crate::routes::projects::list_routes))
        .route("/projects/{id}/routes", post(crate::routes::projects::create_route))
        .route("/projects/{id}/routes/{route_id}", delete(crate::routes::projects::delete_route))
        .route("/projects/{id}/capabilities", get(crate::routes::capabilities::list_project_capabilities))
        .route("/projects/{id}/capabilities", post(crate::routes::capabilities::grant_project_capabilities))
        .route(
            "/projects/{id}/capabilities/{capability}",
            delete(crate::routes::capabilities::revoke_project_capability),
        )
        .route("/github/repos", get(crate::routes::github::repos::list_repos))
        .route("/github/repos/{owner}/{repo}", get(crate::routes::github::repos::repo_detail))
        .route("/github/repos/{owner}/{repo}/branches", get(crate::routes::github::repos::list_branches))
        .route("/github/installations", get(crate::routes::github::connect::list_installations))
        .route("/projects/{id}/github/config", put(crate::routes::github::project_link::put_config))
        .route("/projects/{id}/github/config", patch(crate::routes::github::project_link::patch_config))
        .route("/projects/{id}/github/config", delete(crate::routes::github::project_link::delete_config))
        .route("/projects/{id}/github/rotate-token", post(crate::routes::github::project_link::rotate_token))
        .route("/projects/{id}/github/deploy", post(crate::routes::github::project_link::deploy_now))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth::guard::require_manage));

    // Routes - Admin (session OR deploy token with scope = admin)
    // Destructive and platform-level operations.
    let admin_routes = Router::new()
        .route("/projects/{id}", delete(crate::routes::manage::handlers::delete_project))
        .route("/projects/{id}/volumes/{name}", delete(crate::routes::volumes::delete_volume))
        .route("/projects/{id}/volumes", delete(crate::routes::volumes::delete_all_volumes))
        .route("/nodes", post(crate::routes::nodes::create_node))
        .route("/nodes/{id}", delete(crate::routes::nodes::delete_node))
        .route("/nodes/{id}/connect", post(crate::routes::nodes::connect_node))
        .route("/nodes/{id}/images/prune", post(crate::routes::nodes::prune_node_images))
        .route("/settings", get(crate::routes::global_settings::get_settings))
        .route("/settings", patch(crate::routes::global_settings::update_settings))
        .route("/settings/cleanup-dns", post(crate::routes::global_settings::cleanup_dns))
        .route("/settings/sync-dns", post(crate::routes::global_settings::sync_dns))
        .route("/settings/domain/preflight", post(crate::routes::global_settings::domain_preflight))
        .route("/settings/domain/apply", post(crate::routes::global_settings::domain_apply))
        .route("/settings/domain/jobs/{id}", get(crate::routes::global_settings::domain_job_status))
        .route("/settings/domain/jobs/{id}/retry", post(crate::routes::global_settings::domain_job_retry))
        .route("/backup/status", get(crate::routes::backup::backup_status))
        .route("/backup/enable", post(crate::routes::backup::backup_enable))
        .route("/backup/disable", post(crate::routes::backup::backup_disable))
        .route("/backup/s3", post(crate::routes::backup::backup_add_s3))
        .route("/backup/s3", delete(crate::routes::backup::backup_remove_s3))
        .route("/backup/points", post(crate::routes::backup::backup_create_point))
        .route("/backup/points/{id}", delete(crate::routes::backup::backup_delete_point))
        .route("/backup/restore", post(crate::routes::backup::backup_restore))
        .route("/backup/restore-local", post(crate::routes::backup::backup_restore_local))
        .route("/backup/restore-point", post(crate::routes::backup::backup_restore_point))
        .route("/backup/revert-restore", post(crate::routes::backup::backup_revert_restore))
        .route("/system/stats", get(crate::routes::health::system_stats))
        .route("/scan", get(crate::routes::scan::scan_containers))
        .route("/scan/import", post(crate::routes::scan::import_containers))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth::guard::require_admin));

    // Routes - Deploy + image upload (session OR deploy token with scope >= deploy,
    // per-project binding checked in-handler via extract_deploy_token)
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

    // Routes - Deploy token management (session only — tokens must not mint tokens)
    let token_routes = Router::new()
        .route("/deploy-tokens", post(crate::routes::deploy_tokens::create_token))
        .route("/deploy-tokens", get(crate::routes::deploy_tokens::list_tokens))
        .route("/deploy-tokens/{id}", delete(crate::routes::deploy_tokens::revoke_token))
        .route_layer(login_required!(auth::backend::PasswordBackend, login_url = "/auth/login"));

    // Routes - Registry management (session only — instance-level credentials)
    let registry_routes = Router::new()
        .route("/registries", get(crate::routes::registries::list_registries))
        .route("/registries", post(crate::routes::registries::create_registry))
        .route("/registries/{id}", put(crate::routes::registries::update_registry))
        .route("/registries/{id}", delete(crate::routes::registries::delete_registry))
        .route("/registries/{id}/test", post(crate::routes::registries::test_registry))
        .route_layer(login_required!(auth::backend::PasswordBackend, login_url = "/auth/login"));

    // Routes - GitHub connection management (session or admin token —
    // connects instance-level credentials)
    let github_admin_routes = Router::new()
        .route("/github/connection", get(crate::routes::github::connect::connection_status))
        .route("/github/connections", get(crate::routes::github::connect::list_connections))
        .route("/github/connections/{id}", delete(crate::routes::github::connect::disconnect))
        .route("/github/pat", post(crate::routes::github::connect::connect_pat))
        .route("/github/app", post(crate::routes::github::connect::connect_app))
        .route("/github/manifest", post(crate::routes::github::connect::start_manifest))
        .route("/github/manifest/exchange", post(crate::routes::github::connect::exchange_manifest))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth::guard::require_admin));

    let api = Router::new()
        .merge(auth_public)
        .merge(auth_protected)
        .merge(read_routes)
        .merge(manage_routes)
        .merge(admin_routes)
        .merge(deploy_routes)
        .merge(token_routes)
        .merge(registry_routes)
        .merge(github_admin_routes)
        .route("/health", get(crate::routes::health::health_check))
        .route("/github/webhook", post(crate::routes::github::webhook::github_webhook))
        .route("/openapi.json", get(crate::routes::openapi::openapi_json))
        .route("/docs", get(crate::routes::docs::serve_docs))
        .route("/llms.txt", get(crate::routes::openapi::llms_txt))
        .route("/caddy/ask", get(crate::routes::caddy::ask))
        .route("/internal/wake-report", post(crate::routes::wake_report::wake_report))
        .route("/internal/heartbeat", post(crate::routes::heartbeat::heartbeat));

    // /api/* is a stripped alias (/api/auth/login ≡ /auth/login) — clients guess the prefix.
    Router::new()
        .merge(api.clone())
        .nest("/api", api)
        .fallback(crate::routes::waker::wake)
        .layer(axum::middleware::from_fn_with_state(state.clone(), crate::routes::waker::waker_intercept))
        .layer(CatchPanicLayer::custom(panic_response))
        .layer(auth::auth_layer(state.clone()))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

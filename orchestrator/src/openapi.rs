use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "LiteBin API",
        version = env!("CARGO_PKG_VERSION"),
        description = "Self-hosted PaaS API for deploying and managing containerized applications.",
    ),
    tags(
        (name = "auth", description = "Authentication & user management"),
        (name = "projects", description = "Project CRUD and routes"),
        (name = "deploy", description = "Single-service and compose deployments"),
        (name = "manage", description = "Project lifecycle (start, stop, delete, recreate)"),
        (name = "stats", description = "Project statistics, logs, and disk usage"),
        (name = "nodes", description = "Node management (multi-server)"),
        (name = "settings", description = "Project and global settings"),
        (name = "deploy-tokens", description = "Deploy token management for CI/CD"),
        (name = "global-settings", description = "Global platform settings and DNS"),
        (name = "volumes", description = "Volume management"),
        (name = "scan", description = "Container scanning and import"),
        (name = "health", description = "Health checks and system stats"),
    ),
    paths(
        // Auth
        crate::routes::auth::login,
        crate::routes::auth::logout,
        crate::routes::auth::register,
        crate::routes::auth::setup_check,
        crate::routes::auth::me,
        crate::routes::auth::change_password,
        crate::routes::auth::status,
        // Projects
        crate::routes::projects::create_project,
        crate::routes::projects::get_project,
        crate::routes::projects::list_projects,
        crate::routes::projects::list_routes,
        crate::routes::projects::create_route,
        crate::routes::projects::delete_route,
        // Deploy
        crate::routes::deploy::single::deploy_create,
        crate::routes::deploy::single::deploy_update,
        crate::routes::deploy::compose::deploy_compose,
        crate::routes::images::upload_image,
        // Manage
        crate::routes::manage::handlers::stop_project,
        crate::routes::manage::handlers::start_project,
        crate::routes::manage::handlers::delete_project,
        crate::routes::manage::handlers::recreate_project,
        crate::routes::manage::handlers::start_service,
        crate::routes::manage::handlers::stop_service,
        crate::routes::manage::handlers::restart_service,
        // Stats
        crate::routes::stats::all_project_stats,
        crate::routes::stats::project_stats,
        crate::routes::stats::project_disk_usage,
        crate::routes::stats::project_logs,
        crate::routes::stats::deploy_logs,
        // Nodes
        crate::routes::nodes::list_nodes,
        crate::routes::nodes::create_node,
        crate::routes::nodes::connect_node,
        crate::routes::nodes::delete_node,
        crate::routes::nodes::node_image_stats,
        crate::routes::nodes::prune_node_images,
        // Settings
        crate::routes::settings::update_project_settings,
        crate::routes::settings::update_service_settings,
        // Deploy tokens
        crate::routes::deploy_tokens::create_token,
        crate::routes::deploy_tokens::list_tokens,
        crate::routes::deploy_tokens::revoke_token,
        // Global settings
        crate::routes::global_settings::get_settings,
        crate::routes::global_settings::update_settings,
        crate::routes::global_settings::cleanup_dns,
        crate::routes::global_settings::sync_dns,
        // Volumes
        crate::routes::volumes::delete_volume,
        crate::routes::volumes::delete_all_volumes,
        // Scan
        crate::routes::scan::scan_containers,
        crate::routes::scan::import_containers,
        // Health
        crate::routes::health::health_check,
        crate::routes::health::system_stats,
    ),
    components(
        schemas(
            // Shared types (litebin-common)
            litebin_common::types::ProjectStatus,
            litebin_common::types::NodeStatus,
            litebin_common::types::RoutingMode,
            litebin_common::types::DeployType,
            litebin_common::types::Node,
            litebin_common::types::Project,
            litebin_common::types::VolumeMount,
            litebin_common::types::ImageStats,
            litebin_common::types::HealthReport,
            litebin_common::types::ContainerStatus,
            litebin_common::types::ProjectService,
            litebin_common::types::ProjectVolume,
            // Auth
            crate::routes::auth::LoginRequest,
            crate::routes::auth::LoginResponse,
            crate::routes::auth::UserResponse,
            crate::routes::auth::RegisterRequest,
            crate::routes::auth::SetupResponse,
            crate::routes::auth::ChangePasswordRequest,
            crate::routes::auth::ChangePasswordResponse,
            crate::routes::auth::StatusNode,
            crate::routes::auth::StatusResponse,
            // Projects
            crate::routes::projects::CreateProjectRequest,
            crate::routes::projects::ProjectResponse,
            crate::routes::projects::CreateRouteRequest,
            crate::routes::projects::ProjectRouteResponse,
            // Stats
            crate::routes::stats::ServiceVolumeInfo,
            crate::routes::stats::ServiceInfo,
            crate::routes::stats::StatsResponse,
            crate::routes::stats::BatchStatsResponse,
            crate::routes::stats::DiskUsageResponse,
            crate::routes::stats::LogsQuery,
            crate::routes::stats::LogsResponse,
            // Nodes
            crate::routes::nodes::CreateNodeRequest,
            crate::routes::nodes::NodeResponse,
            crate::routes::nodes::ErrorResponse,
            crate::routes::nodes::ConflictResponse,
            crate::routes::nodes::NodeImageStatsResponse,
            // Manage
            crate::routes::manage::helpers::MessageResponse,
            crate::routes::manage::handlers::RecreateRequest,
            // Deploy
            crate::routes::deploy::single::DeployRequest,
            crate::routes::deploy::single::DeployResponse,
            // Images
            crate::routes::images::UploadQueryParams,
            crate::routes::images::UploadResponse,
            // Deploy tokens
            crate::routes::deploy_tokens::CreateTokenRequest,
            crate::routes::deploy_tokens::CreateTokenResponse,
            crate::routes::deploy_tokens::ListTokensQuery,
            crate::db::models::DeployTokenResponse,
            // Settings
            crate::routes::settings::UpdateSettingsRequest,
            crate::routes::settings::UpdateServiceSettingsRequest,
            // Global settings
            crate::routes::global_settings::GlobalSettings,
            crate::routes::global_settings::UpdateGlobalSettings,
            crate::routes::global_settings::CleanupDnsResponse,
            crate::routes::global_settings::SyncDnsResponse,
            // Scan
            crate::routes::scan::ImportRequest,
            crate::routes::scan::ImportResponse,
            crate::routes::scan::ImportedGroup,
            // Health
            crate::routes::health::HealthResponse,
            crate::routes::health::ServiceStats,
            crate::routes::health::SystemStatsResponse,
        )
    ),
    modifiers(&SecurityAddon),
)]
pub struct ApiDoc;

struct SecurityAddon;

impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::security::{ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};

        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "session_auth",
                SecurityScheme::ApiKey(utoipa::openapi::security::ApiKey::Cookie(ApiKeyValue::with_description(
                    "id",
                    "Session cookie (set on login)",
                ))),
            );
            components.add_security_scheme(
                "bearer_token",
                SecurityScheme::Http(
                    HttpBuilder::new()
                        .scheme(HttpAuthScheme::Bearer)
                        .bearer_format("deploy token")
                        .description(Some("Deploy token (CI/CD auth)"))
                        .build(),
                ),
            );
        }
    }
}

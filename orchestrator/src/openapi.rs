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
        (name = "backup", description = "Platform SQLite backup (Litestream sidecar)"),
        (name = "volumes", description = "Volume management"),
        (name = "scan", description = "Container scanning and import"),
        (name = "env", description = "Runtime environment variables (write-only values)"),
        (name = "github", description = "GitHub connect-and-deploy: repo browsing, workflow links, run status"),
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
        crate::routes::device_auth::start_device_flow,
        crate::routes::device_auth::lookup_device_request,
        crate::routes::device_auth::approve_device_request,
        crate::routes::device_auth::poll_device_token,
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
        crate::routes::global_settings::domain_preflight,
        crate::routes::global_settings::domain_apply,
        crate::routes::global_settings::domain_job_status,
        crate::routes::global_settings::domain_job_retry,
        // Backup
        crate::routes::backup::backup_status,
        crate::routes::backup::backup_enable,
        crate::routes::backup::backup_disable,
        crate::routes::backup::backup_add_s3,
        crate::routes::backup::backup_remove_s3,
        crate::routes::backup::backup_create_point,
        crate::routes::backup::backup_delete_point,
        crate::routes::backup::backup_restore,
        crate::routes::backup::backup_restore_local,
        crate::routes::backup::backup_restore_point,
        crate::routes::backup::backup_revert_restore,
        // Volumes
        crate::routes::volumes::delete_volume,
        crate::routes::volumes::delete_all_volumes,
        // Scan
        crate::routes::scan::scan_containers,
        crate::routes::scan::import_containers,
        // Health
        crate::routes::health::health_check,
        crate::routes::health::system_stats,
        // Meta
        crate::routes::meta::get_meta,
        crate::routes::meta::whoami,
        // Env
        crate::routes::env::get_project_env,
        crate::routes::env::update_project_env,
        // GitHub
        crate::routes::github::connect::connection_status,
        crate::routes::github::connect::list_connections,
        crate::routes::github::connect::connect_pat,
        crate::routes::github::connect::connect_app,
        crate::routes::github::connect::disconnect,
        crate::routes::github::connect::start_manifest,
        crate::routes::github::connect::exchange_manifest,
        crate::routes::github::connect::list_installations,
        crate::routes::github::repos::list_repos,
        crate::routes::github::repos::repo_detail,
        crate::routes::github::repos::list_branches,
        crate::routes::github::project_link::get_config,
        crate::routes::github::project_link::put_config,
        crate::routes::github::project_link::patch_config,
        crate::routes::github::project_link::delete_config,
        crate::routes::github::project_link::rotate_token,
        crate::routes::github::project_link::deploy_now,
        crate::routes::github::runs::project_runs,
        crate::routes::github::runs::bulk_status,
    ),
    components(
        schemas(
            // Shared types (litebin-common)
            litebin_common::types::ProjectStatus,
            litebin_common::types::NodeStatus,
            litebin_common::types::RoutingMode,
            litebin_common::types::DeployType,
            litebin_common::types::TokenScope,
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
            crate::routes::device_auth::DeviceStartRequest,
            crate::routes::device_auth::DeviceStartResponse,
            crate::routes::device_auth::DeviceRequestInfo,
            crate::routes::device_auth::DeviceApproveRequest,
            crate::routes::device_auth::DeviceTokenRequest,
            crate::routes::device_auth::DeviceTokenResponse,
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
            crate::routes::stats::DeployLogsResponse,
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
            crate::routes::global_settings::DomainPreflightRequest,
            crate::routes::global_settings::DomainPreflightResponse,
            crate::routes::global_settings::DomainApplyRequest,
            crate::routes::global_settings::DomainApplyResponse,
            crate::platform::DomainJob,
            crate::platform::DomainJobStep,
            crate::platform::DomainJobStatus,
            crate::platform::DomainStepStatus,
            // Backup
            crate::routes::backup::BackupStatus,
            crate::routes::backup::BackupS3Status,
            crate::routes::backup::BackupS3Request,
            crate::routes::backup::BackupRestoreRequest,
            crate::routes::backup::BackupActionResponse,
            crate::routes::backup::BackupPoint,
            crate::routes::backup::BackupPointRequest,
            crate::backup::ReplicaInfo,
            // Scan
            crate::routes::scan::ImportRequest,
            crate::routes::scan::ImportResponse,
            crate::routes::scan::ImportedGroup,
            // Health
            crate::routes::health::HealthResponse,
            crate::routes::health::ServiceStats,
            // Env
            crate::routes::env::UpdateEnvRequest,
            crate::routes::env::UpdateEnvResponse,
            crate::routes::env::EnvResponse,
            crate::routes::env::EnvVarInfo,
            // Meta
            crate::routes::meta::MetaResponse,
            crate::routes::meta::WhoamiResponse,
            crate::routes::health::SystemStatsResponse,
            // GitHub
            crate::routes::github::connect::ConnectionStatus,
            crate::routes::github::connect::ConnectionSummary,
            crate::routes::github::connect::PatRequest,
            crate::routes::github::connect::PatConnected,
            crate::routes::github::connect::AppCredentialsRequest,
            crate::routes::github::connect::ManifestStartResponse,
            crate::routes::github::connect::ManifestStartRequest,
            crate::routes::github::connect::ManifestExchangeRequest,
            crate::routes::github::connect::ManifestExchangeResponse,
            crate::routes::github::connect::InstallationSummary,
            crate::routes::github::repos::RepoListQuery,
            crate::github::repos::RepoPage,
            crate::github::repos::RepoSummary,
            crate::github::repos::RepoDetail,
            crate::github::repos::BranchSummary,
            crate::routes::github::project_link::WorkflowInputs,
            crate::routes::github::project_link::LinkRequest,
            crate::routes::github::project_link::ProjectGithubConfigResponse,
            crate::routes::github::project_link::PatchConfigRequest,
            crate::routes::github::project_link::DeployRequest,
            crate::routes::github::project_link::UnlinkQuery,
            crate::routes::github::runs::ProjectRuns,
            crate::github::actions::WorkflowRun,
            crate::routes::github::runs::BulkRunStatus,
            crate::routes::github::runs::BulkStatus,
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

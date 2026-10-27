pub(crate) mod helpers;

mod auth_tests;
mod backup_tests;
mod dashboard_proxy_tests;
mod deploy_stage_tests;
mod deploy_tokens_tests;
mod device_auth_tests;
mod env_tests;
// The folder tests hold a std Mutex across awaits — deliberate: it serializes
// env-var repointing between tests and nothing contends in async context.
#[allow(clippy::await_holding_lock)]
mod export_duplicate_tests;
mod health_tests;
mod janitor_tests;
mod nodes_tests;
mod orchestrator_path_tests;
mod port_settings_tests;
mod projects_tests;
mod sleep_settings_tests;
mod token_scope_tests;
mod waker_tests;

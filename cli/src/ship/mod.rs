mod build_upload;
mod deploy;
mod env;
mod flow;
mod public_service;
mod ui;
mod validate;

pub use deploy::deploy_compose_noninteractive;
pub(crate) use env::{EnvSource, resolve_env_files};
pub use flow::{run, select_server_interactive};
pub use ui::{detect_compose_file, detect_compose_files, resolve_compose_files, resolve_platform};

/// Options for non-interactive compose deploy (used by `deploy` command).
pub struct ComposeDeployOpts {
    /// If Some, only build these services (no interactive prompt).
    pub target_services: Option<Vec<String>>,
    /// Target node ID (optional).
    pub node_id: Option<String>,
    /// Capability ids to grant (e.g. docker-observe, raw-ports).
    pub grant_capabilities: Vec<String>,
    /// Capability ids to defer: stage without granting; start stays blocked
    /// until granted.
    pub defer_capabilities: Vec<String>,
    /// Rewrite explicit HOST:CONTAINER mappings to container-only so the
    /// compose stops requiring raw-ports.
    pub strip_host_ports: bool,
    /// Deploy the whole project without managed HTTP ingress.
    pub is_background: bool,
    /// How to upload images (auto/direct/relay). Mirrors the `--upload` flag.
    pub upload: crate::upload::UploadMode,
    /// Build-time env selection (`--secret` / `--no-env` / auto-discovery).
    pub env_source: EnvSource,
    /// Stage only: store images + compose without starting.
    pub stage_only: bool,
}

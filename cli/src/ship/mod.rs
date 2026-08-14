mod build_upload;
mod deploy;
mod env;
mod flow;
mod public_service;
mod ui;
mod validate;

pub use deploy::deploy_compose_noninteractive;
pub use flow::run;
pub use ui::{detect_compose_file, resolve_platform};

/// Options for non-interactive compose deploy (used by `deploy` command).
pub struct ComposeDeployOpts {
    /// If Some, only build these services (no interactive prompt).
    pub target_services: Option<Vec<String>>,
    /// Target node ID (optional).
    pub node_id: Option<String>,
    /// Capability ids to grant (e.g. docker-observe, raw-ports).
    pub grant_capabilities: Vec<String>,
    /// Deploy the whole project without managed HTTP ingress.
    pub is_background: bool,
    /// How to upload images (auto/direct/relay). Mirrors the `--upload` flag.
    pub upload: crate::upload::UploadMode,
}

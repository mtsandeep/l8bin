use thiserror::Error;

#[derive(Debug, Error)]
pub enum ComposeError {
    #[error("invalid compose YAML: {0}")]
    InvalidYaml(#[from] serde_yaml::Error),

    #[error("service '{name}' not found")]
    ServiceNotFound { name: String },

    #[error("no services defined in compose file")]
    NoServices,

    #[error("dependency cycle detected: {chain}")]
    CycleDetected { chain: String },

    #[error("service '{service}' depends on unknown service '{dep}'")]
    GhostDependency { service: String, dep: String },

    #[error("no public service detected: no service exposes a port or has label 'litebin.public=true'")]
    NoPublicService,

    #[error("multiple public services detected: {services}")]
    MultiplePublicServices { services: String },

    #[error("invalid field '{field}' on service '{service}': {reason}")]
    InvalidField {
        service: String,
        field: String,
        reason: String,
    },
}

pub type Result<T> = std::result::Result<T, ComposeError>;

pub mod error;
pub mod interpolate;
pub mod mapping;
pub mod parse;
pub mod validate;

pub use error::{ComposeError, Result};
pub use mapping::{BollardMappingOptions, ComposeBollardConfig};
pub use parse::{ComposeFile, ComposeService};

/// Parse a docker-compose.yaml string into a ComposeFile.
pub struct ComposeParser;

impl ComposeParser {
    /// Parse a compose YAML string without variable interpolation.
    pub fn parse(yaml: &str) -> Result<ComposeFile> {
        let compose: ComposeFile = serde_yaml::from_str(yaml)?;
        Ok(compose)
    }

    /// Parse a compose YAML string with variable interpolation.
    ///
    /// Supports `${VAR}`, `${VAR:-default}`, `${VAR:+alternate}`, `$VAR`, and `$$`.
    /// Variables are resolved from: (1) the compose file's own `environment` sections,
    /// (2) `extra_env` KEY=VALUE strings (e.g. from `.env` files), (3) system environment.
    pub fn parse_with_interpolation(yaml: &str, extra_env: &[String]) -> Result<ComposeFile> {
        let mut value: serde_yaml::Value = serde_yaml::from_str(yaml)?;

        // Pre-extract environment values from the compose itself (they define the values,
        // so they should NOT be interpolated themselves but used to interpolate other fields).
        let compose_env = interpolate::extract_compose_env(&value);

        // Build env map: compose env < extra_env < system env (system env is base, overridden by extras)
        let mut env = interpolate::build_env_map(extra_env);
        env.extend(compose_env); // compose env takes highest priority

        interpolate::interpolate(&mut value, &env)?;

        let compose: ComposeFile = serde_yaml::from_value(value)?;
        Ok(compose)
    }
}

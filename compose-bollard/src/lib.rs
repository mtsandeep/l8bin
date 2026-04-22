pub mod error;
pub mod mapping;
pub mod parse;
pub mod validate;

pub use error::{ComposeError, Result};
pub use mapping::{BollardMappingOptions, ComposeBollardConfig};
pub use parse::{ComposeFile, ComposeService};

/// Parse a docker-compose.yaml string into a ComposeFile.
pub struct ComposeParser;

impl ComposeParser {
    /// Parse a compose YAML string.
    pub fn parse(yaml: &str) -> Result<ComposeFile> {
        let compose: ComposeFile = serde_yaml::from_str(yaml)?;
        Ok(compose)
    }
}

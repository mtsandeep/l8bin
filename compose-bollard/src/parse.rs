use std::collections::HashMap;
use serde::{Deserialize, Serialize};

/// Parsed representation of a docker-compose.yml file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposeFile {
    #[serde(default)]
    pub services: HashMap<String, ComposeService>,
}

/// A single service from docker-compose.yml.
/// Fields are kept as Option<String> / Option<Vec<String>> to match compose format.
/// `#[serde(flatten)]` captures unknown fields silently.
/// Build configuration from a compose service.
/// Supports both string form (`build: ./api`) and object form (`build: { context: ./api, dockerfile: Dockerfile.dev }`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BuildConfig {
    /// Simple string path: `build: ./api`
    Path(String),
    /// Object form with context, dockerfile, args, etc.
    Object {
        context: Option<String>,
        dockerfile: Option<String>,
        #[serde(default)]
        args: HashMap<String, serde_yaml::Value>,
    },
    #[default]
    None,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComposeService {
    pub image: Option<String>,
    pub build: Option<BuildConfig>,

    pub command: Option<serde_yaml::Value>,
    pub entrypoint: Option<serde_yaml::Value>,
    pub working_dir: Option<String>,
    pub user: Option<String>,

    pub environment: Option<serde_yaml::Value>,
    pub labels: Option<serde_yaml::Value>,

    pub ports: Option<Vec<String>>,
    pub depends_on: Option<serde_yaml::Value>,
    pub volumes: Option<Vec<String>>,
    pub healthcheck: Option<serde_yaml::Value>,

    pub shm_size: Option<String>,
    pub tmpfs: Option<serde_yaml::Value>,
    pub read_only: Option<bool>,
    pub extra_hosts: Option<Vec<String>>,

    pub memory: Option<serde_yaml::Value>,
    pub cpus: Option<serde_yaml::Value>,

    pub cap_add: Option<Vec<String>>,
    pub cap_drop: Option<Vec<String>>,

    #[serde(flatten)]
    pub extra: HashMap<String, serde_yaml::Value>,
}

impl ComposeService {
    /// Get the build context directory (e.g. `./api` from `build: ./api`).
    /// Returns None if the service uses `image:` instead of `build:`.
    pub fn build_context(&self) -> Option<&str> {
        self.build.as_ref().and_then(|b| match b {
            BuildConfig::Path(p) => Some(p.as_str()),
            BuildConfig::Object { context, .. } => context.as_deref(),
            BuildConfig::None => None,
        })
    }

    /// Get the custom Dockerfile path relative to context (e.g. `Dockerfile.dev`).
    /// Returns None if using the default `Dockerfile`.
    pub fn build_dockerfile(&self) -> Option<&str> {
        self.build.as_ref().and_then(|b| match b {
            BuildConfig::Path(_) => None,
            BuildConfig::Object { dockerfile, .. } => dockerfile.as_deref(),
            BuildConfig::None => None,
        })
    }

    /// Parse `depends_on` into a Vec<String>.
    /// Compose format: either a list of strings or a map of service→condition.
    pub fn dependency_names(&self) -> Vec<String> {
        match &self.depends_on {
            None => Vec::new(),
            Some(serde_yaml::Value::Sequence(list)) => list
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            Some(serde_yaml::Value::Mapping(map)) => map
                .keys()
                .filter_map(|k| k.as_str().map(|s| s.to_string()))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Parse `environment` into a Vec<"KEY=VALUE">.
    /// Handles both `KEY: VALUE` (map) and `KEY=VALUE` (list) formats.
    pub fn env_list(&self) -> Vec<String> {
        match &self.environment {
            None => Vec::new(),
            Some(serde_yaml::Value::Sequence(list)) => list
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            Some(serde_yaml::Value::Mapping(map)) => map
                .iter()
                .filter_map(|(k, v)| {
                    let key = k.as_str()?;
                    let val = match v.as_str() {
                        Some(s) => s.to_string(),
                        None => serde_yaml::to_string(v).ok()?.trim_end().to_string(),
                    };
                    Some(format!("{}={}", key, val))
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Parse `command` into a Vec<String> (shell form or exec form).
    pub fn cmd_list(&self) -> Option<Vec<String>> {
        match &self.command {
            None => None,
            Some(serde_yaml::Value::String(s)) => Some(shlex::split(s).unwrap_or_else(|| vec![s.clone()])),
            Some(serde_yaml::Value::Sequence(list)) => Some(
                list.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect(),
            ),
            _ => None,
        }
    }

    /// Parse `entrypoint` into a Vec<String> (exec form or shell form).
    pub fn entrypoint_list(&self) -> Option<Vec<String>> {
        match &self.entrypoint {
            None => None,
            Some(serde_yaml::Value::String(s)) => Some(shlex::split(s).unwrap_or_else(|| vec![s.clone()])),
            Some(serde_yaml::Value::Sequence(list)) => Some(
                list.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect(),
            ),
            _ => None,
        }
    }

    /// Parse `ports` into a Vec<(container_port, protocol)>.
    /// Accepts formats: "8080", "8080/tcp", "8080/udp".
    /// Host-mapped ports ("8080:3000") are noted but only the container port is used.
    pub fn exposed_ports(&self) -> Vec<(u16, String)> {
        let mut result = Vec::new();
        if let Some(ports) = &self.ports {
            for p in ports {
                // Handle "HOST:CONTAINER/PROTOCOL" or "HOST:CONTAINER" or "CONTAINER/PROTOCOL" or "CONTAINER"
                let (container_part, protocol) = if let Some((_, after)) = p.rsplit_once(':') {
                    // Could be "HOST:CONTAINER/PROTOCOL" or just "HOST:CONTAINER"
                    if let Some((cp, proto)) = after.split_once('/') {
                        (cp, proto.to_string())
                    } else {
                        (after, "tcp".to_string())
                    }
                } else if let Some((cp, proto)) = p.split_once('/') {
                    (cp, proto.to_string())
                } else {
                    (p.as_str(), "tcp".to_string())
                };

                if let Ok(port) = container_part.parse::<u16>() {
                    result.push((port, protocol));
                }
            }
        }
        result
    }

    /// Parse `tmpfs` into a HashMap</mount/point, options>.
    /// Handles both string and map formats.
    pub fn tmpfs_map(&self) -> HashMap<String, String> {
        let mut result = HashMap::new();
        if let Some(tmpfs) = &self.tmpfs {
            match tmpfs {
                serde_yaml::Value::String(s) => {
                    // Simple "/tmp" (no options) or "/tmp:size=100m" (with options)
                    if let Some((path, opts)) = s.split_once(':') {
                        result.insert(path.trim().to_string(), opts.trim().to_string());
                    } else {
                        result.insert(s.trim().to_string(), String::new());
                    }
                }
                serde_yaml::Value::Mapping(map) => {
                    for (k, v) in map {
                        if let (Some(path), Some(opts)) = (k.as_str(), v.as_str()) {
                            result.insert(path.to_string(), opts.to_string());
                        }
                    }
                }
                serde_yaml::Value::Sequence(list) => {
                    for item in list {
                        if let Some(s) = item.as_str() {
                            if let Some((path, opts)) = s.split_once(':') {
                                result.insert(path.trim().to_string(), opts.trim().to_string());
                            } else {
                                result.insert(s.trim().to_string(), String::new());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        result
    }

    /// Parse `memory` string like "512m", "1g", "256MB" into bytes.
    pub fn memory_bytes(&self) -> Option<u64> {
        self.memory.as_ref().and_then(|v| {
            let s = v.as_str()?;
            parse_memory_size(s)
        })
    }

    /// Parse `cpus` into nano_cpus value for Docker API.
    pub fn nano_cpus(&self) -> Option<i64> {
        self.cpus.as_ref().and_then(|v| {
            let cpus = v.as_f64().or_else(|| {
                v.as_str().and_then(|s| s.parse::<f64>().ok())
            })?;
            Some((cpus * 1_000_000_000.0) as i64)
        })
    }

    /// Check if this service has the label `litebin.public=true`.
    pub fn is_public_by_label(&self) -> bool {
        match &self.labels {
            None => false,
            Some(serde_yaml::Value::Mapping(map)) => map
                .iter()
                .any(|(k, v)| {
                    k.as_str() == Some("litebin.public")
                        && v.as_str() == Some("true")
                }),
            Some(serde_yaml::Value::Sequence(list)) => list.iter().any(|v| {
                v.as_str()
                    .map(|s| s == "litebin.public=true")
                    .unwrap_or(false)
            }),
            _ => false,
        }
    }
}

/// Parse a memory size string like "512m", "1g", "256MB", "2GB" into bytes.
fn parse_memory_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num_str, multiplier) = if let Some(rest) = s.strip_suffix("gb") {
        (rest, 1024u64 * 1024 * 1024)
    } else if let Some(rest) = s.strip_suffix("g") {
        (rest, 1024u64 * 1024 * 1024)
    } else if let Some(rest) = s.strip_suffix("mb") {
        (rest, 1024u64 * 1024)
    } else if let Some(rest) = s.strip_suffix("m") {
        (rest, 1024u64 * 1024)
    } else if let Some(rest) = s.strip_suffix("kb") {
        (rest, 1024u64)
    } else if let Some(rest) = s.strip_suffix("k") {
        (rest, 1024u64)
    } else if let Some(rest) = s.strip_suffix('b') {
        (rest, 1)
    } else {
        (s, 1)
    };
    let num: f64 = num_str.trim().parse().ok()?;
    Some((num * multiplier as f64) as u64)
}

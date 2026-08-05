use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Parsed representation of a docker-compose.yaml file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposeFile {
    #[serde(default)]
    pub services: HashMap<String, ComposeService>,
}

impl ComposeFile {
    /// Services treated as one-shot: those another service depends on with
    /// `condition: service_completed_successfully`, plus any explicitly marked
    /// with the `litebin.oneshot=true` label (fire-and-forget init tasks that
    /// nothing waits on).
    pub fn oneshot_service_names(&self) -> std::collections::HashSet<String> {
        let mut names = std::collections::HashSet::new();
        for (name, svc) in &self.services {
            if svc.is_oneshot_by_label() {
                names.insert(name.clone());
            }
        }
        for service in self.services.values() {
            for (dep, cond) in service.dependency_conditions() {
                if cond == "service_completed_successfully" {
                    names.insert(dep);
                }
            }
        }
        names
    }
}

/// A single service from docker-compose.yaml.
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

    pub stdin_open: Option<bool>,
    pub tty: Option<bool>,
    pub restart: Option<String>,

    #[serde(flatten)]
    pub extra: HashMap<String, serde_yaml::Value>,
}

impl ComposeService {
    pub fn network_mode(&self) -> Option<&str> {
        self.extra.get("network_mode").and_then(serde_yaml::Value::as_str)
    }

    pub fn uses_host_network(&self) -> bool {
        self.network_mode() == Some("host")
    }

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
            Some(serde_yaml::Value::Sequence(list)) => {
                list.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
            }
            Some(serde_yaml::Value::Mapping(map)) => {
                map.keys().filter_map(|k| k.as_str().map(|s| s.to_string())).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Parse `depends_on` into (dep_name, condition) pairs.
    /// Short form `depends_on: [api]` defaults to "service_started".
    /// Map form `{ postgres: { condition: service_healthy } }` preserves the condition.
    pub fn dependency_conditions(&self) -> Vec<(String, String)> {
        match &self.depends_on {
            None => Vec::new(),
            Some(serde_yaml::Value::Sequence(list)) => {
                list.iter().filter_map(|v| v.as_str().map(|s| (s.to_string(), "service_started".to_string()))).collect()
            }
            Some(serde_yaml::Value::Mapping(map)) => map
                .iter()
                .filter_map(|(k, v)| {
                    let name = k.as_str()?.to_string();
                    let condition = v.get("condition").and_then(|c| c.as_str()).unwrap_or("service_started");
                    Some((name, condition.to_string()))
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Parse `environment` into a Vec<"KEY=VALUE">.
    /// Handles both `KEY: VALUE` (map) and `KEY=VALUE` (list) formats.
    pub fn env_list(&self) -> Vec<String> {
        match &self.environment {
            None => Vec::new(),
            Some(serde_yaml::Value::Sequence(list)) => {
                list.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
            }
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
            Some(serde_yaml::Value::Sequence(list)) => {
                Some(list.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            }
            _ => None,
        }
    }

    /// Parse `entrypoint` into a Vec<String> (exec form or shell form).
    pub fn entrypoint_list(&self) -> Option<Vec<String>> {
        match &self.entrypoint {
            None => None,
            Some(serde_yaml::Value::String(s)) => Some(shlex::split(s).unwrap_or_else(|| vec![s.clone()])),
            Some(serde_yaml::Value::Sequence(list)) => {
                Some(list.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            }
            _ => None,
        }
    }

    /// Container ports: `(container_port, protocol)`. Drops any host remap.
    pub fn exposed_ports(&self) -> Vec<(u16, String)> {
        self.port_specs().into_iter().map(|(cport, proto, _host)| (cport, proto)).collect()
    }

    /// Like `exposed_ports` but keeps an explicit host port when given
    /// (`HOST:CONTAINER[/proto]`, also `IP:HOST:CONTAINER`).
    pub fn port_specs(&self) -> Vec<(u16, String, Option<u16>)> {
        let mut result = Vec::new();
        if let Some(ports) = &self.ports {
            for p in ports {
                let s = p.as_str();
                let (core, proto) = match s.rsplit_once('/') {
                    Some((c, pr @ ("tcp" | "udp"))) => (c, pr),
                    _ => (s, "tcp"),
                };
                // The host port is the second-to-last colon segment, if any.
                let parts: Vec<&str> = core.split(':').collect();
                let (host, container) = match parts.len() {
                    1 => (None, parts[0]),
                    2 => (Some(parts[0]), parts[1]),
                    _ => (Some(parts[parts.len() - 2]), parts[parts.len() - 1]),
                };
                if let Ok(cport) = container.parse::<u16>() {
                    let host_port = host.and_then(|h| h.parse::<u16>().ok());
                    result.push((cport, proto.to_string(), host_port));
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
            let cpus = v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))?;
            Some((cpus * 1_000_000_000.0) as i64)
        })
    }

    /// Check if this service has the label `litebin.public=true`.
    pub fn is_public_by_label(&self) -> bool {
        match &self.labels {
            None => false,
            Some(serde_yaml::Value::Mapping(map)) => {
                map.iter().any(|(k, v)| k.as_str() == Some("litebin.public") && v.as_str() == Some("true"))
            }
            Some(serde_yaml::Value::Sequence(list)) => {
                list.iter().any(|v| v.as_str().map(|s| s == "litebin.public=true").unwrap_or(false))
            }
            _ => false,
        }
    }

    /// Explicitly marked as a one-shot via `litebin.oneshot=true`.
    pub fn is_oneshot_by_label(&self) -> bool {
        match &self.labels {
            None => false,
            Some(serde_yaml::Value::Mapping(map)) => {
                map.iter().any(|(k, v)| k.as_str() == Some("litebin.oneshot") && v.as_str() == Some("true"))
            }
            Some(serde_yaml::Value::Sequence(list)) => {
                list.iter().any(|v| v.as_str().map(|s| s == "litebin.oneshot=true").unwrap_or(false))
            }
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

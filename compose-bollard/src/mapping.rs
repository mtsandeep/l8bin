use std::collections::HashMap;

use bollard::models::{ContainerCreateBody, HealthConfig, HostConfig};

use crate::parse::ComposeService;

/// Options for the compose-to-bollard mapping.
#[derive(Debug, Clone, Default)]
pub struct BollardMappingOptions {
    /// Extra env vars merged on top of compose environment (compose values take precedence).
    pub env_overrides: HashMap<String, String>,

    /// Auto-add /tmp as tmpfs when read_only is true and no tmpfs is defined.
    pub auto_tmpfs_for_readonly: bool,
}

/// The result of mapping a compose service to bollard config structs.
#[derive(Debug, Clone)]
pub struct ComposeBollardConfig {
    pub create_body: ContainerCreateBody,
    pub host_config: HostConfig,
}

impl ComposeService {
    /// Convert this compose service into bollard config structs.
    /// The caller should override `host_config.binds`, `host_config.port_bindings`,
    /// and networking after this call.
    pub fn to_bollard_config(&self, options: &BollardMappingOptions) -> ComposeBollardConfig {
        let mut host_config = HostConfig::default();

        // Memory
        if let Some(bytes) = self.memory_bytes() {
            host_config.memory = Some(bytes as i64);
        }

        // CPU
        if let Some(nano_cpus) = self.nano_cpus() {
            host_config.nano_cpus = Some(nano_cpus);
        }

        // Read-only root filesystem
        if self.read_only == Some(true) {
            host_config.readonly_rootfs = Some(true);
        }

        // shm_size (parse "256m" etc.)
        if let Some(shm) = &self.shm_size {
            if let Some(bytes) = parse_size(shm) {
                host_config.shm_size = Some(bytes as i64);
            }
        }

        // tmpfs
        let mut tmpfs = self.tmpfs_map();
        // Auto-add /tmp for read_only containers
        if self.read_only == Some(true)
            && options.auto_tmpfs_for_readonly
            && !tmpfs.contains_key("/tmp")
        {
            tmpfs.insert("/tmp".to_string(), String::new());
        }
        if !tmpfs.is_empty() {
            host_config.tmpfs = Some(tmpfs);
        }

        // extra_hosts
        if let Some(hosts) = &self.extra_hosts {
            if !hosts.is_empty() {
                host_config.extra_hosts = Some(hosts.clone());
            }
        }

        // cap_add / cap_drop
        if let Some(caps) = &self.cap_add {
            if !caps.is_empty() {
                host_config.cap_add = Some(caps.clone());
            }
        }
        if let Some(caps) = &self.cap_drop {
            if !caps.is_empty() {
                host_config.cap_drop = Some(caps.clone());
            }
        }

        // Build env list: compose env first, then overrides (compose takes precedence on conflict)
        let mut env = self.env_list();
        // Add overrides that aren't already in the env list
        for (key, val) in &options.env_overrides {
            let has_key = env.iter().any(|e| {
                e.split_once('=')
                    .map(|(k, _)| k == key.as_str())
                    .unwrap_or(false)
            });
            if !has_key {
                env.push(format!("{}={}", key, val));
            }
        }

        // Exposed ports
        let exposed_ports: Vec<String> = self
            .exposed_ports()
            .iter()
            .map(|(port, proto)| format!("{}/{}", port, proto))
            .collect();

        // Only set cmd/entrypoint if explicitly defined in compose.
        // Setting them to None would override the Dockerfile's CMD/ENTRYPOINT.
        let cmd = self.cmd_list();
        let entrypoint = self.entrypoint_list();
        let has_command = cmd.is_some() || entrypoint.is_some();

        // Healthcheck
        let healthcheck = self.healthcheck.as_ref().and_then(|hc| parse_healthcheck(hc));

        let create_body = ContainerCreateBody {
            image: self.image.clone(),
            cmd: if has_command { cmd } else { None },
            entrypoint: if has_command { entrypoint } else { None },
            working_dir: self.working_dir.clone(),
            user: self.user.clone(),
            env: if env.is_empty() { None } else { Some(env) },
            exposed_ports: if exposed_ports.is_empty() {
                None
            } else {
                Some(exposed_ports)
            },
            host_config: Some(host_config.clone()),
            healthcheck,
            ..Default::default()
        };

        ComposeBollardConfig {
            create_body,
            host_config,
        }
    }
}

/// Parse a compose healthcheck definition into a bollard HealthConfig.
/// Handles both list form (`["CMD", "pg_isready"]`) and string form (`"CMD pg_isready"`).
fn parse_healthcheck(hc: &serde_yaml::Value) -> Option<HealthConfig> {
    let test = hc.get("test")?;
    let test_vec: Vec<String> = match test {
        serde_yaml::Value::Sequence(list) => list
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        serde_yaml::Value::String(s) => {
            // String form: "CMD pg_isready" or "CMD-SHELL pg_isready -U foo"
            if let Some(rest) = s.strip_prefix("CMD-SHELL ") {
                vec!["CMD-SHELL".to_string(), rest.to_string()]
            } else if let Some(rest) = s.strip_prefix("CMD ") {
                shlex::split(rest).unwrap_or_else(|| vec![rest.to_string()])
            } else {
                shlex::split(s).unwrap_or_else(|| vec![s.clone()])
            }
        }
        _ => return None,
    };

    // "NONE" disables the healthcheck
    if test_vec.len() == 1 && test_vec[0].eq_ignore_ascii_case("NONE") {
        return Some(HealthConfig {
            test: Some(test_vec),
            ..Default::default()
        });
    }

    let interval = hc.get("interval").and_then(|v| parse_duration(v));
    let timeout = hc.get("timeout").and_then(|v| parse_duration(v));
    let start_period = hc.get("start_period").and_then(|v| parse_duration(v));
    let retries = hc.get("retries").and_then(|v| v.as_u64()).map(|r| r as i64);

    Some(HealthConfig {
        test: Some(test_vec),
        interval,
        timeout,
        start_period,
        start_interval: None,
        retries,
    })
}

/// Parse a compose duration string ("2s", "1m", "100ms") into nanoseconds.
fn parse_duration(v: &serde_yaml::Value) -> Option<i64> {
    let s = v.as_str()?;
    let s = s.trim();
    let (num_str, multiplier_ns) = if let Some(rest) = s.strip_suffix("ns") {
        (rest, 1i64)
    } else if let Some(rest) = s.strip_suffix("us") {
        (rest, 1_000i64)
    } else if let Some(rest) = s.strip_suffix("ms") {
        (rest, 1_000_000i64)
    } else if let Some(rest) = s.strip_suffix('s') {
        (rest, 1_000_000_000i64)
    } else if let Some(rest) = s.strip_suffix('m') {
        (rest, 60_000_000_000i64)
    } else if let Some(rest) = s.strip_suffix('h') {
        (rest, 3_600_000_000_000i64)
    } else {
        return None;
    };
    let num: f64 = num_str.trim().parse().ok()?;
    Some((num * multiplier_ns as f64) as i64)
}

/// Parse a size string like "256m", "1g" into bytes.
fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase();
    let (num_str, multiplier) = if let Some(rest) = s.strip_suffix("gb") {
        (rest, 1024u64 * 1024 * 1024)
    } else if let Some(rest) = s.strip_suffix('g') {
        (rest, 1024u64 * 1024 * 1024)
    } else if let Some(rest) = s.strip_suffix("mb") {
        (rest, 1024u64 * 1024)
    } else if let Some(rest) = s.strip_suffix('m') {
        (rest, 1024u64 * 1024)
    } else if let Some(rest) = s.strip_suffix("kb") {
        (rest, 1024u64)
    } else if let Some(rest) = s.strip_suffix('k') {
        (rest, 1024u64)
    } else if let Some(rest) = s.strip_suffix('b') {
        (rest, 1)
    } else {
        (s.as_str(), 1)
    };
    let num: f64 = num_str.trim().parse().ok()?;
    Some((num * multiplier as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_mapping() {
        let svc = ComposeService {
            image: Some("nginx:latest".to_string()),
            command: Some(serde_yaml::Value::String("nginx -g 'daemon off'".to_string())),
            environment: Some(serde_yaml::Value::Mapping(
                vec![
                    (
                        serde_yaml::Value::String("FOO".to_string()),
                        serde_yaml::Value::String("bar".to_string()),
                    ),
                    (
                        serde_yaml::Value::String("BAZ".to_string()),
                        serde_yaml::Value::Number(serde_yaml::Number::from(42)),
                    ),
                ]
                .into_iter()
                .collect(),
            )),
            working_dir: Some("/app".to_string()),
            ..Default::default()
        };

        let config = svc.to_bollard_config(&BollardMappingOptions::default());
        assert_eq!(config.create_body.image, Some("nginx:latest".to_string()));
        assert!(config.create_body.cmd.is_some());
        assert_eq!(config.create_body.working_dir, Some("/app".to_string()));

        let env = config.create_body.env.unwrap();
        assert!(env.contains(&"FOO=bar".to_string()));
        assert!(env.contains(&"BAZ=42".to_string()));
    }

    #[test]
    fn memory_and_cpus() {
        let svc = ComposeService {
            image: Some("app".to_string()),
            memory: Some(serde_yaml::Value::String("512m".to_string())),
            cpus: Some(serde_yaml::Value::String("1.5".to_string())),
            ..Default::default()
        };

        let config = svc.to_bollard_config(&BollardMappingOptions::default());
        assert_eq!(config.host_config.memory, Some(512 * 1024 * 1024));
        assert_eq!(config.host_config.nano_cpus, Some(1_500_000_000_i64));
    }

    #[test]
    fn readonly_auto_tmpfs() {
        let svc = ComposeService {
            image: Some("app".to_string()),
            read_only: Some(true),
            ..Default::default()
        };

        let config = svc.to_bollard_config(&BollardMappingOptions {
            auto_tmpfs_for_readonly: true,
            ..Default::default()
        });
        assert_eq!(config.host_config.readonly_rootfs, Some(true));
        let tmpfs = config.host_config.tmpfs.unwrap();
        assert!(tmpfs.contains_key("/tmp"));
    }

    #[test]
    fn env_overrides_dont_override_compose() {
        let svc = ComposeService {
            image: Some("app".to_string()),
            environment: Some(serde_yaml::Value::Mapping(
                vec![(
                    serde_yaml::Value::String("DB".to_string()),
                    serde_yaml::Value::String("postgres://local".to_string()),
                )]
                .into_iter()
                .collect(),
            )),
            ..Default::default()
        };

        let mut overrides = HashMap::new();
        overrides.insert("DB".to_string(), "postgres://remote".to_string());
        overrides.insert("NEW".to_string(), "value".to_string());

        let config = svc.to_bollard_config(&BollardMappingOptions {
            env_overrides: overrides,
            auto_tmpfs_for_readonly: false,
        });

        let env = config.create_body.env.unwrap();
        // Compose value takes precedence
        assert!(env.contains(&"DB=postgres://local".to_string()));
        // Override adds new vars
        assert!(env.contains(&"NEW=value".to_string()));
    }

    #[test]
    fn ports_parsing() {
        let svc = ComposeService {
            image: Some("app".to_string()),
            ports: Some(vec![
                "8080".to_string(),
                "3000:3000".to_string(),
                "9090/udp".to_string(),
            ]),
            ..Default::default()
        };

        let ports = svc.exposed_ports();
        assert_eq!(ports.len(), 3);
        assert_eq!(ports[0], (8080, "tcp".to_string()));
        assert_eq!(ports[1], (3000, "tcp".to_string()));
        assert_eq!(ports[2], (9090, "udp".to_string()));
    }
}

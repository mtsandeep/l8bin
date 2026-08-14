use super::FindingDisposition;

/// Known top-level Compose keys LiteBin may encounter.
pub(super) const KNOWN_TOP_LEVEL: &[&str] =
    &["version", "name", "services", "volumes", "networks", "configs", "secrets", "x-litebin"];

/// Service fields LiteBin maps into Bollard / runtime config.
pub(super) const SUPPORTED_SERVICE_FIELDS: &[&str] = &[
    "image",
    "build",
    "command",
    "entrypoint",
    "working_dir",
    "user",
    "environment",
    "labels",
    "ports",
    "depends_on",
    "volumes",
    "healthcheck",
    "shm_size",
    "tmpfs",
    "read_only",
    "extra_hosts",
    "memory",
    "cpus",
    "cap_add",
    "cap_drop",
    "stdin_open",
    "tty",
    "restart",
];

/// Service fields that are recognized but not yet implemented.
pub(super) const UNSUPPORTED_SERVICE_FIELDS: &[(&str, &str)] = &[
    ("network_mode", "network_mode is not applied yet; LiteBin always uses a managed project bridge network"),
    ("networks", "custom Compose networks are ignored; LiteBin creates a per-project bridge network"),
    ("privileged", "privileged mode is not supported"),
    ("pid", "pid namespace sharing is not supported"),
    ("devices", "device mounts are not supported"),
    ("ipc", "IPC namespace sharing is not supported"),
    ("uts", "UTS namespace options are not supported"),
    ("runtime", "custom container runtimes are not supported"),
    ("cgroup_parent", "cgroup_parent is not supported"),
    ("sysctls", "sysctls are not supported"),
];

/// Soft-ignored service fields (informational unsupported / overridden).
pub(super) const IGNORED_SERVICE_FIELDS: &[(&str, FindingDisposition, &str)] = &[
    (
        "container_name",
        FindingDisposition::Overridden,
        "container_name is overridden; LiteBin names containers litebin-<project>.<service>",
    ),
    (
        "env_file",
        FindingDisposition::Overridden,
        "env_file is not loaded from Compose; provide runtime env via LiteBin secrets / .env.l8bin instead",
    ),
    ("logging", FindingDisposition::Overridden, "logging is overridden by LiteBin (json-file, 10m × 3)"),
    (
        "deploy",
        FindingDisposition::Overridden,
        "deploy.* (Swarm) is ignored; use top-level memory/cpus for resource limits",
    ),
    ("profiles", FindingDisposition::Overridden, "Compose profiles are ignored; all services in the file are deployed"),
    ("security_opt", FindingDisposition::Overridden, "security_opt is overridden by LiteBin (no-new-privileges)"),
    ("dns", FindingDisposition::Overridden, "custom DNS is ignored"),
    ("hostname", FindingDisposition::Overridden, "hostname is ignored"),
    ("domainname", FindingDisposition::Overridden, "domainname is ignored"),
    (
        "platform",
        FindingDisposition::Overridden,
        "platform is ignored from Compose; build/pull uses the target node architecture",
    ),
    ("ulimits", FindingDisposition::Overridden, "ulimits are ignored"),
    (
        "expose",
        FindingDisposition::Translated,
        "expose is ignored for routing; use ports (or LiteBin public service selection) for HTTP ingress",
    ),
    ("init", FindingDisposition::Overridden, "init: true is ignored"),
    (
        "stop_grace_period",
        FindingDisposition::Overridden,
        "stop_grace_period is ignored; LiteBin manages stop timeouts",
    ),
    ("stop_signal", FindingDisposition::Overridden, "stop_signal is ignored"),
];

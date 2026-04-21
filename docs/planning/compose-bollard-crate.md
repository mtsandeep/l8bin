# compose-bollard

A Rust crate that converts Docker Compose YAML into bollard Docker API config structs.

**Repository:** `crates/compose-bollard/` (internal workspace crate, publish later)

---

## Why

LiteBin needs to deploy multi-service projects from `docker-compose.yml` files. Docker's API (bollard) doesn't accept compose format — it needs typed structs (`ContainerCreateBody`, `HostConfig`, `NetworkingConfig`). Someone has to do the conversion.

No existing Rust crate does this well:

| Crate | Problem |
|---|---|
| `docker-compose-types` | Type definitions only — no mapping to bollard |
| `docktopus` | Full orchestration layer — makes deployment decisions that conflict with LiteBin's orchestration |
| DIY inline code | Tightly coupled to LiteBin, not reusable |

`compose-bollard` fills the gap: compose YAML in, bollard config structs out. No orchestration, no deployment decisions, no opinions.

---

## What It Does

One job: `docker-compose.yml` → bollard config structs.

```rust
let compose = ComposeParser::parse(&yaml_content)?;
let service = compose.get_service("web")?;

// Default mapping: all compose fields → bollard types
let mut config = service.to_bollard_config(&options)?;

// config.create_body    → ContainerCreateBody
// config.host_config    → HostConfig
// config.networking_config → NetworkingConfig
```

### What It Maps (Generic)

All compose fields with a direct bollard equivalent. v1 fields are listed in the [v1 scope](#v1-scope-mvp) section below — the rest are future additions.

| Compose Field | Bollard Target |
|---|---|
| `image` | `ContainerCreateBody.image` |
| `command` | `ContainerCreateBody.cmd` |
| `entrypoint` | `ContainerCreateBody.entrypoint` |
| `working_dir` | `ContainerCreateBody.working_dir` |
| `user` | `ContainerCreateBody.user` |
| `environment` | `ContainerCreateBody.env` |
| `labels` | `ContainerCreateBody.labels` |
| `container_name` | Returned as metadata (used as container name parameter) |
| `shm_size` | `HostConfig.shm_size` (parses `"256m"` → bytes) |
| `tmpfs` | `HostConfig.tmpfs` (parses options: `"size=100m,noexec"`) |
| `read_only` | `HostConfig.read_only` (auto-adds `/tmp` tmpfs if no tmpfs defined) |
| `extra_hosts` | `HostConfig.extra_hosts` |
| `memory` / `mem_limit` | `HostConfig.memory` (parses `"512m"` → bytes) |
| `cpus` / `cpu_quota` | `HostConfig.nano_cpus` |
| `cap_add` / `cap_drop` | `HostConfig.cap_add/cap_drop` |
| `dns` | `HostConfig.dns` |
| `ulimits` | `HostConfig.ulimits` |
| `privileged` | `HostConfig.privileged` |
| `security_opt` | `HostConfig.security_opt` |
| `logging` | `HostConfig.log_config` (`driver` + `options`) |
| `healthcheck` | `HealthConfig` (returned as metadata, set on container after create) |
| `ports` | `HostConfig.port_bindings` (parses ranges: `"8080-8090:3000"`) |
| `volumes` | `HostConfig.binds` (parses mount options: `:ro`, `:noexec`) |
| `restart` | `HostConfig.restart_policy` (`always` → `unless-stopped`) |
| `depends_on` | Parsed for topological sort (not passed to bollard) |
| `env_file` | Read file, merge into environment (opt-in via options) |
| `${VAR}` interpolation | Variable substitution in all string values (opt-in via options) |

### Override Pattern

The caller gets the full default mapping and overrides what they need:

```rust
let mut config = service.to_bollard_config(&options)?;

// Override only what your orchestration requires
config.host_config.binds = Some(my_binds);          // LiteBin: project data dirs
config.host_config.port_bindings = Some(my_ports);  // LiteBin: controlled allocation
config.networking_config = Some(my_networks);      // LiteBin: per-project network
config.create_body.env = Some(my_env);              // LiteBin: project .env merge
// Standalone user: use defaults as-is, override only what you need

// Everything else flows through from compose automatically
docker.create_container(None, config.create_body, None).await?;
```

When the crate adds support for new compose fields, the caller gets them automatically — no code changes needed.

---

## API Design

```rust
use compose_bollard::{ComposeParser, BollardMappingOptions, ComposeServiceConfig};

/// Parse a compose file
let compose = ComposeParser::parse(&yaml_content)?;

/// List services
let services: Vec<&str> = compose.service_names();

/// Get a specific service
let service = compose.get_service("web")?;

/// Convert to bollard config
let config = service.to_bollard_config(&BollardMappingOptions {
    env_overrides: HashMap::from([
        ("DATABASE_URL".into(), "postgres://...".into()),
    ]),
})?;

/// Access bollard types directly
let create_body: ContainerCreateBody = config.create_body;
let host_config: HostConfig<()> = config.host_config;
let networking: NetworkingConfig = config.networking_config;

/// Validation helpers
let order = compose.topological_sort()?;        // Vec<String> — service names in order
let public = compose.detect_public_service()?;  // Option<String> — public service name
let cycles = compose.detect_cycles()?;          // Option<Vec<String>> — cycle chain if any
```

### Options

```rust
pub struct BollardMappingOptions {
    /// Extra env vars merged on top of compose environment (compose values take precedence)
    pub env_overrides: HashMap<String, String>,

    /// Auto-add /tmp as tmpfs when read_only is true and no tmpfs is defined
    pub auto_tmpfs_for_readonly: bool,  // default: true

    /// Always add host.docker.internal:host-gateway for Linux hosts
    pub auto_extra_hosts: bool,  // default: false
}
```

---

## Ignored Fields

Compose fields captured via `#[serde(flatten)]` and returned as warnings:

| Field | Reason |
|---|---|
| `build:` | Requires build agent — not supported |
| `networks:` | Collapsed to single network by caller |
| `volumes:` (top-level) | Converted to bind mounts by caller |
| `hostname:` | DNS aliases used instead |
| `deploy.resources.replicas` | Not supported (single instance per service) |
| `profiles` | Not supported (all services deployed) |
| `extends` | Not supported (inlining only) |

---

## Dependencies

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_yaml = "0.9"
bollard = "0.18"
thiserror = "2"
```

Minimal. No CLI tools, no Docker daemon dependency, no runtime overhead.

---

## File Structure

```
crates/compose-bollard/
├── Cargo.toml
└── src/
    ├── lib.rs           — Public API: ComposeParser, ComposeService
    ├── parse.rs         — ComposeFile, ComposeService serde structs
    ├── mapping.rs       — ComposeService → bollard config structs
    ├── validate.rs      — Topological sort, cycle detection, public service detection
    └── error.rs         — Error types
```

Estimated: ~300-400 LoC for v1.

---

## v1 Scope (MVP)

Most commonly used compose fields that cover 90%+ of real-world compose files:

- `image`, `command`, `entrypoint`, `working_dir`, `user`
- `environment`, `labels`
- `ports` (mapped to `HostConfig.port_bindings`)
- `depends_on` (parsed for topological sort)
- `volumes` (mapped to `HostConfig.binds`)
- `shm_size`, `tmpfs`, `read_only`, `extra_hosts`
- `memory`, `cpus`
- `cap_add`, `cap_drop`

---

## Future

| Addition | When |
|---|---|
| `healthcheck` mapping | Post-MVP health checks feature |
| `dns`, `dns_search`, `dns_opt` | As needed |
| `ulimits` | As needed |
| `privileged`, `security_opt` | As needed |
| `logging` | As needed |
| `pid`, `userns_mode`, `ipc` | As needed |
| `mac_address` | As needed |
| `sysctls` | As needed |
| `restart` | As needed |
| `container_name` | As needed |
| `env_file` | As needed |
| Compose variable interpolation (`${VAR}`, `${VAR:-default}`) | As needed |
| Compose extends | As needed |
| Compose profiles | As needed |
| Compose YAML anchors/merge (already handled by serde_yaml) | Already works |
| Publish to crates.io | When stable and useful beyond LiteBin |

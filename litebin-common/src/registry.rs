//! Registry host parsing, mirror chains, and per-image-reference pull resolution.
//!
//! Docker matches the `X-Registry-Auth` payload against the registry being
//! pulled by `serveraddress`, so credentials are attached per pull candidate.
//! Pulls try a registry's mirrors in order before the registry itself.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// One configured mirror: endpoint plus optional credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MirrorConfig {
    /// Mirror endpoint (`https://mirror.example.com[/prefix]`). Must serve the
    /// upstream registry's repository paths (pull-through caches do).
    pub url: String,
    /// base64 `user:password`; None = anonymous. A mirror's credentials are
    /// its own — the registry's credentials never apply to mirrors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
}

/// One configured registry: endpoint, optional credentials, ordered mirrors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RegistryConfig {
    /// Canonical auth serveraddress, e.g. `https://index.docker.io/v1/`.
    pub serveraddress: String,
    /// base64 `user:password`; None = anonymous.
    pub auth: Option<String>,
    /// Mirrors tried in order before the registry itself (the final fallback).
    pub mirrors: Vec<MirrorConfig>,
}

/// Pull configuration: configured registries by canonical host, plus the
/// default registry for image names that carry no registry host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RegistryState {
    pub default_host: Option<String>,
    pub registries: HashMap<String, RegistryConfig>,
}

/// Wire form of [`RegistryState`] for orchestrator → agent dispatches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RegistryWireConfig {
    /// serveraddress of the default registry, if one is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    pub registries: Vec<RegistryConfig>,
}

impl RegistryWireConfig {
    pub fn is_empty(&self) -> bool {
        self.default.is_none() && self.registries.is_empty()
    }
}

/// One pull source for an image reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullCandidate {
    /// Reference to actually pull and create the container with (rewritten
    /// for mirrors and non-docker.io defaults).
    pub image_ref: String,
    /// Canonical host of the registry this candidate belongs to.
    pub host: String,
    /// base64 `user:password` for this source; None = anonymous.
    pub auth: Option<String>,
    pub is_mirror: bool,
}

/// Host component of an image reference; `None` means Docker Hub
/// (single-component refs and host-less namespaces like `library/postgres`).
/// Docker's rule: the first path component is a registry host when it contains
/// a `.`, a `:` (port), or is the literal `localhost`.
pub fn image_ref_host(image_ref: &str) -> Option<&str> {
    if image_ref.starts_with("sha256:") {
        return None;
    }
    let mut parts = image_ref.split('/');
    let first = parts.next()?;
    // Single component: `name[:tag]` — the colon is a tag separator.
    parts.next()?;
    if first.contains('.') || first.contains(':') || first == "localhost" { Some(first) } else { None }
}

/// Canonical host: Docker Hub is reachable under three names but is one
/// registry with one credential.
pub fn canonical_host(host: &str) -> &str {
    match host {
        "index.docker.io" | "registry-1.docker.io" => "docker.io",
        other => other,
    }
}

/// Host of a registry `serveraddress`/URL (`https://ghcr.io`,
/// `https://index.docker.io/v1/`, `registry.example.com`).
pub fn host_from_url(url: &str) -> Option<String> {
    let rest = match url.split_once("://") {
        Some((_, rest)) => rest,
        None => url,
    };
    let authority = rest.split('/').next()?.trim();
    (!authority.is_empty()).then(|| authority.to_string())
}

/// Registry host an image reference will pull from, canonicalized
/// (`ghcr.io/a/b` → `ghcr.io`, `postgres:16` → `docker.io`).
pub fn image_ref_canonical_host(image_ref: &str) -> &str {
    match image_ref_host(image_ref) {
        Some(host) => canonical_host(host),
        None => "docker.io",
    }
}

/// Decode a base64 `auth` into `(username, password)`.
pub fn decode_auth(auth_b64: &str) -> Option<(String, String)> {
    use base64::Engine as _;

    let decoded = base64::engine::general_purpose::STANDARD.decode(auth_b64.trim()).ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (username, password) = decoded.split_once(':')?;
    (!username.is_empty()).then_some((username.to_string(), password.to_string()))
}

/// Ordered pull candidates for `image_ref`: the registry's mirrors in order,
/// then the registry itself. Names without a registry host resolve through the
/// default registry (falling back to the daemon's docker.io semantics when no
/// default is configured). Unknown registries and `sha256:` refs (uploaded
/// images) resolve to a single as-is candidate.
pub fn pull_candidates(image_ref: &str, state: &RegistryState) -> Vec<PullCandidate> {
    if image_ref.starts_with("sha256:") {
        return vec![as_is(image_ref, "docker.io", None, false)];
    }

    match image_ref_host(image_ref) {
        Some(host) => {
            let canonical = canonical_host(host);
            match state.registries.get(canonical) {
                Some(config) => {
                    let mut candidates: Vec<PullCandidate> =
                        config.mirrors.iter().map(|mirror| mirror_candidate(image_ref, mirror)).collect();
                    candidates.push(PullCandidate {
                        image_ref: image_ref.to_string(),
                        host: canonical.to_string(),
                        auth: config.auth.clone(),
                        is_mirror: false,
                    });
                    candidates
                }
                None => vec![as_is(image_ref, canonical, None, false)],
            }
        }
        None => {
            match state.default_host.as_deref().and_then(|host| state.registries.get(host).map(|config| (host, config)))
            {
                Some((host, config)) => {
                    let mut candidates: Vec<PullCandidate> =
                        config.mirrors.iter().map(|mirror| mirror_candidate(image_ref, mirror)).collect();
                    // Bare names keep their daemon semantics when the default IS
                    // docker.io; any other default must be spelled out in the ref.
                    let main_ref = if host == "docker.io" {
                        image_ref.to_string()
                    } else {
                        rewrite_unhosted(image_ref, &config.serveraddress)
                    };
                    candidates.push(PullCandidate {
                        image_ref: main_ref,
                        host: host.to_string(),
                        auth: config.auth.clone(),
                        is_mirror: false,
                    });
                    candidates
                }
                None => vec![as_is(image_ref, "docker.io", None, false)],
            }
        }
    }
}

fn as_is(image_ref: &str, host: &str, auth: Option<String>, is_mirror: bool) -> PullCandidate {
    PullCandidate { image_ref: image_ref.to_string(), host: host.to_string(), auth, is_mirror }
}

/// Candidate for one mirror: the reference rewritten onto the mirror (qualified
/// refs keep their path; bare names gain the `library/` namespace), with the
/// mirror's own credentials.
fn mirror_candidate(image_ref: &str, mirror: &MirrorConfig) -> PullCandidate {
    let host = host_from_url(&mirror.url).map(|h| canonical_host(&h).to_string()).unwrap_or_default();
    let rewritten = match image_ref_host(image_ref) {
        // Qualified: replace the registry host with the mirror prefix.
        Some(_) => match image_ref.split_once('/') {
            Some((_, rest)) => format!("{}/{}", ref_prefix(&mirror.url), rest),
            None => image_ref.to_string(),
        },
        None => rewrite_unhosted(image_ref, &mirror.url),
    };
    PullCandidate { image_ref: rewritten, host, auth: mirror.auth.clone(), is_mirror: true }
}

/// Rewrite a host-less reference onto an endpoint prefix. `library/` is a
/// Docker Hub official-image namespace, inserted only for bare names.
fn rewrite_unhosted(image_ref: &str, endpoint_url: &str) -> String {
    let prefix = ref_prefix(endpoint_url);
    if image_ref.contains('/') {
        // Namespaced (`myorg/app`) — path preserved.
        format!("{prefix}/{image_ref}")
    } else {
        let split = image_ref.find([':', '@']).unwrap_or(image_ref.len());
        let (name, suffix) = image_ref.split_at(split);
        format!("{prefix}/library/{name}{suffix}")
    }
}

/// Reference prefix of an endpoint: authority plus any path
/// (`https://zot.example.com/hub` → `zot.example.com/hub`).
fn ref_prefix(url: &str) -> String {
    let rest = match url.split_once("://") {
        Some((_, rest)) => rest,
        None => url,
    };
    rest.trim_end_matches('/').to_string()
}

/// Build pull state from wire config (orchestrator load or agent merge).
/// Entries whose URL yields no host are skipped — the API rejects those.
pub fn registry_state(wire: &RegistryWireConfig) -> RegistryState {
    let mut registries = HashMap::new();
    for config in &wire.registries {
        if let Some(host) = host_from_url(&config.serveraddress) {
            registries.insert(canonical_host(&host).to_string(), config.clone());
        }
    }
    let default_host = wire
        .default
        .as_deref()
        .and_then(host_from_url)
        .map(|host| canonical_host(&host).to_string())
        .filter(|host| registries.contains_key(host));
    RegistryState { default_host, registries }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mirror(url: &str, auth: Option<&str>) -> MirrorConfig {
        MirrorConfig { url: url.to_string(), auth: auth.map(str::to_string) }
    }

    fn config(url: &str, auth: Option<&str>, mirrors: &[MirrorConfig]) -> RegistryConfig {
        RegistryConfig { serveraddress: url.to_string(), auth: auth.map(str::to_string), mirrors: mirrors.to_vec() }
    }

    fn hub_with_mirror() -> RegistryState {
        registry_state(&RegistryWireConfig {
            default: Some("https://index.docker.io/v1/".into()),
            registries: vec![config(
                "https://index.docker.io/v1/",
                Some("dXNlcjpwYXNz"),
                &[mirror("https://mirror.gcr.io", None)],
            )],
        })
    }

    #[test]
    fn image_ref_host_extraction() {
        assert_eq!(image_ref_host("nginx"), None);
        assert_eq!(image_ref_host("nginx:1.25"), None);
        assert_eq!(image_ref_host("library/postgres"), None);
        assert_eq!(image_ref_host("my-app-web"), None);
        assert_eq!(image_ref_host("ghcr.io/foo/bar"), Some("ghcr.io"));
        assert_eq!(image_ref_host("ghcr.io/foo/bar:1.2"), Some("ghcr.io"));
        assert_eq!(image_ref_host("registry.example.com/demos/app:latest"), Some("registry.example.com"));
        assert_eq!(image_ref_host("localhost:5000/img"), Some("localhost:5000"));
        assert_eq!(image_ref_host("10.0.0.1:5000/img:tag"), Some("10.0.0.1:5000"));
        assert_eq!(image_ref_host("localhost/img"), Some("localhost"));
        assert_eq!(image_ref_host("sha256:abc123"), None);
    }

    #[test]
    fn canonical_host_collapses_docker_hub_aliases() {
        assert_eq!(canonical_host("docker.io"), "docker.io");
        assert_eq!(canonical_host("index.docker.io"), "docker.io");
        assert_eq!(canonical_host("registry-1.docker.io"), "docker.io");
        assert_eq!(canonical_host("ghcr.io"), "ghcr.io");
    }

    #[test]
    fn host_from_url_variants() {
        assert_eq!(host_from_url("https://ghcr.io").as_deref(), Some("ghcr.io"));
        assert_eq!(host_from_url("https://index.docker.io/v1/").as_deref(), Some("index.docker.io"));
        assert_eq!(host_from_url("http://localhost:5000").as_deref(), Some("localhost:5000"));
        assert_eq!(host_from_url("registry.example.com").as_deref(), Some("registry.example.com"));
        assert_eq!(host_from_url("https://"), None);
        assert_eq!(host_from_url(""), None);
    }

    #[test]
    fn image_ref_canonical_host_defaults_to_docker_hub() {
        assert_eq!(image_ref_canonical_host("postgres:16"), "docker.io");
        assert_eq!(image_ref_canonical_host("registry-1.docker.io/x/y"), "docker.io");
        assert_eq!(image_ref_canonical_host("ghcr.io/x/y"), "ghcr.io");
    }

    #[test]
    fn decode_auth_variants() {
        // base64 of "user:pass"
        assert_eq!(decode_auth("dXNlcjpwYXNz").as_ref(), Some(&("user".to_string(), "pass".to_string())));
        // password may contain colons
        let colon_pass = decode_auth("dXNlcjpwYTpzczp3b3Jk").unwrap();
        assert_eq!(colon_pass.0, "user");
        assert_eq!(colon_pass.1, "pa:ss:word");
        assert_eq!(decode_auth("cGFzc3dvcmQtbm8tdXNlcg"), None); // no colon
        assert_eq!(decode_auth(""), None);
        assert_eq!(decode_auth("!!!not-base64!!!"), None);
    }

    #[test]
    fn nameless_ref_uses_default_mirrors_then_hub() {
        let candidates = pull_candidates("nginx:alpine", &hub_with_mirror());
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].image_ref, "mirror.gcr.io/library/nginx:alpine");
        assert!(candidates[0].is_mirror);
        assert_eq!(candidates[0].host, "mirror.gcr.io");
        assert!(candidates[0].auth.is_none());
        assert_eq!(candidates[1].image_ref, "nginx:alpine");
        assert!(!candidates[1].is_mirror);
        assert_eq!(candidates[1].host, "docker.io");
        assert_eq!(candidates[1].auth.as_deref(), Some("dXNlcjpwYXNz"));
    }

    #[test]
    fn namespaced_nameless_ref_preserves_path_on_mirrors() {
        let candidates = pull_candidates("myorg/app:1", &hub_with_mirror());
        assert_eq!(candidates[0].image_ref, "mirror.gcr.io/myorg/app:1");
        assert_eq!(candidates[1].image_ref, "myorg/app:1");
    }

    #[test]
    fn private_mirror_with_public_registry_uses_mirror_credentials() {
        // A mirror can require auth while the registry itself is public.
        let state = registry_state(&RegistryWireConfig {
            default: Some("https://index.docker.io/v1/".into()),
            registries: vec![config(
                "https://index.docker.io/v1/",
                None,
                &[mirror("https://zot.example.com", Some("bWlycm9yOnBhc3M="))],
            )],
        });
        let candidates = pull_candidates("nginx:alpine", &state);
        assert_eq!(candidates[0].image_ref, "zot.example.com/library/nginx:alpine");
        assert_eq!(candidates[0].auth.as_deref(), Some("bWlycm9yOnBhc3M="));
        // Registry credentials never leak onto mirrors and vice versa.
        assert!(candidates[1].auth.is_none());
        assert_eq!(candidates[1].image_ref, "nginx:alpine");
    }

    #[test]
    fn nameless_digest_ref_gets_library_prefix_on_mirror() {
        let candidates = pull_candidates("nginx@sha256:abc123", &hub_with_mirror());
        assert_eq!(candidates[0].image_ref, "mirror.gcr.io/library/nginx@sha256:abc123");
    }

    #[test]
    fn qualified_hub_ref_rewrites_onto_mirror_but_keeps_main_spelling() {
        let state = hub_with_mirror();
        let candidates = pull_candidates("registry-1.docker.io/library/redis:7", &state);
        assert_eq!(candidates[0].image_ref, "mirror.gcr.io/library/redis:7");
        // Main candidate pulls under the user's spelling; hub aliases share credentials.
        assert_eq!(candidates[1].image_ref, "registry-1.docker.io/library/redis:7");
        assert_eq!(candidates[1].host, "docker.io");
        assert!(candidates[1].auth.is_some());
    }

    #[test]
    fn qualified_ref_on_other_configured_registry() {
        let state = registry_state(&RegistryWireConfig {
            default: None,
            registries: vec![config(
                "https://ghcr.io",
                Some("dXNlcjpwYXNz"),
                &[mirror("https://ghcr-mirror.example.com", None)],
            )],
        });
        let candidates = pull_candidates("ghcr.io/owner/app:v2", &state);
        assert_eq!(candidates[0].image_ref, "ghcr-mirror.example.com/owner/app:v2");
        assert_eq!(candidates[1].image_ref, "ghcr.io/owner/app:v2");
        assert!(candidates[1].auth.is_some());
    }

    #[test]
    fn unknown_registry_and_uploads_resolve_as_is() {
        let state = hub_with_mirror();
        let quay = pull_candidates("quay.io/prometheus/node-exporter:1", &state);
        assert_eq!(quay.len(), 1);
        assert_eq!(quay[0].image_ref, "quay.io/prometheus/node-exporter:1");
        assert!(quay[0].auth.is_none());

        let uploaded = pull_candidates("sha256:9592e22b", &state);
        assert_eq!(uploaded.len(), 1);
        assert_eq!(uploaded[0].image_ref, "sha256:9592e22b");
    }

    #[test]
    fn no_default_falls_back_to_docker_daemon_semantics() {
        let state = RegistryState::default();
        let candidates = pull_candidates("nginx:alpine", &state);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].image_ref, "nginx:alpine");
        assert_eq!(candidates[0].host, "docker.io");
    }

    #[test]
    fn non_hub_default_rewrites_bare_names() {
        let state = registry_state(&RegistryWireConfig {
            default: Some("https://zot.example.com/hub".into()),
            registries: vec![config("https://zot.example.com/hub", None, &[])],
        });
        let candidates = pull_candidates("nginx:alpine", &state);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].image_ref, "zot.example.com/hub/library/nginx:alpine");
        assert_eq!(candidates[0].host, "zot.example.com");

        let namespaced = pull_candidates("myorg/app:2", &state);
        assert_eq!(namespaced[0].image_ref, "zot.example.com/hub/myorg/app:2");
    }

    #[test]
    fn default_without_registry_entry_is_ignored() {
        let wire = RegistryWireConfig { default: Some("https://missing.example.com".into()), registries: vec![] };
        let state = registry_state(&wire);
        assert_eq!(state.default_host, None);
    }
}

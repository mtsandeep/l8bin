use dashmap::DashMap;
use std::sync::Arc;

use axum::http::StatusCode;

use litebin_common::types::Node;

use crate::config::Config;

/// Build a reqwest client configured for mTLS communication with a Worker Agent.
///
/// If cert paths are empty or the CA cert file does not exist, returns a plain
/// HTTP client suitable for development/local-only mode.
///
/// Hostname verification is disabled because agents are accessed by IP address,
/// and their certificates don't include IP SANs. CA verification is sufficient.
pub fn build_node_client(
    ca_cert_path: &str,
    client_cert_path: &str,
    client_key_path: &str,
) -> anyhow::Result<reqwest::Client> {
    // Dev mode: no certs configured
    if ca_cert_path.is_empty() || !std::path::Path::new(ca_cert_path).exists() {
        return Ok(reqwest::Client::new());
    }

    // Load CA cert into rustls root store
    let ca_cert_pem = std::fs::read(ca_cert_path)?;
    let mut root_store = rustls::RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut &ca_cert_pem[..]) {
        root_store.add(cert?)?;
    }

    // Load client identity (cert + key)
    let cert_pem = std::fs::read(client_cert_path)?;
    let key_pem = std::fs::read(client_key_path)?;
    let client_certs: Vec<_> = rustls_pemfile::certs(&mut &cert_pem[..]).collect::<Result<Vec<_>, _>>()?;
    let client_key = rustls_pemfile::ec_private_keys(&mut &key_pem[..])
        .next()
        .ok_or_else(|| anyhow::anyhow!("no EC private key found"))??;

    // Build rustls ClientConfig with hostname verification disabled
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoHostnameVerifier::new(root_store)))
        .with_client_auth_cert(client_certs, client_key.into())?;

    let client = reqwest::Client::builder()
        .use_preconfigured_tls(config)
        .timeout(std::time::Duration::from_secs(1800)) // 30 min for large image proxying
        .build()?;

    Ok(client)
}

/// Custom certificate verifier that verifies the cert chain against the trusted CA
/// but skips hostname verification. Agents are accessed by IP and certs don't have IP SANs.
#[derive(Debug)]
struct NoHostnameVerifier {
    root_store: rustls::RootCertStore,
}

impl NoHostnameVerifier {
    fn new(root_store: rustls::RootCertStore) -> Self {
        Self { root_store }
    }
}

impl rustls::client::danger::ServerCertVerifier for NoHostnameVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        use rustls::client::WebPkiServerVerifier;
        let verifier = WebPkiServerVerifier::builder(Arc::new(self.root_store.clone()))
            .build()
            .map_err(|_| rustls::Error::General("failed to build verifier".into()))?;
        // Verify cert chain but pass a dummy server name to skip hostname check.
        // Agent certs must include SAN=DNS:agent for this to work.
        let dummy_name = rustls::pki_types::ServerName::try_from("agent")
            .map_err(|_| rustls::Error::General("invalid server name".into()))?;
        verifier.verify_server_cert(end_entity, intermediates, &dummy_name, _ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        use rustls::crypto::verify_tls12_signature;
        verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        use rustls::crypto::verify_tls13_signature;
        verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes()
    }
}

/// Look up a node's HTTP client from the pool by node ID.
///
/// Returns an error if the node is not present in the pool.
pub fn get_node_client(
    pool: &DashMap<String, Arc<reqwest::Client>>,
    node_id: &str,
) -> anyhow::Result<Arc<reqwest::Client>> {
    pool.get(node_id)
        .map(|r| r.value().clone())
        .ok_or_else(|| anyhow::anyhow!("node '{}' not found in client pool", node_id))
}

/// Build the base URL for an agent node.
pub fn agent_base_url(config: &Config, node: &Node) -> String {
    if config.ca_cert_path.is_empty() {
        format!("http://{}:{}", node.host, node.agent_port)
    } else {
        format!("https://{}:{}", node.host, node.agent_port)
    }
}

/// Error from an agent HTTP call. Encodes the handler convention used across
/// the orchestrator: transport failure → 503, non-success agent response → 502
/// with the agent's error body, response parse failure → 500.
#[derive(Debug)]
pub enum AgentClientError {
    Transport(reqwest::Error),
    Status { code: StatusCode, body: String },
    Parse(String),
}

impl std::fmt::Display for AgentClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentClientError::Transport(e) => write!(f, "agent unreachable: {e}"),
            AgentClientError::Status { code, body } => write!(f, "agent error ({code}): {body}"),
            AgentClientError::Parse(e) => write!(f, "failed to parse agent response: {e}"),
        }
    }
}

impl std::error::Error for AgentClientError {}

impl AgentClientError {
    /// Map to the `(StatusCode, message)` handler-error convention.
    pub fn into_response_parts(self) -> (StatusCode, String) {
        match self {
            AgentClientError::Transport(e) => (StatusCode::SERVICE_UNAVAILABLE, format!("agent unreachable: {e}")),
            AgentClientError::Status { code, body } => {
                (StatusCode::BAD_GATEWAY, format!("agent error ({code}): {body}"))
            }
            AgentClientError::Parse(e) => {
                (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to parse agent response: {e}"))
            }
        }
    }
}

/// Typed client for an agent node's internal API.
///
/// One method per endpoint; requests and responses go through the shared
/// `litebin_common::agent_api` DTOs, so both sides of the wire compile against
/// the same types.
#[derive(Clone)]
pub struct AgentClient {
    client: Arc<reqwest::Client>,
    base_url: String,
}

impl AgentClient {
    pub fn new(client: Arc<reqwest::Client>, node: &Node, config: &Config) -> Self {
        Self { client, base_url: agent_base_url(config, node) }
    }

    /// DB lookup + client pool lookup + base URL in one step.
    pub async fn resolve(state: &crate::AppState, node_id: &str) -> anyhow::Result<Self> {
        let node: Node = sqlx::query_as("SELECT * FROM nodes WHERE id = ?")
            .bind(node_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| anyhow::anyhow!("db error: {e}"))?
            .ok_or_else(|| anyhow::anyhow!("node '{}' not found", node_id))?;
        let client = get_node_client(&state.node_clients, node_id)?;
        Ok(Self::new(client, &node, &state.config))
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    async fn decode<T: serde::de::DeserializeOwned>(resp: reqwest::Response) -> Result<T, AgentClientError> {
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentClientError::Status { code: status, body });
        }
        resp.json().await.map_err(|e| AgentClientError::Parse(e.to_string()))
    }

    async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &impl serde::Serialize,
    ) -> Result<T, AgentClientError> {
        let resp = self.client.post(self.url(path)).json(body).send().await.map_err(AgentClientError::Transport)?;
        Self::decode(resp).await
    }

    async fn post_status(&self, path: &str, body: &impl serde::Serialize) -> Result<(), AgentClientError> {
        let resp = self.client.post(self.url(path)).json(body).send().await.map_err(AgentClientError::Transport)?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentClientError::Status { code: status, body });
        }
        Ok(())
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: String) -> Result<T, AgentClientError> {
        let resp = self.client.get(url).send().await.map_err(AgentClientError::Transport)?;
        Self::decode(resp).await
    }

    /// POST /containers/batch-run — deploy/start/recreate a compose project's services.
    pub async fn batch_run(
        &self,
        req: &litebin_common::agent_api::BatchRunRequest,
    ) -> Result<litebin_common::agent_api::BatchRunResponse, AgentClientError> {
        self.post_json(litebin_common::agent_api::BATCH_RUN_PATH, req).await
    }

    /// POST /containers/run — create + start a single-service container.
    pub async fn run(
        &self,
        req: &litebin_common::agent_api::RunRequest,
    ) -> Result<litebin_common::agent_api::RunResponse, AgentClientError> {
        self.post_json(litebin_common::agent_api::RUN_PATH, req).await
    }

    /// POST /containers/recreate — remove + re-create a single-service container.
    pub async fn recreate(
        &self,
        req: &litebin_common::agent_api::RunRequest,
    ) -> Result<litebin_common::agent_api::RunResponse, AgentClientError> {
        self.post_json(litebin_common::agent_api::RECREATE_PATH, req).await
    }

    /// POST /containers/start — start an existing container (with env-change recreate inputs).
    pub async fn start(
        &self,
        req: &litebin_common::agent_api::StartRequest,
    ) -> Result<litebin_common::agent_api::StartResponse, AgentClientError> {
        self.post_json(litebin_common::agent_api::START_PATH, req).await
    }

    /// POST /containers/stop — stop a container (agent answers 200 with empty body).
    pub async fn stop(&self, req: &litebin_common::agent_api::StopRequest) -> Result<(), AgentClientError> {
        self.post_status(litebin_common::agent_api::STOP_PATH, req).await
    }

    /// POST /containers/stop-project — stop every container of a project.
    pub async fn stop_project(
        &self,
        req: &litebin_common::agent_api::StopProjectRequest,
    ) -> Result<litebin_common::agent_api::StopProjectResponse, AgentClientError> {
        self.post_json(litebin_common::agent_api::STOP_PROJECT_PATH, req).await
    }

    /// POST /containers/stop-service — stop one service of a multi-service project.
    pub async fn stop_service(
        &self,
        req: &litebin_common::agent_api::StopServiceRequest,
    ) -> Result<litebin_common::agent_api::StopServiceResponse, AgentClientError> {
        self.post_json(litebin_common::agent_api::STOP_SERVICE_PATH, req).await
    }

    /// POST /containers/remove — remove a container (best-effort callers ignore failures).
    pub async fn remove(&self, req: &litebin_common::agent_api::RemoveRequest) -> Result<(), AgentClientError> {
        self.post_status(litebin_common::agent_api::REMOVE_PATH, req).await
    }

    /// POST /containers/cleanup — remove all project containers, network, and volumes.
    pub async fn cleanup(&self, req: &litebin_common::agent_api::CleanupRequest) -> Result<(), AgentClientError> {
        self.post_status(litebin_common::agent_api::CLEANUP_PATH, req).await
    }

    /// GET /health — agent liveness + host capability report.
    pub async fn health(&self) -> Result<litebin_common::types::HealthReport, AgentClientError> {
        self.get_json(self.url("/health")).await
    }

    /// GET /health with a per-request timeout (used by fan-out calls that must not hang).
    pub async fn health_within(
        &self,
        timeout: std::time::Duration,
    ) -> Result<litebin_common::types::HealthReport, AgentClientError> {
        let resp =
            self.client.get(self.url("/health")).timeout(timeout).send().await.map_err(AgentClientError::Transport)?;
        Self::decode(resp).await
    }

    /// POST /internal/register — push master config (domain, report URLs) to the agent.
    pub async fn register(&self, req: &litebin_common::agent_api::RegisterRequest) -> Result<(), AgentClientError> {
        self.post_status(litebin_common::types::AGENT_REGISTER_PATH, req).await
    }

    /// POST /internal/project-meta — replace the agent's project lifecycle/capability map.
    pub async fn push_project_meta(
        &self,
        req: &litebin_common::agent_api::ProjectMetaRequest,
    ) -> Result<(), AgentClientError> {
        self.post_status(litebin_common::agent_api::PROJECT_META_PATH, req).await
    }

    /// POST /caddy/sync — push a Caddy JSON config to the agent (raw by design).
    pub async fn caddy_sync(&self, config: &serde_json::Value) -> Result<(), AgentClientError> {
        let resp = self
            .client
            .post(self.url(litebin_common::agent_api::CADDY_SYNC_PATH))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(config)
            .send()
            .await
            .map_err(AgentClientError::Transport)?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentClientError::Status { code: status, body });
        }
        Ok(())
    }

    /// GET /containers/{id}/status — lightweight container state probe.
    pub async fn container_status(
        &self,
        container_id: &str,
    ) -> Result<litebin_common::types::ContainerStatus, AgentClientError> {
        self.get_json(self.url(&format!("/containers/{container_id}/status"))).await
    }

    /// GET /containers/{id}/logs — plain-text log tail.
    pub async fn container_logs(&self, container_id: &str, tail: Option<usize>) -> Result<String, AgentClientError> {
        let mut request = self.client.get(self.url(&format!("/containers/{container_id}/logs")));
        if let Some(tail) = tail {
            request = request.query(&[("tail", tail)]);
        }
        let resp = request.send().await.map_err(AgentClientError::Transport)?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentClientError::Status { code: status, body });
        }
        resp.text().await.map_err(AgentClientError::Transport)
    }

    /// GET /containers/{id}/disk-usage.
    pub async fn disk_usage(&self, container_id: &str) -> Result<litebin_common::docker::DiskUsage, AgentClientError> {
        self.get_json(self.url(&format!("/containers/{container_id}/disk-usage"))).await
    }

    /// POST /containers/stats — batch stats for the given container IDs.
    pub async fn stats(
        &self,
        req: &litebin_common::agent_api::BatchStatsRequest,
    ) -> Result<Vec<litebin_common::agent_api::ContainerStatsResponse>, AgentClientError> {
        self.post_json(litebin_common::agent_api::CONTAINER_STATS_PATH, req).await
    }

    /// GET /containers/scan — foreign container groups present on the node.
    pub async fn scan(&self) -> Result<Vec<litebin_common::scan::ScanGroup>, AgentClientError> {
        self.get_json(self.url(litebin_common::agent_api::CONTAINER_SCAN_PATH)).await
    }

    /// POST /containers/import — adopt foreign containers into a project.
    pub async fn import(
        &self,
        req: &litebin_common::agent_api::ImportRequest,
    ) -> Result<litebin_common::agent_api::ImportResponse, AgentClientError> {
        self.post_json(litebin_common::agent_api::CONTAINER_IMPORT_PATH, req).await
    }

    /// GET /containers/compose-file?dir= — read a compose file + .env from a project dir.
    pub async fn compose_file(
        &self,
        dir: &str,
    ) -> Result<litebin_common::agent_api::ComposeFileResponse, AgentClientError> {
        let resp = self
            .client
            .get(self.url(litebin_common::agent_api::CONTAINER_COMPOSE_FILE_PATH))
            .query(&[("dir", dir)])
            .send()
            .await
            .map_err(AgentClientError::Transport)?;
        Self::decode(resp).await
    }

    /// POST /images/load?image_id= — stream a docker-save tar to the agent.
    pub async fn load_image_stream(
        &self,
        image_id: &str,
        body: reqwest::Body,
    ) -> Result<litebin_common::agent_api::LoadImageResponse, AgentClientError> {
        let resp = self
            .client
            .post(self.url(litebin_common::agent_api::IMAGES_LOAD_PATH))
            .query(&[("image_id", image_id)])
            .header(reqwest::header::CONTENT_TYPE, "application/x-tar")
            .body(body)
            .send()
            .await
            .map_err(AgentClientError::Transport)?;
        Self::decode(resp).await
    }

    /// POST /internal/mint-upload-token — mint a scoped upload token for direct-to-agent transfer.
    pub async fn mint_upload_token(
        &self,
        req: &litebin_common::upload::MintRequest,
    ) -> Result<litebin_common::upload::MintResponse, AgentClientError> {
        self.post_json(litebin_common::upload::MINT_PATH, req).await
    }

    /// GET /images/inspect?image= — resolve an image reference to its local digest.
    pub async fn inspect_image(
        &self,
        image: &str,
    ) -> Result<litebin_common::agent_api::InspectResponse, AgentClientError> {
        let resp = self
            .client
            .get(self.url(litebin_common::agent_api::IMAGES_INSPECT_PATH))
            .query(&[("image", image)])
            .send()
            .await
            .map_err(AgentClientError::Transport)?;
        Self::decode(resp).await
    }

    /// POST /images/remove-unused — delete an image by digest if no container uses it.
    pub async fn remove_unused_image(
        &self,
        req: &litebin_common::agent_api::RemoveImageRequest,
    ) -> Result<litebin_common::agent_api::RemoveImageResponse, AgentClientError> {
        self.post_json(litebin_common::agent_api::IMAGES_REMOVE_UNUSED_PATH, req).await
    }

    /// POST /images/prune — prune dangling images; returns the agent's raw response text
    /// (the dashboard surfaces it verbatim).
    pub async fn prune_images(&self) -> Result<String, AgentClientError> {
        let resp = self
            .client
            .post(self.url(litebin_common::agent_api::IMAGES_PRUNE_PATH))
            .send()
            .await
            .map_err(AgentClientError::Transport)?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentClientError::Status { code: status, body });
        }
        resp.text().await.map_err(AgentClientError::Transport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dashmap::DashMap;
    use std::sync::Arc;

    #[test]
    fn prop_node_client_pool_lifecycle() {
        let pool: DashMap<String, Arc<reqwest::Client>> = DashMap::new();
        let node_id = "test-node-1";

        // Insert a node
        let client = Arc::new(reqwest::Client::new());
        pool.insert(node_id.to_string(), client);

        // Assert it's present
        assert!(get_node_client(&pool, node_id).is_ok());

        // Remove it
        pool.remove(node_id);

        // Assert it's absent
        assert!(get_node_client(&pool, node_id).is_err());
    }
}

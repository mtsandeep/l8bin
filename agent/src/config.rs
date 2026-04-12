use anyhow::Result;
use serde::{Deserialize, Serialize};

pub struct Config {
    pub agent_port: u16,
    pub cert_path: String,
    pub key_path: String,
    pub ca_cert_path: String,
    pub public_ip: String,
    pub caddy_admin_url: String,
    /// Cert PEM content read at startup for embedding in Caddy JSON config.
    pub cert_pem: String,
    /// Key PEM content read at startup for embedding in Caddy JSON config.
    pub key_pem: String,
}

/// Registration data pushed by the orchestrator over mTLS.
/// Persisted to `data/agent-state.json` for restarts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentRegistration {
    pub node_id: String,
    pub secret: String,
    pub domain: String,
    pub wake_report_url: String,
    pub heartbeat_url: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let cert_path = std::env::var("AGENT_CERT_PATH")
            .unwrap_or_else(|_| "/etc/litebin/certs/agent.pem".to_string());
        let key_path = std::env::var("AGENT_KEY_PATH")
            .unwrap_or_else(|_| "/etc/litebin/certs/agent-key.pem".to_string());

        let cert_pem = std::fs::read_to_string(&cert_path)
            .map_err(|e| anyhow::anyhow!("failed to read cert {}: {}", cert_path, e))?;
        let key_pem = std::fs::read_to_string(&key_path)
            .map_err(|e| anyhow::anyhow!("failed to read key {}: {}", key_path, e))?;

        Ok(Config {
            agent_port: std::env::var("AGENT_PORT")
                .unwrap_or_else(|_| "8443".to_string())
                .parse()?,
            cert_path,
            key_path,
            ca_cert_path: std::env::var("AGENT_CA_CERT_PATH")
                .unwrap_or_else(|_| "/etc/litebin/certs/ca.pem".to_string()),
            public_ip: std::env::var("AGENT_PUBLIC_IP").unwrap_or_default(),
            caddy_admin_url: std::env::var("AGENT_CADDY_ADMIN_URL")
                .unwrap_or_else(|_| "http://localhost:2019".to_string()),
            cert_pem,
            key_pem,
        })
    }
}

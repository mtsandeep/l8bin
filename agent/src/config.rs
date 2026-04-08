use anyhow::Result;
use serde::{Deserialize, Serialize};

pub struct Config {
    pub agent_port: u16,
    pub cert_path: String,
    pub key_path: String,
    pub ca_cert_path: String,
    pub public_ip: String,
    pub caddy_admin_url: String,
}

/// Registration data pushed by the orchestrator over mTLS.
/// Persisted to `data/agent-state.json` for restarts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentRegistration {
    pub node_id: String,
    pub secret: String,
    pub domain: String,
    pub wake_report_url: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Config {
            agent_port: std::env::var("AGENT_PORT")
                .unwrap_or_else(|_| "8443".to_string())
                .parse()?,
            cert_path: std::env::var("AGENT_CERT_PATH")
                .unwrap_or_else(|_| "/etc/litebin/certs/agent.pem".to_string()),
            key_path: std::env::var("AGENT_KEY_PATH")
                .unwrap_or_else(|_| "/etc/litebin/certs/agent-key.pem".to_string()),
            ca_cert_path: std::env::var("AGENT_CA_CERT_PATH")
                .unwrap_or_else(|_| "/etc/litebin/certs/ca.pem".to_string()),
            public_ip: std::env::var("AGENT_PUBLIC_IP").unwrap_or_default(),
            caddy_admin_url: std::env::var("AGENT_CADDY_ADMIN_URL")
                .unwrap_or_else(|_| "http://localhost:2019".to_string()),
        })
    }
}

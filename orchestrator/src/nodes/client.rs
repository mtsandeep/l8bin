use dashmap::DashMap;
use std::sync::Arc;

/// Build a reqwest client configured for mTLS communication with a Worker Agent.
///
/// If cert paths are empty or the CA cert file does not exist, returns a plain
/// HTTP client suitable for development/local-only mode.
pub fn build_node_client(
    ca_cert_path: &str,
    client_cert_path: &str,
    client_key_path: &str,
) -> anyhow::Result<reqwest::Client> {
    // Dev mode: no certs configured
    if ca_cert_path.is_empty() || !std::path::Path::new(ca_cert_path).exists() {
        return Ok(reqwest::Client::new());
    }

    // Load CA cert
    let ca_cert_pem = std::fs::read(ca_cert_path)?;
    let ca_cert = reqwest::Certificate::from_pem(&ca_cert_pem)?;

    // Load client identity (cert + key combined)
    let cert_pem = std::fs::read(client_cert_path)?;
    let key_pem = std::fs::read(client_key_path)?;
    let mut combined = cert_pem;
    combined.extend_from_slice(&key_pem);
    let identity = reqwest::Identity::from_pem(&combined)?;

    let client = reqwest::Client::builder()
        .add_root_certificate(ca_cert)
        .identity(identity)
        .use_rustls_tls()
        .build()?;

    Ok(client)
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

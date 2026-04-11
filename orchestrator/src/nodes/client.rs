use dashmap::DashMap;
use std::sync::Arc;

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
    let client_certs: Vec<_> = rustls_pemfile::certs(&mut &cert_pem[..])
        .collect::<Result<Vec<_>, _>>()?;
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
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
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

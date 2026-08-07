//! TLS client for direct-to-agent uploads.
//!
//! Direct uploads connect to the agent by raw IP on `:443`; the agent serves a
//! cert signed by the litebin CA with `SAN=DNS:agent` (no IP SAN). So — exactly
//! like the master does (`orchestrator/src/nodes/client.rs`) — we trust the CA
//! and skip hostname verification. The agent CA PEM is handed to the client by
//! the master's `/images/upload-target` broker.

use std::sync::Arc;

use anyhow::{Context, Result};

/// Build a reqwest client that trusts the given CA PEM and skips hostname
/// verification (agent certs carry `SAN=DNS:agent`, accessed by IP).
pub fn direct_upload_client(ca_pem: &str) -> Result<reqwest::Client> {
    // Ensure the ring crypto provider is installed (CLI doesn't install one globally).
    let _ = rustls::crypto::ring::default_provider().install_default();

    let mut root_store = rustls::RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut ca_pem.as_bytes()) {
        root_store.add(cert.context("invalid CA cert in ca_pem")?)?;
    }

    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoHostnameVerifier::new(root_store)))
        .with_no_client_auth();

    let client = reqwest::Client::builder()
        .use_preconfigured_tls(config)
        .timeout(std::time::Duration::from_secs(1800))
        .build()?;

    Ok(client)
}

/// Verify the server cert against the trusted CA but skip the hostname check.
/// Mirrors `orchestrator/src/nodes/client.rs::NoHostnameVerifier`.
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
        end_entity: &rustls::pki_types::CertificateDer,
        intermediates: &[rustls::pki_types::CertificateDer],
        _server_name: &rustls::pki_types::ServerName,
        _ocsp_response: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        use rustls::client::WebPkiServerVerifier;
        let verifier = WebPkiServerVerifier::builder(Arc::new(self.root_store.clone()))
            .build()
            .map_err(|_| rustls::Error::General("failed to build verifier".into()))?;
        // Verify the chain but pass a dummy server name to skip the hostname check.
        // Agent certs include SAN=DNS:agent for this to work.
        let dummy_name =
            rustls::pki_types::ServerName::try_from("agent").map_err(|_| rustls::Error::General("invalid server name".into()))?;
        verifier.verify_server_cert(end_entity, intermediates, &dummy_name, _ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
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
        cert: &rustls::pki_types::CertificateDer,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
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

use anyhow::{Context, Result};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls_pemfile::{certs, ec_private_keys};
use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;

pub fn build_server_tls_config(
    cert_path: &str,
    key_path: &str,
    ca_cert_path: &str,
) -> Result<ServerConfig> {
    // Load CA cert for client verification
    let ca_certs: Vec<CertificateDer<'static>> = {
        let f = File::open(ca_cert_path)
            .with_context(|| format!("failed to open CA cert: {ca_cert_path}"))?;
        certs(&mut BufReader::new(f))
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to parse CA cert")?
    };

    let mut root_store = rustls::RootCertStore::empty();
    for cert in ca_certs {
        root_store
            .add(cert)
            .context("failed to add CA cert to root store")?;
    }

    // Build client verifier requiring cert signed by Root CA
    let client_verifier = WebPkiClientVerifier::builder(Arc::new(root_store))
        .build()
        .context("failed to build client verifier")?;

    // Load server cert chain
    let server_certs: Vec<CertificateDer<'static>> = {
        let f = File::open(cert_path)
            .with_context(|| format!("failed to open server cert: {cert_path}"))?;
        certs(&mut BufReader::new(f))
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to parse server cert")?
    };

    // Load server private key (SEC1/EC format from openssl ecparam)
    let server_key: PrivateKeyDer<'static> = {
        let f = File::open(key_path)
            .with_context(|| format!("failed to open server key: {key_path}"))?;
        ec_private_keys(&mut BufReader::new(f))
            .next()
            .context("no EC private key found")?
            .context("failed to parse private key")?
            .into()
    };

    let config = ServerConfig::builder()
        .with_client_cert_verifier(client_verifier)
        .with_single_cert(server_certs, server_key)
        .context("failed to build ServerConfig")?;

    Ok(config)
}

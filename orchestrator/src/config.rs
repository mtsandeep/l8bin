use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub domain: String,
    pub caddy_admin_url: String,
    pub database_url: String,
    pub docker_network: String,
    pub host: String,
    pub port: u16,
    pub default_auto_stop_mins: i64,
    pub janitor_interval_secs: u64,
    pub flush_interval_secs: u64,
    pub ca_cert_path: String,
    pub client_cert_path: String,
    pub client_key_path: String,
    pub heartbeat_interval_secs: u64,
    pub public_ip: String,
    pub routing_mode: String,
    pub cloudflare_api_token: String,
    pub cloudflare_zone_id: String,
    pub dashboard_subdomain: String,
    pub poke_subdomain: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            domain: env::var("DOMAIN").unwrap_or_else(|_| "localhost".into()),
            caddy_admin_url: env::var("CADDY_ADMIN_URL")
                .unwrap_or_else(|_| "http://localhost:2019".into()),
            database_url: env::var("DATABASE_URL")
                .unwrap_or_else(|_| "sqlite:./data/litebin.db".into()),
            docker_network: env::var("DOCKER_NETWORK")
                .unwrap_or_else(|_| "litebin-network".into()),
            host: env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into()),
            port: env::var("PORT")
                .unwrap_or_else(|_| "5080".into())
                .parse()?,
            default_auto_stop_mins: env::var("DEFAULT_AUTO_STOP_MINS")
                .unwrap_or_else(|_| "15".into())
                .parse()?,
            janitor_interval_secs: env::var("JANITOR_INTERVAL_SECS")
                .unwrap_or_else(|_| "300".into()) // 5 minutes
                .parse()?,
            flush_interval_secs: env::var("FLUSH_INTERVAL_SECS")
                .unwrap_or_else(|_| "60".into())
                .parse()?,
            ca_cert_path: env::var("MASTER_CA_CERT_PATH").unwrap_or_default(),
            client_cert_path: env::var("MASTER_CLIENT_CERT_PATH").unwrap_or_default(),
            client_key_path: env::var("MASTER_CLIENT_KEY_PATH").unwrap_or_default(),
            heartbeat_interval_secs: env::var("HEARTBEAT_INTERVAL_SECS")
                .unwrap_or_else(|_| "30".into())
                .parse()?,
            public_ip: env::var("PUBLIC_IP").unwrap_or_default(),
            routing_mode: env::var("ROUTING_MODE")
                .unwrap_or_else(|_| "master_proxy".into()),
            cloudflare_api_token: env::var("CLOUDFLARE_API_TOKEN").unwrap_or_default(),
            cloudflare_zone_id: env::var("CLOUDFLARE_ZONE_ID").unwrap_or_default(),
            dashboard_subdomain: env::var("DASHBOARD_SUBDOMAIN")
                .unwrap_or_else(|_| "l8bin".into()),
            poke_subdomain: env::var("POKE_SUBDOMAIN")
                .unwrap_or_else(|_| "poke".into()),
        })
    }
}

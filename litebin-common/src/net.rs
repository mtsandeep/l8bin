/// Detect the host's public IP address by querying external services.
/// Tries multiple providers in order — returns the first successful result.
/// Each provider gets 3s total before we move to the next.
/// Returns `None` only if all providers fail.
pub async fn detect_public_ip() -> Option<String> {
    let providers: &[&str] = &[
        "https://api.ipify.org",
        "https://checkip.amazonaws.com",
        "https://ipv4.icanhazip.com",
        "https://v4.ident.me",
    ];

    for url in providers {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(2))
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .ok()?;

        if let Some(ip) = try_fetch_ip(&client, url).await {
            return Some(ip);
        }
    }

    None
}

async fn try_fetch_ip(client: &reqwest::Client, url: &str) -> Option<String> {
    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let ip = resp.text().await.ok()?;
    let ip = ip.trim().to_string();
    if ip.parse::<std::net::Ipv4Addr>().is_ok() {
        Some(ip)
    } else {
        None
    }
}

use std::collections::{HashMap, HashSet};

use litebin_common::routing::ProjectRoute;

use super::CloudflareDnsRouter;

/// Desired DNS records: name → ip. The `public_ip` is the resolved
/// dns_target setting (fallback: PUBLIC_IP env); records needing it are
/// skipped when empty.
pub(crate) async fn desired_dns_records(
    db: &sqlx::SqlitePool,
    public_ip: &str,
    domain: &str,
    dashboard_subdomain: &str,
    poke_subdomain: &str,
) -> HashMap<String, String> {
    let mut desired: HashMap<String, String> = HashMap::new(); // name → ip

    if !public_ip.is_empty() {
        desired.insert(format!("{}.{}", dashboard_subdomain, domain), public_ip.to_string());
        desired.insert(format!("{}.{}", poke_subdomain, domain), public_ip.to_string());
    }

    let all_projects: Vec<(String, Option<String>, Option<String>, bool)> =
        match sqlx::query_as("SELECT id, node_id, custom_domain, is_background FROM projects").fetch_all(db).await {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, "desired DNS records: failed to fetch projects");
                return HashMap::new();
            }
        };

    for (project_id, node_id, custom_domain, is_background) in &all_projects {
        let ip = match (*is_background, node_id.as_deref()) {
            (true, _) => {
                if public_ip.is_empty() {
                    continue;
                }
                public_ip.to_string()
            }
            (false, Some(nid)) if nid != "local" => {
                let row: Option<(Option<String>,)> = match sqlx::query_as("SELECT public_ip FROM nodes WHERE id = ?")
                    .bind(nid)
                    .fetch_optional(db)
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(project_id = %project_id, node_id = %nid, error = %e, "sync_dns: failed to fetch node public_ip");
                        None
                    }
                };
                match row.and_then(|(ip,)| ip) {
                    Some(ip) if !ip.is_empty() => ip,
                    _ => {
                        tracing::warn!(
                            project_id = %project_id,
                            node_id = %nid,
                            "skipping DNS record — remote node has no public_ip"
                        );
                        continue;
                    }
                }
            }
            _ => {
                if public_ip.is_empty() {
                    continue;
                }
                public_ip.to_string()
            }
        };

        // Subdomain A record
        desired.insert(format!("{}.{}", project_id, domain), ip.clone());

        // Custom domain A record (if any)
        if !*is_background {
            if let Some(cd) = custom_domain {
                desired.insert(cd.clone(), ip.clone());

                // Also add the www variant as a redirect handled by Caddy,
                // but we still need a DNS record pointing to the same IP
                let www = match cd.strip_prefix("www.") {
                    Some(rest) => rest.to_string(),
                    None => format!("www.{}", cd),
                };
                desired.insert(www, ip.clone());
            }

            // Custom alias/subdomain routes (project_routes): each generated
            // hostname gets a Caddy route but also needs an A record to resolve.
            // Mirrors the hosts built in build_master_caddy_config/build_agent_caddy_config.
            let alias_rows: Vec<(String, Option<String>)> = match sqlx::query_as(
                "SELECT route_type, subdomain FROM project_routes WHERE project_id = ? AND subdomain IS NOT NULL AND subdomain != ''",
            )
            .bind(project_id)
            .fetch_all(db)
            .await
            {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(project_id = %project_id, error = %e, "sync_dns: failed to fetch project_routes");
                        vec![]
                    }
                };
            for (route_type, sub) in alias_rows {
                if let Some(alias) = sub {
                    if route_type == "alias" {
                        // {alias}.{domain} — short alias
                        desired.insert(format!("{}.{}", alias, domain), ip.clone());
                    } else {
                        // {alias}.{project}.{domain} — namespaced subdomain
                        desired.insert(format!("{}.{}.{}", alias, project_id, domain), ip.clone());
                    }
                }
            }
        }
    }
    desired
}

impl CloudflareDnsRouter {
    /// Sync Cloudflare DNS records: upsert for all projects, delete stale ones.
    /// DNS records are kept for all projects regardless of status (running, stopped, etc.)
    /// so that stopped projects still resolve and hit the waker.
    pub(super) async fn sync_dns(
        &self,
        _projects: &[ProjectRoute],
        domain: &str,
        dashboard_subdomain: &str,
        poke_subdomain: &str,
    ) -> anyhow::Result<litebin_common::routing::DnsSyncResult> {
        // dns_target setting overrides the PUBLIC_IP env for DNS records.
        let public_ip = resolved_dns_ip(&self.db, &self.config.public_ip).await;
        let desired = desired_dns_records(&self.db, &public_ip, domain, dashboard_subdomain, poke_subdomain).await;

        // Resolve the zone per name: platform names → configured zone,
        // custom domains → their own zone in the account.
        let configured_zone_name = self.cloudflare.get_zone_name().await?;
        let account_zones = match self.cloudflare.list_zones().await {
            Ok(z) => z,
            Err(e) => {
                tracing::warn!(error = %e, "sync_dns: failed to list account zones; custom-domain records outside the configured zone are skipped");
                Vec::new()
            }
        };
        let zone_of = |name: &str| -> Option<String> {
            zone_for_name(name, self.cloudflare.zone_id(), &configured_zone_name, &account_zones).map(str::to_string)
        };

        // Existing records per involved zone — ledger backfill + IP drift detection.
        let mut existing: Vec<litebin_common::cloudflare::DnsRecord> =
            self.cloudflare.list_records_by_suffix(&format!(".{}", domain), "A").await?;
        let mut zone_names: HashMap<String, String> =
            account_zones.iter().map(|z| (z.id.clone(), z.name.clone())).collect();
        zone_names.insert(self.cloudflare.zone_id().to_string(), configured_zone_name.clone());
        let mut listed_zones: HashSet<String> = HashSet::new();
        listed_zones.insert(self.cloudflare.zone_id().to_string());
        for name in desired.keys() {
            if let Some(zone) = zone_of(name)
                && listed_zones.insert(zone.clone())
                && let Some(zone_name) = zone_names.get(&zone)
                && let Ok(records) =
                    self.cloudflare.list_records_by_suffix_in_zone(&zone, &format!(".{zone_name}"), "A").await
            {
                existing.extend(records);
            }
        }

        let mut result = litebin_common::routing::DnsSyncResult::default();

        // Delete stale records — ledger rows only (user-created records never are).
        let ledger_rows = crate::dns_ledger::rows(&self.db).await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "sync_dns: failed to load DNS record ledger; skipping stale deletion");
            Vec::new()
        });
        let desired_names: HashSet<&str> = desired.keys().map(|s| s.as_str()).collect();
        for row in crate::dns_ledger::stale(&ledger_rows, &desired_names) {
            let zone = if row.zone_id.is_empty() { self.cloudflare.zone_id() } else { &row.zone_id };
            if let Err(e) = self.cloudflare.delete_record_in_zone(zone, &row.record_id).await {
                tracing::warn!(record = %row.name, error = %e, "failed to delete stale DNS record");
                result.errors += 1;
                continue;
            }
            if let Err(e) = crate::dns_ledger::remove(&self.db, &row.name).await {
                tracing::warn!(record = %row.name, error = %e, "failed to drop stale DNS record from ledger");
            }
            result.deleted += 1;
        }

        // Upsert desired records, recording every managed name in the ledger
        for (name, ip) in &desired {
            let Some(zone) = zone_of(name) else {
                if is_platform_name(name, domain) {
                    // Platform names must be manageable — zone misconfigured.
                    tracing::warn!(name, "failed to upsert DNS record: no Cloudflare zone covers this name");
                    result.errors += 1;
                } else {
                    // Custom domain outside the accessible account — A record is managed manually.
                    tracing::info!(
                        name,
                        "custom domain not on the configured Cloudflare account; add its A record manually"
                    );
                }
                continue;
            };
            if let Some(record) = existing.iter().find(|r| r.name == name.as_str()) {
                record_in_ledger(&self.db, &ledger_rows, name, &record.id, &zone).await;
                if &record.content == ip {
                    result.unchanged += 1;
                    continue;
                }
            }
            match self.cloudflare.upsert_record_in_zone(&zone, name, "A", ip, 1, false).await {
                Ok(upserted) => {
                    if upserted.created {
                        result.created += 1;
                    } else {
                        result.repointed += 1;
                    }
                    record_in_ledger(&self.db, &ledger_rows, name, &upserted.id, &zone).await;
                }
                Err(e) => {
                    tracing::warn!(name, ip, error = %e, "failed to upsert DNS record");
                    result.errors += 1;
                }
            }
        }

        tracing::info!(
            created = result.created,
            repointed = result.repointed,
            unchanged = result.unchanged,
            deleted = result.deleted,
            errors = result.errors,
            desired = desired.len(),
            existing = existing.len(),
            "DNS sync complete"
        );

        Ok(result)
    }
}

fn is_platform_name(name: &str, domain: &str) -> bool {
    name == domain || name.ends_with(&format!(".{domain}"))
}

/// dns_target setting when set, else the env/detected PUBLIC_IP.
pub(crate) async fn resolved_dns_ip(db: &sqlx::SqlitePool, env_ip: &str) -> String {
    let target: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind("dns_target")
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    target.filter(|v| !v.is_empty()).unwrap_or_else(|| env_ip.to_string())
}

/// The zone managing `name`: the configured zone when it covers the name,
/// else the longest-covering account zone, else None.
pub(crate) fn zone_for_name<'a>(
    name: &str,
    configured_zone_id: &'a str,
    configured_zone_name: &str,
    account_zones: &'a [litebin_common::cloudflare::Zone],
) -> Option<&'a str> {
    if litebin_common::cloudflare::CloudflareClient::zone_covers_hostname(configured_zone_name, name) {
        return Some(configured_zone_id);
    }
    account_zones
        .iter()
        .filter(|z| litebin_common::cloudflare::CloudflareClient::zone_covers_hostname(&z.name, name))
        .max_by_key(|z| z.name.len())
        .map(|z| z.id.as_str())
}

/// Remember a managed record in the ledger (no-op when already recorded).
async fn record_in_ledger(
    db: &sqlx::SqlitePool,
    ledger_rows: &[crate::dns_ledger::LedgerRow],
    name: &str,
    id: &str,
    zone: &str,
) {
    if ledger_rows.iter().any(|r| r.name == name && r.record_id == id) {
        return;
    }
    if let Err(e) = crate::dns_ledger::upsert(db, name, id, zone).await {
        tracing::warn!(record = name, error = %e, "failed to record DNS record in ledger");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zone(id: &str, name: &str) -> litebin_common::cloudflare::Zone {
        litebin_common::cloudflare::Zone { id: id.into(), name: name.into() }
    }

    #[test]
    fn platform_names_map_to_configured_zone() {
        let zones = vec![zone("z2", "example.org")];
        assert_eq!(zone_for_name("dash.example.com", "z1", "example.com", &zones), Some("z1"));
    }

    #[test]
    fn custom_domains_map_to_their_own_zone() {
        let zones = vec![zone("z1", "example.com"), zone("z2", "example.org")];
        assert_eq!(zone_for_name("app.example.org", "z1", "example.com", &zones), Some("z2"));
    }

    #[test]
    fn longest_covering_zone_wins() {
        let zones = vec![zone("z2", "example.org"), zone("z3", "sub.example.org")];
        assert_eq!(zone_for_name("a.sub.example.org", "z1", "example.com", &zones), Some("z3"));
    }

    #[test]
    fn unknown_zone_is_none() {
        assert_eq!(zone_for_name("app.unknown.example", "z1", "example.com", &[]), None);
    }

    #[tokio::test]
    async fn desired_records_cover_platform_custom_and_alias_names() {
        let db = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("src/db/migrations").run(&db).await.unwrap();
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO users (id, username, password_hash, created_at, updated_at) VALUES ('u1', 'u1', 'x', ?, ?)",
        )
        .bind(now)
        .bind(now)
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO nodes (id, name, host, created_at, updated_at) VALUES ('local', 'local', '127.0.0.1', ?, ?)",
        )
        .bind(now)
        .bind(now)
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO projects (id, user_id, name, status, node_id, deploy_type, is_background,              auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled, allow_raw_ports,              allow_docker_access, custom_domain, created_at, updated_at)              VALUES ('app', 'u1', 'App', 'running', 'local', 'compose', 0, 1, 15, 0, 0, 0, 'app.example.org', ?, ?)",
        )
        .bind(now)
        .bind(now)
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO project_routes (id, project_id, route_type, subdomain, upstream, created_at)              VALUES ('r1', 'app', 'alias', 'short', 'http://app', ?)",
        )
        .bind(now)
        .execute(&db)
        .await
        .unwrap();

        let desired = desired_dns_records(&db, "1.2.3.4", "example.com", "dash", "poke").await;
        for name in [
            "dash.example.com",
            "poke.example.com",
            "app.example.com",
            "app.example.org",
            "www.app.example.org",
            "short.example.com",
        ] {
            assert_eq!(desired.get(name).map(String::as_str), Some("1.2.3.4"), "{name} missing");
        }

        let no_ip = desired_dns_records(&db, "", "example.com", "dash", "poke").await;
        assert!(!no_ip.contains_key("dash.example.com"));
        assert!(!no_ip.contains_key("app.example.com"));
    }

    #[test]
    fn platform_names_are_identified() {
        assert!(is_platform_name("dash.example.com", "example.com"));
        assert!(is_platform_name("example.com", "example.com"));
        assert!(!is_platform_name("app.example.org", "example.com"));
        assert!(!is_platform_name("notexample.com", "example.com"));
    }
}

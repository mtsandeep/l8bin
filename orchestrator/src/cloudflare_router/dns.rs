use std::collections::{HashMap, HashSet};

use litebin_common::routing::ProjectRoute;

use super::CloudflareDnsRouter;

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
        // Compute desired DNS records
        let mut desired: HashMap<String, String> = HashMap::new(); // name → ip

        // Dashboard subdomain A record → master node public IP
        if !self.config.public_ip.is_empty() {
            let dashboard_host = format!("{}.{}", dashboard_subdomain, domain);
            desired.insert(dashboard_host, self.config.public_ip.clone());
        }

        // Poke subdomain A record → master node public IP
        if !self.config.public_ip.is_empty() {
            let poke_host = format!("{}.{}", poke_subdomain, domain);
            desired.insert(poke_host, self.config.public_ip.clone());
        }

        // Query ALL projects and add DNS records for each one.
        // DNS records are only removed when a project is deleted (or Cloudflare is cleared),
        // never when a project is stopped — so that stopped projects still resolve and
        // reach the waker via the catch-all route.
        let all_projects: Vec<(String, Option<String>, Option<String>, bool)> =
            match sqlx::query_as("SELECT id, node_id, custom_domain, is_background FROM projects")
                .fetch_all(&self.db)
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(error = %e, "sync_dns: failed to fetch projects, aborting DNS sync");
                    return Err(e.into());
                }
            };

        for (project_id, node_id, custom_domain, is_background) in &all_projects {
            let ip = match (*is_background, node_id.as_deref()) {
                (true, _) => {
                    if self.config.public_ip.is_empty() {
                        continue;
                    }
                    self.config.public_ip.clone()
                }
                (false, Some(nid)) if nid != "local" => {
                    let row: Option<(Option<String>,)> = match sqlx::query_as(
                        "SELECT public_ip FROM nodes WHERE id = ?",
                    )
                    .bind(nid)
                    .fetch_optional(&self.db)
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
                    if self.config.public_ip.is_empty() {
                        continue;
                    }
                    self.config.public_ip.clone()
                }
            };

            // Subdomain A record (e.g. mc.e4dx.com)
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
                .fetch_all(&self.db)
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

/// The zone managing `name`: the configured zone when it covers the name,
/// else the longest-covering account zone, else None.
fn zone_for_name<'a>(
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
        let zones = vec![zone("z2", "codinglog.com")];
        assert_eq!(zone_for_name("dash.11b.in", "z1", "11b.in", &zones), Some("z1"));
    }

    #[test]
    fn custom_domains_map_to_their_own_zone() {
        let zones = vec![zone("z1", "11b.in"), zone("z2", "codinglog.com")];
        assert_eq!(zone_for_name("cms.codinglog.com", "z1", "11b.in", &zones), Some("z2"));
    }

    #[test]
    fn longest_covering_zone_wins() {
        let zones = vec![zone("z2", "codinglog.com"), zone("z3", "sub.codinglog.com")];
        assert_eq!(zone_for_name("app.sub.codinglog.com", "z1", "11b.in", &zones), Some("z3"));
    }

    #[test]
    fn unknown_zone_is_none() {
        assert_eq!(zone_for_name("app.unknown.io", "z1", "11b.in", &[]), None);
    }

    #[test]
    fn platform_names_are_identified() {
        assert!(is_platform_name("dash.11b.in", "11b.in"));
        assert!(is_platform_name("11b.in", "11b.in"));
        assert!(!is_platform_name("cms.codinglog.com", "11b.in"));
        assert!(!is_platform_name("not11b.in", "11b.in"));
    }
}

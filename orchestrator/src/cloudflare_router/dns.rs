use std::collections::{HashMap, HashSet};

use litebin_common::routing::ProjectRoute;

use super::CloudflareDnsRouter;

impl CloudflareDnsRouter {
    /// Sync Cloudflare DNS records: upsert for all projects, delete stale ones.
    /// DNS records are kept for all projects regardless of status (running, stopped, etc.)
    /// so that stopped projects still resolve and hit the waker.
    /// Returns counts of created, deleted, and errored records.
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

        let domain_suffix = format!(".{}", domain);

        // List existing A records for our domain
        let existing = self.cloudflare.list_records_by_suffix(&domain_suffix, "A").await?;

        let mut result = litebin_common::routing::DnsSyncResult::default();

        // Build set of existing record names for fast lookup
        let existing_names: HashSet<&str> = existing.iter().map(|r| r.name.as_str()).collect();

        // Delete records that exist but shouldn't
        let desired_names: HashSet<&str> = desired.keys().map(|s| s.as_str()).collect();
        for record in &existing {
            if !desired_names.contains(record.name.as_str()) {
                if let Err(e) = self.cloudflare.delete_record(&record.id).await {
                    tracing::warn!(record = %record.name, error = %e, "failed to delete stale DNS record");
                    result.errors += 1;
                } else {
                    result.deleted += 1;
                }
            }
        }

        // Upsert desired records
        for (name, ip) in &desired {
            if existing_names.contains(name.as_str()) {
                result.unchanged += 1;
                continue;
            }
            match self.cloudflare.upsert_record(name, "A", ip, 1, false).await {
                Ok(true) => result.created += 1,
                Ok(false) => result.unchanged += 1,
                Err(e) => {
                    tracing::warn!(name, ip, error = %e, "failed to upsert DNS record");
                    result.errors += 1;
                }
            }
        }

        tracing::info!(
            created = result.created,
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

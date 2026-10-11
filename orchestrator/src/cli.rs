//! Out-of-band admin subcommands.
//!
//! These handlers run as their own process invocation (no HTTP server is
//! started, no routes are registered). They are unreachable from the network
//! and only affect local state — useful for recovery scenarios such as a
//! forgotten admin password.

use std::io::{self, Write};

use bcrypt::{DEFAULT_COST, hash};

use crate::config::Config;
use crate::db;

/// `reset-password` subcommand.
///
/// Prompts interactively for a username and a new password, then writes a
/// fresh bcrypt hash into the `users` table. Intended usage:
///
/// ```text
/// docker exec -it <orchestrator-container> /app/litebin-orchestrator reset-password
/// ```
///
/// Safety notes:
/// - No HTTP route is registered for this — the only way in is process argv.
/// - The DB pool is opened in read-write mode but no migrations are forced;
///   the existing schema is assumed (this matches the running server).
/// - If the server is currently running, SQLite's file locking still keeps
///   the update safe; the affected user's other sessions are not invalidated
///   implicitly — `session_auth_hash` is derived from `password_hash`, so
///   existing cookies become invalid on the next request.
pub async fn reset_password() -> anyhow::Result<()> {
    // We deliberately skip tracing initialization for subcommands so the
    // output stays clean and script-friendly.
    let config = Config::from_env()?;
    let db = db::init_pool(&config.database_url).await?;

    println!("litebin orchestrator — password reset");
    println!();

    let username = prompt("Username: ")?;
    if username.is_empty() {
        anyhow::bail!("username is required");
    }

    // Confirm the user exists before asking for a new password — saves the
    // operator from typing a password only to discover they mistyped the
    // username. We intentionally do NOT reveal whether the user exists to
    // any caller other than the local operator (this is a recovery tool,
    // not an endpoint), so a clear error here is fine.
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE username = ?)")
        .bind(&username)
        .fetch_one(&db)
        .await?;
    if !exists {
        anyhow::bail!("no user found with username {:?}", username);
    }

    let new_password = rpassword::prompt_password("New password: ")?;
    if new_password.is_empty() {
        anyhow::bail!("password must not be empty");
    }
    let confirm = rpassword::prompt_password("Confirm password: ")?;
    if new_password != confirm {
        anyhow::bail!("passwords did not match");
    }

    let new_hash = hash(new_password.as_bytes(), DEFAULT_COST)?;
    let now = chrono::Utc::now().timestamp();
    let rows = sqlx::query("UPDATE users SET password_hash = ?, updated_at = ? WHERE username = ?")
        .bind(&new_hash)
        .bind(now)
        .bind(&username)
        .execute(&db)
        .await?
        .rows_affected();

    if rows == 0 {
        // Should not happen given the EXISTS check above, but guard anyway.
        anyhow::bail!("no rows updated; user may have been removed mid-flight");
    }

    println!();
    println!("Password for {:?} has been reset.", username);
    println!("Existing sessions for this user are now invalid and must log in again.");
    Ok(())
}

/// `set-domain <domain>` subcommand.
///
/// Persists a new platform domain into the settings DB and best-effort
/// re-registers online agents; restart the orchestrator afterwards (boot
/// route sync rebuilds Caddy/DNS/agent configs). Intended usage:
///
/// ```text
/// docker exec -it litebin-orchestrator /app/litebin-orchestrator set-domain <domain>
/// ```
///
/// Safety notes:
/// - No HTTP route is registered for this — the only way in is process argv.
/// - Editing `.env` `DOMAIN` would NOT work: it is an INSERT OR IGNORE seed
///   and the DB row wins once it exists.
pub async fn set_domain(raw: &str) -> anyhow::Result<()> {
    // We deliberately skip tracing initialization for subcommands so the
    // output stays clean and script-friendly.
    let config = Config::from_env()?;
    let db = db::init_pool(&config.database_url).await?;

    let domain = crate::platform::normalize_domain(raw).map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("litebin orchestrator — set platform domain");
    println!();

    let old: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'domain'").fetch_optional(&db).await?;
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES ('domain', ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(&domain)
    .execute(&db)
    .await?;
    println!("domain: {} -> {}", old.as_deref().unwrap_or("(unset)"), domain);

    // Best-effort re-register online agents so their Caddy hosts and wake
    // URLs follow the new domain. Offline agents keep the old registration —
    // re-run node connect (API/dashboard) for them after recovery.
    let poke_subdomain = setting_or(&db, "poke_subdomain", config.poke_subdomain.clone()).await;
    let wake = crate::routes::nodes::wake_report_url_from(&config, &poke_subdomain, &domain);
    let heartbeat = crate::routes::nodes::heartbeat_url_from(&config, &poke_subdomain, &domain);
    let node_clients = std::sync::Arc::new(dashmap::DashMap::new());
    let (ok, errs) =
        crate::routes::nodes::reregister_nodes(&db, &config, &node_clients, &domain, wake, heartbeat).await;
    if ok > 0 {
        println!("re-registered {ok} online agent(s) with the new domain");
    }
    for e in &errs {
        println!("warning: {e}");
    }
    if !errs.is_empty() {
        println!("agents that failed above keep the old domain; re-run node connect for them after restart");
    }
    if ok == 0 && errs.is_empty() {
        println!("no remote agents to re-register");
    }

    let dashboard_subdomain = setting_or(&db, "dashboard_subdomain", config.dashboard_subdomain.clone()).await;
    println!();
    println!("Dashboard will be at https://{dashboard_subdomain}.{domain} after restart.");
    println!("Restart now:  docker compose restart orchestrator");
    Ok(())
}

/// `set-dashboard-subdomain <label>` subcommand.
///
/// Persists the dashboard label into the settings DB; restart the orchestrator
/// afterwards. Intended usage:
///
/// ```text
/// docker exec -it litebin-orchestrator /app/litebin-orchestrator set-dashboard-subdomain <label>
/// ```
pub async fn set_dashboard_subdomain(raw: &str) -> anyhow::Result<()> {
    let config = Config::from_env()?;
    let db = db::init_pool(&config.database_url).await?;

    let label = crate::platform::normalize_subdomain_label(raw).map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("litebin orchestrator — set dashboard subdomain");
    println!();

    let old: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'dashboard_subdomain'").fetch_optional(&db).await?;
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES ('dashboard_subdomain', ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(&label)
    .execute(&db)
    .await?;
    println!("dashboard_subdomain: {} -> {}", old.as_deref().unwrap_or("(unset)"), label);

    let domain = setting_or(&db, "domain", config.domain.clone()).await;
    println!("Dashboard will be at https://{label}.{domain} after restart.");
    println!("Restart now:  docker compose restart orchestrator");
    Ok(())
}

async fn setting_or(db: &sqlx::SqlitePool, key: &str, fallback: String) -> String {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .filter(|v| !v.is_empty())
        .unwrap_or(fallback)
}

fn prompt(label: &str) -> io::Result<String> {
    print!("{label}");
    io::stdout().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    Ok(buf.trim().to_string())
}

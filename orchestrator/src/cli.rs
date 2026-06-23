//! Out-of-band admin subcommands.
//!
//! These handlers run as their own process invocation (no HTTP server is
//! started, no routes are registered). They are unreachable from the network
//! and only affect local state — useful for recovery scenarios such as a
//! forgotten admin password.

use std::io::{self, Write};

use bcrypt::{hash, DEFAULT_COST};

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

fn prompt(label: &str) -> io::Result<String> {
    print!("{label}");
    io::stdout().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    Ok(buf.trim().to_string())
}

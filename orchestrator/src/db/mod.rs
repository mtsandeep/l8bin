pub mod models;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::str::FromStr;
use std::time::Duration;

pub async fn init_pool(database_url: &str) -> anyhow::Result<SqlitePool> {
    // Ensure the data directory exists
    if let Some(path) = database_url.strip_prefix("sqlite:") {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .busy_timeout(Duration::from_secs(5));

    let pool = SqlitePoolOptions::new()
        .max_connections(10)
        .connect_with(options)
        .await?;

    // Enable WAL mode for better concurrent read performance
    sqlx::query("PRAGMA journal_mode=WAL")
        .execute(&pool)
        .await?;

    sqlx::migrate!("src/db/migrations").run(&pool).await?;

    tracing::info!("database migrations complete");
    Ok(pool)
}

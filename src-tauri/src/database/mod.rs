pub mod migrations;

use sqlx::sqlite::SqliteConnectOptions;
use sqlx::SqlitePool;

use crate::error::{AppError, Result};

/// Initialize the database: create `~/.rss-reader/` directory, open SQLite
/// with WAL journal mode, and run migrations.
pub async fn init_database() -> Result<SqlitePool> {
    let home_dir =
        dirs::home_dir().ok_or_else(|| AppError::Internal("Failed to get home directory".into()))?;
    let app_dir = home_dir.join(".rss-reader");

    std::fs::create_dir_all(&app_dir)
        .map_err(|e| AppError::Internal(format!("Failed to create app dir: {}", e)))?;

    let db_path = app_dir.join("rss_reader.db");

    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(30));

    let pool = SqlitePool::connect_with(options)
        .await
        .map_err(AppError::Database)?;

    migrations::run_migrations(&pool).await?;

    Ok(pool)
}

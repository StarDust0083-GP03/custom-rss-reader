pub mod schema;
pub mod migrations;

use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};
use std::str::FromStr;
use tauri::{AppHandle, Manager};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DbError {
    #[error("Database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("Migration error: {0}")]
    Migration(String),
}

pub type Result<T> = std::result::Result<T, DbError>;

pub async fn init_database(app_handle: &AppHandle) -> Result<SqlitePool> {
    // Use home directory for better compatibility across platforms
    let home_dir = dirs::home_dir()
        .ok_or_else(|| DbError::Migration("Failed to get home directory".to_string()))?;

    let app_dir = home_dir.join(".rss-reader");

    // Create directory
    std::fs::create_dir_all(&app_dir)
        .map_err(|e| DbError::Migration(format!("Failed to create app dir: {}", e)))?;

    let db_path = app_dir.join("rss_reader.db");

    // Use direct path with SQLite options
    let connection_string = format!("sqlite:file://{}", db_path.display());

    let options = SqliteConnectOptions::from_str(&connection_string)?
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(30));

    let pool = SqlitePool::connect_with(options).await?;

    // Run migrations
    migrations::run_migrations(&pool).await?;

    Ok(pool)
}

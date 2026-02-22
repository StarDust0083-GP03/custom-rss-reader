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
    let app_dir = app_handle.path().app_data_dir()
        .map_err(|e| DbError::Migration(format!("Failed to get app dir: {}", e)))?;

    std::fs::create_dir_all(&app_dir)
        .map_err(|e| DbError::Migration(format!("Failed to create app dir: {}", e)))?;

    let db_path = app_dir.join("rss_reader.db");
    let connection_string = format!("sqlite:{}", db_path.display());

    let options = SqliteConnectOptions::from_str(&connection_string)?
        .create_if_missing(true);

    let pool = SqlitePool::connect_with(options).await?;

    // Run migrations
    migrations::run_migrations(&pool).await?;

    Ok(pool)
}

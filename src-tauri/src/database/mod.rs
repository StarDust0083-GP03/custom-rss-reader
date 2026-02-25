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
    // Try app_data_dir first, fall back to app_config_dir for better compatibility
    let app_dir = app_handle.path().app_data_dir()
        .or_else(|_| app_handle.path().app_config_dir())
        .map_err(|e| DbError::Migration(format!("Failed to get app dir: {}", e)))?;

    // Create directory with proper permissions
    std::fs::create_dir_all(&app_dir)
        .map_err(|e| DbError::Migration(format!("Failed to create app dir: {}", e)))?;

    // On Unix systems (macOS/Linux), ensure directory has proper permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(&app_dir) {
            let mut perms = metadata.permissions();
            // Ensure directory is readable/writable by owner (0o755 for broader compatibility)
            let current_mode = perms.mode() & 0o777;
            if current_mode != 0o755 {
                perms.set_mode(0o755);
                let _ = std::fs::set_permissions(&app_dir, perms);
            }
        }
    }

    let db_path = app_dir.join("rss_reader.db");

    // If database file exists, ensure it has proper permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if db_path.exists() {
            if let Ok(metadata) = std::fs::metadata(&db_path) {
                let mut perms = metadata.permissions();
                // Ensure file is readable/writable by owner (0o644 for better compatibility)
                let current_mode = perms.mode() & 0o777;
                if current_mode != 0o644 {
                    perms.set_mode(0o644);
                    let _ = std::fs::set_permissions(&db_path, perms);
                }
            }
        }
    }

    // Use SQLite connection string with proper options
    let connection_string = format!("sqlite:{}", db_path.display());

    let options = SqliteConnectOptions::from_str(&connection_string)?
        .create_if_missing(true)
        // Enable WAL mode for better concurrency
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        // Set busy timeout to handle concurrent access
        .busy_timeout(std::time::Duration::from_secs(30));

    let pool = SqlitePool::connect_with(options).await?;

    // Run migrations
    migrations::run_migrations(&pool).await?;

    Ok(pool)
}

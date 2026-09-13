pub mod migrations;

use sqlx::sqlite::SqliteConnectOptions;
use sqlx::SqlitePool;

use crate::error::{AppError, Result};

/// Initialize the database: create `~/.rss-reader/` directory, open SQLite
/// with WAL journal mode, and run migrations.
pub async fn init_database() -> Result<SqlitePool> {
    let home_dir = dirs::home_dir()
        .ok_or_else(|| AppError::Internal("Failed to get home directory".into()))?;
    let app_dir = home_dir.join(".rss-reader");

    std::fs::create_dir_all(&app_dir)
        .map_err(|e| AppError::Internal(format!("Failed to create app dir: {}", e)))?;

    let db_path = app_dir.join("rss_reader.db");

    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        // `feed_items.subscription_id` relies on ON DELETE CASCADE. SQLite
        // disables foreign keys per connection unless explicitly enabled.
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(30));

    let pool = SqlitePool::connect_with(options)
        .await
        .map_err(AppError::Database)?;

    // A migration that merges or rewrites user data (duplicate articles,
    // annotation columns) is not reversible in place. Keep a copy next to the
    // database before the first new version runs, so a bad upgrade can be
    // rolled back by hand instead of losing a library.
    backup_before_migration(&pool, &db_path).await;

    migrations::run_migrations(&pool).await?;
    checkpoint_large_wal(&pool, &db_path).await;

    Ok(pool)
}

/// Truncate an unusually large WAL after migrations or a large rewrite.
///
/// Checkpointing every startup would add unnecessary work to normal launches,
/// so only clean up WAL files above 64 MiB. This is best effort: a live reader
/// may temporarily prevent truncation, and that must not make the app fail to
/// start.
async fn checkpoint_large_wal(pool: &SqlitePool, db_path: &std::path::Path) {
    const CHECKPOINT_THRESHOLD_BYTES: u64 = 64 * 1024 * 1024;
    let wal_path = db_path.with_extension("db-wal");
    let Ok(before) = std::fs::metadata(&wal_path).map(|metadata| metadata.len()) else {
        return;
    };
    if before < CHECKPOINT_THRESHOLD_BYTES {
        return;
    }

    match sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_one(pool)
        .await
    {
        Ok(_) => {
            let after = std::fs::metadata(&wal_path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            println!(
                "[database] checkpointed WAL: {} MiB -> {} MiB",
                before / (1024 * 1024),
                after / (1024 * 1024)
            );
        }
        Err(error) => eprintln!(
            "[database] WAL checkpoint skipped ({}; WAL was {} MiB)",
            error,
            before / (1024 * 1024)
        ),
    }
}

/// Copy the database file when this build has migrations the file hasn't seen.
///
/// Best effort: a failed backup is logged and never blocks startup — refusing
/// to open the library would be a worse outcome than migrating without one.
pub(crate) async fn backup_before_migration(pool: &SqlitePool, db_path: &std::path::Path) {
    let pending = match migrations::pending_versions(pool).await {
        Ok(pending) => pending,
        Err(error) => {
            eprintln!("[migration] could not read schema versions: {}", error);
            return;
        }
    };
    let Some(&newest) = pending.iter().max() else {
        return;
    };
    if !db_path.exists() {
        return;
    }

    // `create_if_missing` means the file exists on a first launch too. A
    // database with no tables holds nothing to protect, and writing a backup of
    // an empty file would both waste space and make "a backup exists" a
    // meaningless signal when diagnosing an upgrade.
    let existing_tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    if existing_tables == 0 {
        return;
    }

    let backup = db_path.with_extension(format!("db.pre-migration-v{newest}.bak"));
    if backup.exists() {
        return;
    }

    // `VACUUM INTO` produces a consistent snapshot of a WAL database without
    // stopping the pool, unlike a raw file copy.
    let target = backup.to_string_lossy().to_string();
    match sqlx::query("VACUUM INTO $1")
        .bind(&target)
        .execute(pool)
        .await
    {
        Ok(_) => println!(
            "[migration] backed up database to {} before applying {} migration(s)",
            backup.display(),
            pending.len()
        ),
        Err(error) => eprintln!(
            "[migration] pre-migration backup failed ({}); continuing without one",
            error
        ),
    }
}

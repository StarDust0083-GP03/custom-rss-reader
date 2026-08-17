use sqlx::{Executor, SqlitePool};

use crate::error::{AppError, Result};

/// Run all pending database migrations.
///
/// Creates tables from scratch if they don't exist, otherwise adds any
/// missing columns incrementally. Indexes are created at the end.
pub async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    let tables_exist = sqlx::query(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='feed_items'",
    )
    .fetch_optional(pool)
    .await
    .map_err(AppError::Database)?
    .is_some();

    if !tables_exist {
        create_tables(pool).await?;
    } else {
        add_missing_columns(pool).await?;
        // Existing databases may carry duplicate (subscription_id, guid) rows
        // from before the unique index existed; remove them so the index can
        // be created.
        dedupe_feed_item_guids(pool).await?;
    }

    create_indexes(pool).await?;

    Ok(())
}

async fn create_tables(pool: &SqlitePool) -> Result<()> {
    pool.execute(
        r#"
        CREATE TABLE subscriptions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            url TEXT NOT NULL UNIQUE,
            title TEXT,
            website_url TEXT,
            rsshub_url TEXT,
            use_website BOOLEAN DEFAULT 0,
            auto_classify BOOLEAN DEFAULT 1,
            opml_attributes TEXT,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .await
    .map_err(AppError::Database)?;

    pool.execute(
        r#"
        CREATE TABLE feed_items (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            subscription_id INTEGER NOT NULL,
            guid TEXT,
            title TEXT NOT NULL,
            link TEXT,
            content TEXT,
            content_md TEXT,
            description TEXT,
            author TEXT,
            published_at DATETIME,
            fetched_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            is_website_content BOOLEAN DEFAULT 0,
            is_read BOOLEAN DEFAULT 0,
            is_favorite BOOLEAN DEFAULT 0,
            is_read_later BOOLEAN DEFAULT 0,
            is_ignored BOOLEAN DEFAULT 0,
            tags TEXT,
            category TEXT,
            translated_title TEXT,
            translated_content TEXT,
            translated_at DATETIME,
            FOREIGN KEY (subscription_id) REFERENCES subscriptions(id) ON DELETE CASCADE
        )
        "#,
    )
    .await
    .map_err(AppError::Database)?;

    Ok(())
}

/// Add a column if it does not exist yet. Logs (but does not fail on) errors
/// so a single failed ALTER does not abort the whole migration.
async fn ensure_column(pool: &SqlitePool, table: &str, column: &str, ddl: &str) {
    let probe = format!("SELECT {} FROM {} LIMIT 1", column, table);
    let exists = sqlx::query(&probe).fetch_optional(pool).await.is_ok();
    if !exists {
        if let Err(e) = sqlx::query(ddl).execute(pool).await {
            eprintln!("[migration] failed to add column {}.{}: {}", table, column, e);
        }
    }
}

async fn add_missing_columns(pool: &SqlitePool) -> Result<()> {
    ensure_column(pool, "feed_items", "content_md",
        "ALTER TABLE feed_items ADD COLUMN content_md TEXT").await;
    ensure_column(pool, "feed_items", "tags",
        "ALTER TABLE feed_items ADD COLUMN tags TEXT").await;
    ensure_column(pool, "feed_items", "category",
        "ALTER TABLE feed_items ADD COLUMN category TEXT").await;
    ensure_column(pool, "feed_items", "translated_title",
        "ALTER TABLE feed_items ADD COLUMN translated_title TEXT").await;
    ensure_column(pool, "feed_items", "translated_content",
        "ALTER TABLE feed_items ADD COLUMN translated_content TEXT").await;
    ensure_column(pool, "feed_items", "translated_at",
        "ALTER TABLE feed_items ADD COLUMN translated_at DATETIME").await;
    ensure_column(pool, "feed_items", "is_ignored",
        "ALTER TABLE feed_items ADD COLUMN is_ignored BOOLEAN DEFAULT 0").await;
    ensure_column(pool, "subscriptions", "auto_classify",
        "ALTER TABLE subscriptions ADD COLUMN auto_classify BOOLEAN DEFAULT 1").await;

    Ok(())
}

/// Remove duplicate (subscription_id, guid) rows, keeping the oldest copy.
/// Rows with NULL guid are left alone (SQLite unique indexes treat NULLs as
/// distinct anyway).
async fn dedupe_feed_item_guids(pool: &SqlitePool) -> Result<()> {
    let result = sqlx::query(
        r#"
        DELETE FROM feed_items
        WHERE guid IS NOT NULL AND id NOT IN (
            SELECT MIN(id) FROM feed_items WHERE guid IS NOT NULL
            GROUP BY subscription_id, guid
        )
        "#,
    )
    .execute(pool)
    .await
    .map_err(AppError::Database)?;

    if result.rows_affected() > 0 {
        println!(
            "[migration] removed {} duplicate feed_items rows",
            result.rows_affected()
        );
    }
    Ok(())
}

async fn create_indexes(pool: &SqlitePool) -> Result<()> {
    // Drop legacy low-selectivity indexes. The boolean single-column indexes
    // are essentially never used by the query planner, and the subscription
    // index is covered by the (subscription_id, published_at) composite.
    let drops = [
        "DROP INDEX IF EXISTS idx_feed_items_is_read",
        "DROP INDEX IF EXISTS idx_feed_items_is_favorite",
        "DROP INDEX IF EXISTS idx_feed_items_is_read_later",
        "DROP INDEX IF EXISTS idx_feed_items_subscription",
    ];
    for ddl in drops {
        sqlx::query(ddl).execute(pool).await.map_err(AppError::Database)?;
    }

    let indexes = [
        // Covers the hot path: WHERE subscription_id = ? ORDER BY published_at DESC
        "CREATE INDEX IF NOT EXISTS idx_feed_items_sub_pub ON feed_items(subscription_id, published_at DESC)",
        "CREATE INDEX IF NOT EXISTS idx_feed_items_published ON feed_items(published_at DESC)",
        "CREATE INDEX IF NOT EXISTS idx_feed_items_fav_pub ON feed_items(is_favorite, published_at DESC)",
        "CREATE INDEX IF NOT EXISTS idx_feed_items_rl_pub ON feed_items(is_read_later, published_at DESC)",
    ];
    for ddl in indexes {
        sqlx::query(ddl).execute(pool).await.map_err(AppError::Database)?;
    }

    // The unique dedup index must not fail silently: without it the fetch
    // pipeline loses its last line of defense against duplicate rows.
    let unique = "CREATE UNIQUE INDEX IF NOT EXISTS idx_feed_items_guid ON feed_items(subscription_id, guid)";
    if let Err(e) = sqlx::query(unique).execute(pool).await {
        eprintln!(
            "[migration] WARNING: could not create unique index idx_feed_items_guid: {}. \
             Duplicate (subscription_id, guid) rows may still exist; dedup protection is OFF.",
            e
        );
    }

    Ok(())
}

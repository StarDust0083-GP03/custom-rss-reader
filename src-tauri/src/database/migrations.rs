use sqlx::{Executor, SqlitePool};

use crate::error::{AppError, Result};

/// Minimum content length to bother converting to markdown (avoids pointless
/// conversions for RSS snippets that are just "Continue reading..." links).
const CONTENT_MD_BACKFILL_MIN_LEN: usize = 80;

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
    .map_err(|e| AppError::Database(e))?
    .is_some();

    if !tables_exist {
        create_tables(pool).await?;
    } else {
        add_missing_columns(pool).await?;
    }

    // Backfill content_md for existing items that have HTML content but no markdown
    backfill_content_md(pool).await?;

    // Best-effort index creation
    let _ = create_indexes(pool).await;

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
    .map_err(|e| AppError::Database(e))?;

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
    .map_err(|e| AppError::Database(e))?;

    Ok(())
}

async fn add_missing_columns(pool: &SqlitePool) -> Result<()> {
    // content_md column
    let has_content_md = sqlx::query("SELECT content_md FROM feed_items LIMIT 1")
        .fetch_optional(pool)
        .await
        .is_ok();
    if !has_content_md {
        sqlx::query("ALTER TABLE feed_items ADD COLUMN content_md TEXT")
            .execute(pool)
            .await
            .ok();
    }

    // tags column
    let has_tags = sqlx::query("SELECT tags FROM feed_items LIMIT 1")
        .fetch_optional(pool)
        .await
        .is_ok();
    if !has_tags {
        sqlx::query("ALTER TABLE feed_items ADD COLUMN tags TEXT")
            .execute(pool)
            .await
            .ok();
    }

    // category column
    let has_category = sqlx::query("SELECT category FROM feed_items LIMIT 1")
        .fetch_optional(pool)
        .await
        .is_ok();
    if !has_category {
        sqlx::query("ALTER TABLE feed_items ADD COLUMN category TEXT")
            .execute(pool)
            .await
            .ok();
    }

    // translated_title column
    let has_translated_title =
        sqlx::query("SELECT translated_title FROM feed_items LIMIT 1")
            .fetch_optional(pool)
            .await
            .is_ok();
    if !has_translated_title {
        sqlx::query("ALTER TABLE feed_items ADD COLUMN translated_title TEXT")
            .execute(pool)
            .await
            .ok();
    }

    // translated_content column
    let has_translated_content =
        sqlx::query("SELECT translated_content FROM feed_items LIMIT 1")
            .fetch_optional(pool)
            .await
            .is_ok();
    if !has_translated_content {
        sqlx::query("ALTER TABLE feed_items ADD COLUMN translated_content TEXT")
            .execute(pool)
            .await
            .ok();
    }

    // translated_at column
    let has_translated_at = sqlx::query("SELECT translated_at FROM feed_items LIMIT 1")
        .fetch_optional(pool)
        .await
        .is_ok();
    if !has_translated_at {
        sqlx::query("ALTER TABLE feed_items ADD COLUMN translated_at DATETIME")
            .execute(pool)
            .await
            .ok();
    }

    // auto_classify column on subscriptions
    let has_auto_classify =
        sqlx::query("SELECT auto_classify FROM subscriptions LIMIT 1")
            .fetch_optional(pool)
            .await
            .is_ok();
    if !has_auto_classify {
        sqlx::query("ALTER TABLE subscriptions ADD COLUMN auto_classify BOOLEAN DEFAULT 1")
            .execute(pool)
            .await
            .ok();
    }

    // is_ignored column on feed_items
    let has_is_ignored = sqlx::query("SELECT is_ignored FROM feed_items LIMIT 1")
        .fetch_optional(pool)
        .await
        .is_ok();
    if !has_is_ignored {
        sqlx::query("ALTER TABLE feed_items ADD COLUMN is_ignored BOOLEAN DEFAULT 0")
            .execute(pool)
            .await
            .ok();
    }

    Ok(())
}

async fn create_indexes(pool: &SqlitePool) -> Result<()> {
    let indexes = [
        "CREATE INDEX IF NOT EXISTS idx_feed_items_subscription ON feed_items(subscription_id)",
        "CREATE INDEX IF NOT EXISTS idx_feed_items_published ON feed_items(published_at DESC)",
        "CREATE INDEX IF NOT EXISTS idx_feed_items_is_read ON feed_items(is_read)",
        "CREATE INDEX IF NOT EXISTS idx_feed_items_is_favorite ON feed_items(is_favorite)",
        "CREATE INDEX IF NOT EXISTS idx_feed_items_is_read_later ON feed_items(is_read_later)",
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_feed_items_guid ON feed_items(subscription_id, guid)",
    ];

    for index in indexes {
        sqlx::query(index).execute(pool).await.ok();
    }

    Ok(())
}

/// Backfill `content_md` for existing feed items that have HTML `content`
/// but no markdown yet.
///
/// Items whose content is shorter than `CONTENT_MD_BACKFILL_MIN_LEN` chars
/// are skipped (likely just "Continue reading..." snippets).
async fn backfill_content_md(pool: &SqlitePool) -> Result<()> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, content FROM feed_items WHERE content IS NOT NULL AND content_md IS NULL",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Database(e))?;

    if rows.is_empty() {
        return Ok(());
    }

    let count = rows.len();
    let mut updated = 0;

    for (id, content) in &rows {
        if content.len() < CONTENT_MD_BACKFILL_MIN_LEN {
            continue;
        }
        let md = html2md::parse_html(content);
        if md.is_empty() || md.trim() == content.trim() {
            continue; // skip if no useful conversion occurred
        }
        sqlx::query("UPDATE feed_items SET content_md = $1 WHERE id = $2")
            .bind(&md)
            .bind(id)
            .execute(pool)
            .await
            .map_err(|e| AppError::Database(e))?;
        updated += 1;
    }

    if updated > 0 {
        println!(
            "[migration] Backfilled content_md for {}/{} items",
            updated, count
        );
    }

    Ok(())
}

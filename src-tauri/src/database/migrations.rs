use sqlx::{Executor, SqliteConnection, SqlitePool};

use crate::error::{AppError, Result};
use crate::models::tag::{normalize_tag, MAX_TAGS_PER_ITEM};

/// Run all pending database migrations.
///
/// Creates tables from scratch if they don't exist, otherwise adds any
/// missing columns incrementally. Indexes are created at the end.
pub async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    let mut tx = pool.begin().await.map_err(AppError::Database)?;
    let tables_exist =
        sqlx::query("SELECT name FROM sqlite_master WHERE type='table' AND name='feed_items'")
            .fetch_optional(&mut *tx)
            .await
            .map_err(AppError::Database)?
            .is_some();

    if !tables_exist {
        create_tables(&mut tx).await?;
    } else {
        add_missing_columns(&mut tx).await?;
        // Existing databases may carry duplicate (subscription_id, guid) rows
        // from before the unique index existed; remove them so the index can
        // be created.
        dedupe_feed_item_guids(&mut tx).await?;
    }

    create_tag_tables(&mut tx).await?;
    create_app_metadata(&mut tx).await?;
    backfill_tag_catalog(&mut tx).await?;
    create_indexes(&mut tx).await?;
    tx.commit().await.map_err(AppError::Database)?;

    Ok(())
}

async fn create_tables(conn: &mut SqliteConnection) -> Result<()> {
    conn.execute(
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

    conn.execute(
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

async fn create_app_metadata(conn: &mut SqliteConnection) -> Result<()> {
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS app_metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )
        "#,
    )
    .await
    .map_err(AppError::Database)?;

    // The id survives normal migrations but changes when the SQLite database
    // is replaced. Chroma sync uses it to invalidate an old watermark.
    conn.execute(
        "INSERT OR IGNORE INTO app_metadata (key, value) VALUES ('database_id', lower(hex(randomblob(16))))",
    )
    .await
    .map_err(AppError::Database)?;

    Ok(())
}

async fn create_tag_tables(conn: &mut SqliteConnection) -> Result<()> {
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS tag_catalog (
            name TEXT PRIMARY KEY COLLATE NOCASE,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .await
    .map_err(AppError::Database)?;

    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS tag_aliases (
            alias TEXT PRIMARY KEY COLLATE NOCASE,
            canonical_name TEXT NOT NULL COLLATE NOCASE,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .await
    .map_err(AppError::Database)?;

    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS blocked_tags (
            name TEXT PRIMARY KEY COLLATE NOCASE,
            blocked_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .await
    .map_err(AppError::Database)?;

    Ok(())
}

/// Seed the catalog when upgrading a database that predates controlled tags.
/// The catalog count guard keeps subsequent startups cheap.
async fn backfill_tag_catalog(conn: &mut SqliteConnection) -> Result<()> {
    let catalog_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tag_catalog")
        .fetch_one(&mut *conn)
        .await
        .map_err(AppError::Database)?;
    if catalog_count > 0 {
        return Ok(());
    }

    let rows: Vec<(i64, Option<String>)> = sqlx::query_as(
        "SELECT id, tags FROM feed_items WHERE tags IS NOT NULL AND json_valid(tags)",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(AppError::Database)?;

    for (item_id, raw_tags) in rows {
        let Some(raw_tags) = raw_tags else { continue };
        let Ok(tags) = serde_json::from_str::<Vec<String>>(&raw_tags) else {
            continue;
        };
        let mut normalized = Vec::new();
        for raw in tags {
            let Some(tag) = normalize_tag(&raw) else {
                continue;
            };
            if normalized.len() >= MAX_TAGS_PER_ITEM {
                break;
            }
            if !normalized.contains(&tag) {
                sqlx::query("INSERT OR IGNORE INTO tag_catalog (name) VALUES ($1)")
                    .bind(&tag)
                    .execute(&mut *conn)
                    .await
                    .map_err(AppError::Database)?;
                normalized.push(tag);
            }
        }

        let normalized_json = serde_json::to_string(&normalized)
            .map_err(|e| AppError::Internal(format!("Failed to serialize tags: {}", e)))?;
        if normalized_json != raw_tags {
            sqlx::query("UPDATE feed_items SET tags = $2 WHERE id = $1")
                .bind(item_id)
                .bind(normalized_json)
                .execute(&mut *conn)
                .await
                .map_err(AppError::Database)?;
        }
    }

    Ok(())
}

/// Add a column if it does not exist yet. Migration errors are returned to
/// the caller: starting with a partially migrated schema is less safe than
/// stopping and asking for the database error to be repaired.
async fn ensure_column(
    conn: &mut SqliteConnection,
    table: &str,
    column: &str,
    ddl: &str,
) -> Result<()> {
    let exists: Option<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info($1) WHERE name = $2 LIMIT 1")
            .bind(table)
            .bind(column)
            .fetch_optional(&mut *conn)
            .await
            .map_err(AppError::Database)?;

    if exists.is_none() {
        sqlx::query(ddl)
            .execute(&mut *conn)
            .await
            .map_err(AppError::Database)
            .map(|_| ())?;
    }
    Ok(())
}

async fn add_missing_columns(conn: &mut SqliteConnection) -> Result<()> {
    ensure_column(
        conn,
        "feed_items",
        "content_md",
        "ALTER TABLE feed_items ADD COLUMN content_md TEXT",
    )
    .await?;
    ensure_column(
        conn,
        "feed_items",
        "tags",
        "ALTER TABLE feed_items ADD COLUMN tags TEXT",
    )
    .await?;
    ensure_column(
        conn,
        "feed_items",
        "category",
        "ALTER TABLE feed_items ADD COLUMN category TEXT",
    )
    .await?;
    ensure_column(
        conn,
        "feed_items",
        "translated_title",
        "ALTER TABLE feed_items ADD COLUMN translated_title TEXT",
    )
    .await?;
    ensure_column(
        conn,
        "feed_items",
        "translated_content",
        "ALTER TABLE feed_items ADD COLUMN translated_content TEXT",
    )
    .await?;
    ensure_column(
        conn,
        "feed_items",
        "translated_at",
        "ALTER TABLE feed_items ADD COLUMN translated_at DATETIME",
    )
    .await?;
    ensure_column(
        conn,
        "feed_items",
        "is_ignored",
        "ALTER TABLE feed_items ADD COLUMN is_ignored BOOLEAN DEFAULT 0",
    )
    .await?;
    ensure_column(
        conn,
        "subscriptions",
        "auto_classify",
        "ALTER TABLE subscriptions ADD COLUMN auto_classify BOOLEAN DEFAULT 1",
    )
    .await?;

    Ok(())
}

/// Remove duplicate (subscription_id, guid) rows, keeping the oldest copy.
/// Rows with NULL guid are left alone (SQLite unique indexes treat NULLs as
/// distinct anyway).
async fn dedupe_feed_item_guids(conn: &mut SqliteConnection) -> Result<()> {
    let result = sqlx::query(
        r#"
        DELETE FROM feed_items
        WHERE guid IS NOT NULL AND id NOT IN (
            SELECT MIN(id) FROM feed_items WHERE guid IS NOT NULL
            GROUP BY subscription_id, guid
        )
        "#,
    )
    .execute(&mut *conn)
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

async fn create_indexes(conn: &mut SqliteConnection) -> Result<()> {
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
        sqlx::query(ddl)
            .execute(&mut *conn)
            .await
            .map_err(AppError::Database)?;
    }

    let indexes = [
        // Covers the hot path: WHERE subscription_id = ? ORDER BY published_at DESC
        "CREATE INDEX IF NOT EXISTS idx_feed_items_sub_pub ON feed_items(subscription_id, published_at DESC)",
        "CREATE INDEX IF NOT EXISTS idx_feed_items_published ON feed_items(published_at DESC)",
        "CREATE INDEX IF NOT EXISTS idx_feed_items_fav_pub ON feed_items(is_favorite, published_at DESC)",
        "CREATE INDEX IF NOT EXISTS idx_feed_items_rl_pub ON feed_items(is_read_later, published_at DESC)",
    ];
    for ddl in indexes {
        sqlx::query(ddl)
            .execute(&mut *conn)
            .await
            .map_err(AppError::Database)?;
    }

    // The unique dedup index must not fail silently: without it the fetch
    // pipeline loses its last line of defense against duplicate rows.
    let unique = "CREATE UNIQUE INDEX IF NOT EXISTS idx_feed_items_guid ON feed_items(subscription_id, guid)";
    sqlx::query(unique)
        .execute(&mut *conn)
        .await
        .map_err(AppError::Database)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::run_migrations;

    #[tokio::test]
    async fn legacy_schema_gets_missing_columns_and_indexes() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("create test database");

        sqlx::query(
            "CREATE TABLE subscriptions (id INTEGER PRIMARY KEY, url TEXT NOT NULL UNIQUE)",
        )
        .execute(&pool)
        .await
        .expect("create legacy subscriptions table");
        sqlx::query(
            "CREATE TABLE feed_items (
                id INTEGER PRIMARY KEY,
                subscription_id INTEGER NOT NULL,
                guid TEXT,
                title TEXT NOT NULL,
                link TEXT,
                content TEXT,
                description TEXT,
                author TEXT,
                published_at DATETIME,
                fetched_at DATETIME,
                is_website_content BOOLEAN DEFAULT 0,
                is_read BOOLEAN DEFAULT 0,
                is_favorite BOOLEAN DEFAULT 0,
                is_read_later BOOLEAN DEFAULT 0,
                tags TEXT
            )",
        )
        .execute(&pool)
        .await
        .expect("create legacy feed_items table");
        sqlx::query(
            "INSERT INTO feed_items (subscription_id, title, tags) VALUES (1, 'legacy', ?)",
        )
        .bind(r#"["Machine Learning","machine-learning","AI"]"#)
        .execute(&pool)
        .await
        .expect("seed legacy tagged item");

        run_migrations(&pool)
            .await
            .expect("legacy migration should complete");

        let content_md: Option<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info('feed_items') WHERE name = 'content_md'",
        )
        .fetch_optional(&pool)
        .await
        .expect("inspect content_md");
        let auto_classify: Option<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info('subscriptions') WHERE name = 'auto_classify'",
        )
        .fetch_optional(&pool)
        .await
        .expect("inspect auto_classify");
        let index: Option<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND name = 'idx_feed_items_guid'",
        )
        .fetch_optional(&pool)
        .await
        .expect("inspect unique index");

        assert_eq!(content_md.as_deref(), Some("content_md"));
        assert_eq!(auto_classify.as_deref(), Some("auto_classify"));
        assert_eq!(index.as_deref(), Some("idx_feed_items_guid"));

        let normalized_tags: (String,) = sqlx::query_as("SELECT tags FROM feed_items WHERE id = 1")
            .fetch_one(&pool)
            .await
            .expect("inspect migrated tags");
        assert_eq!(normalized_tags.0, r#"["machine_learning","ai"]"#);
        let catalog_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tag_catalog")
            .fetch_one(&pool)
            .await
            .expect("inspect tag catalog");
        assert_eq!(catalog_count.0, 2);

        let database_id: String =
            sqlx::query_scalar("SELECT value FROM app_metadata WHERE key = 'database_id'")
                .fetch_one(&pool)
                .await
                .expect("inspect database identity");
        assert_eq!(database_id.len(), 32);
    }
}

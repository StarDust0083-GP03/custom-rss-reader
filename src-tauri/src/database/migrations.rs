use sqlx::{SqlitePool, Executor};

pub async fn run_migrations(pool: &SqlitePool) -> super::Result<()> {
    // Ensure tables exist
    let tables_exist = sqlx::query(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='feed_items'"
    )
    .fetch_optional(pool)
    .await?
    .is_some();

    if !tables_exist {
        // Create tables from scratch
        create_tables(pool).await?;
    } else {
        // Add new columns if they don't exist
        add_missing_columns(pool).await?;
    }

    // Ensure indexes exist
    let _ = create_indexes(pool).await;

    Ok(())
}

async fn create_tables(pool: &SqlitePool) -> super::Result<()> {
    // Create subscriptions table
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
        "#
    )
    .await?;

    // Create feed_items table with all columns
    pool.execute(
        r#"
        CREATE TABLE feed_items (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            subscription_id INTEGER NOT NULL,
            guid TEXT,
            title TEXT NOT NULL,
            link TEXT,
            content TEXT,
            description TEXT,
            author TEXT,
            published_at DATETIME,
            fetched_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            is_website_content BOOLEAN DEFAULT 0,
            is_read BOOLEAN DEFAULT 0,
            is_favorite BOOLEAN DEFAULT 0,
            is_read_later BOOLEAN DEFAULT 0,
            tags TEXT,
            category TEXT,
            translated_title TEXT,
            translated_content TEXT,
            translated_at DATETIME,
            FOREIGN KEY (subscription_id) REFERENCES subscriptions(id) ON DELETE CASCADE
        )
        "#
    )
    .await?;

    Ok(())
}

async fn add_missing_columns(pool: &SqlitePool) -> super::Result<()> {
    // Check and add tags column
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

    // Check and add category column
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

    // Check and add translated_title column
    let has_translated_title = sqlx::query("SELECT translated_title FROM feed_items LIMIT 1")
        .fetch_optional(pool)
        .await
        .is_ok();

    if !has_translated_title {
        sqlx::query("ALTER TABLE feed_items ADD COLUMN translated_title TEXT")
            .execute(pool)
            .await
            .ok();
    }

    // Check and add translated_content column
    let has_translated_content = sqlx::query("SELECT translated_content FROM feed_items LIMIT 1")
        .fetch_optional(pool)
        .await
        .is_ok();

    if !has_translated_content {
        sqlx::query("ALTER TABLE feed_items ADD COLUMN translated_content TEXT")
            .execute(pool)
            .await
            .ok();
    }

    // Check and add translated_at column
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

    // Check and add auto_classify column to subscriptions
    let has_auto_classify = sqlx::query("SELECT auto_classify FROM subscriptions LIMIT 1")
        .fetch_optional(pool)
        .await
        .is_ok();

    if !has_auto_classify {
        sqlx::query("ALTER TABLE subscriptions ADD COLUMN auto_classify BOOLEAN DEFAULT 1")
            .execute(pool)
            .await
            .ok();
    }

    Ok(())
}

async fn create_indexes(pool: &SqlitePool) -> super::Result<()> {
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

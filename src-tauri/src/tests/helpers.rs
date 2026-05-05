use std::sync::Arc;

use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

use crate::models::{NewSubscription, Subscription};
use crate::repositories::feed_item_repo::SqliteFeedItemRepository;
use crate::repositories::{FeedItemRepository, SubscriptionRepository};
use crate::repositories::subscription_repo::SqliteSubscriptionRepository;
use crate::{FeedService, SubscriptionService};

/// Test environment holding all dependencies.
/// The pool is kept alive for the lifetime of the test.
pub struct TestEnv {
    pub service: SubscriptionService,
    pub feed_service: FeedService,
    pub repo: Arc<dyn SubscriptionRepository>,
    pub feed_repo: Arc<dyn FeedItemRepository>,
    #[allow(dead_code)]
    pub pool: SqlitePool,
}

impl TestEnv {
    /// Create a fresh in-memory SQLite database, apply migrations,
    /// and build the full repository + service stack.
    pub async fn new() -> Self {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("Failed to create in-memory SQLite database");

        // Create the subscriptions table (same DDL as the real app).
        sqlx::query(
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
        .execute(&pool)
        .await
        .expect("Failed to create subscriptions table");

        // Create the feed_items table (same DDL as the real app, plus content_md).
        sqlx::query(
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
        .execute(&pool)
        .await
        .expect("Failed to create feed_items table");

        let sub_repo: Arc<dyn SubscriptionRepository> =
            Arc::new(SqliteSubscriptionRepository::new(pool.clone()));
        let feed_repo: Arc<dyn FeedItemRepository> =
            Arc::new(SqliteFeedItemRepository::new(pool.clone()));
        let service = SubscriptionService::new(sub_repo.clone());
        let feed_service = FeedService::new(feed_repo.clone());

        TestEnv {
            service,
            feed_service,
            repo: sub_repo,
            feed_repo,
            pool,
        }
    }
}

/// Convenience helper to build a `NewSubscription` with default values.
pub fn new_sub(url: &str) -> NewSubscription {
    NewSubscription {
        url: url.to_string(),
        title: None,
        website_url: None,
        rsshub_url: None,
        use_website: false,
        auto_classify: true,
        opml_attributes: None,
    }
}

pub async fn seed_subscription(env: &TestEnv, url: &str, _title: &str) -> Subscription {
    env.repo
        .create(new_sub(url))
        .await
        .expect("Failed to seed subscription")
}

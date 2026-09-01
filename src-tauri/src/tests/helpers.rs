use std::sync::Arc;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::database::migrations::run_migrations;
use crate::models::{NewSubscription, Subscription};
use crate::repositories::feed_item_repo::SqliteFeedItemRepository;
use crate::repositories::subscription_repo::SqliteSubscriptionRepository;
use crate::repositories::{FeedItemRepository, SubscriptionRepository};
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
    /// Create a fresh in-memory SQLite database, run the production
    /// migrations against it, and build the full repository + service
    /// stack. Using the real migrations keeps the test DDL in sync with
    /// the app — a missing column in the helper is impossible by
    /// construction.
    pub async fn new() -> Self {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .foreign_keys(true),
            )
            .await
            .expect("Failed to create in-memory SQLite database");

        run_migrations(&pool)
            .await
            .expect("Failed to run migrations on test database");

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

use std::sync::Arc;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::database::migrations::run_migrations;
use crate::error::Result;
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

/// An AI service that answers immediately, so tests never touch a model.
///
/// `FakeAi::failing()` makes `classify_batch` return an error, which is how the
/// worker's retry path is exercised.
pub struct FakeAi {
    fail: bool,
    /// Number of `classify_batch` calls, so tests can prove batching.
    pub calls: std::sync::atomic::AtomicUsize,
}

impl FakeAi {
    pub fn ok() -> Arc<Self> {
        Arc::new(Self {
            fail: false,
            calls: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    pub fn failing() -> Arc<Self> {
        Arc::new(Self {
            fail: true,
            calls: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    pub fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl crate::ai::service::AiService for FakeAi {
    async fn translate_bilingual(&self, _: &str, _: &str, _: &str) -> Result<String> {
        Ok(String::new())
    }

    async fn translate_block(&self, _: &str, _: &str, _: &str, _: bool) -> Result<String> {
        Ok(String::new())
    }

    async fn classify(
        &self,
        _: crate::ai::ClassificationRequest,
    ) -> Result<crate::ai::ClassificationResponse> {
        Ok(crate::ai::ClassificationResponse {
            tags: vec!["fake".into()],
        })
    }

    async fn classify_batch(
        &self,
        entries: &[crate::ai::BatchClassifyEntry],
        _existing_tags: &[String],
    ) -> Result<Vec<crate::ai::ClassificationResponse>> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail {
            return Err(crate::error::AppError::Network("provider down".into()));
        }
        // One response per entry, in order — the contract the worker relies on.
        Ok(entries
            .iter()
            .map(|_| crate::ai::ClassificationResponse {
                tags: vec!["machine_learning".into()],
            })
            .collect())
    }

    async fn recommend_reads(
        &self,
        _: &[crate::ai::RecommendCandidate],
    ) -> Result<Vec<crate::ai::Recommendation>> {
        Ok(Vec::new())
    }

    async fn explain_tags(&self, names: &[String]) -> Result<Vec<crate::ai::TagExplanation>> {
        Ok(names
            .iter()
            .map(|name| crate::ai::TagExplanation {
                name: name.clone(),
                explanation: format!("definition of {name}"),
            })
            .collect())
    }

    async fn suggest_topics(
        &self,
        catalog: &[crate::ai::TopicChoice],
        words: &[crate::ai::TopicWordInput],
    ) -> Result<Vec<crate::ai::TopicSuggestion>> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail {
            return Err(crate::error::AppError::Network("provider down".into()));
        }
        if catalog.is_empty() {
            return Ok(Vec::new());
        }
        // Every word lands in the first topic, which is enough for a caller to
        // test staging and saving without a live model.
        Ok(words
            .iter()
            .map(|word| crate::ai::TopicSuggestion {
                name: word.name.clone(),
                category_id: Some(catalog[0].id),
                state: "assigned".to_string(),
                reason: "fake".to_string(),
            })
            .collect())
    }

    async fn test_connection(&self) -> Result<String> {
        Ok("ok".into())
    }

    fn config_max_chars(&self) -> usize {
        1000
    }

    fn config_model(&self) -> String {
        "fake-model".into()
    }
}

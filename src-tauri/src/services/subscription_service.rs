use std::sync::Arc;

use crate::error::Result;
use crate::models::{NewSubscription, Subscription, UpdateSubscription};
use crate::repositories::SubscriptionRepository;

/// Business logic layer for subscriptions.
///
/// Receives the repository via trait object, making it testable
/// without a real database. Contains all domain rules:
/// - Duplicate prevention (checked before insert)
/// - Cross-field validation
/// - Any side-effect orchestration
pub struct SubscriptionService {
    repo: Arc<dyn SubscriptionRepository>,
}

impl SubscriptionService {
    pub fn new(repo: Arc<dyn SubscriptionRepository>) -> Self {
        Self { repo }
    }

    /// Add a new subscription.
    /// Checks for duplicates before delegating to the repository.
    pub async fn add_subscription(&self, input: NewSubscription) -> Result<Subscription> {
        // Validate input
        input.validate()?;

        // Business rule: prevent duplicate URLs
        if self.repo.exists_by_url(&input.url).await? {
            return Err(crate::error::AppError::Duplicate(format!(
                "Subscription with URL '{}' already exists",
                input.url
            )));
        }

        self.repo.create(input).await
    }

    /// Get a single subscription by ID.
    pub async fn get_subscription(&self, id: i64) -> Result<Subscription> {
        self.repo.find_by_id(id).await
    }

    /// List all subscriptions.
    pub async fn list_subscriptions(&self) -> Result<Vec<Subscription>> {
        self.repo.find_all().await
    }

    /// Update an existing subscription.
    pub async fn update_subscription(
        &self,
        id: i64,
        input: UpdateSubscription,
    ) -> Result<Subscription> {
        // Business rule: website_url must be a valid URL if provided
        if let Some(ref url) = input.website_url {
            if !url.is_empty() && !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(crate::error::AppError::Validation(
                    "website_url must start with http:// or https://".into(),
                ));
            }
        }
        if let Some(ref url) = input.rsshub_url {
            if !url.is_empty() && !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(crate::error::AppError::Validation(
                    "rsshub_url must start with http:// or https://".into(),
                ));
            }
        }

        self.repo.update(id, input).await
    }

    /// Remove a subscription by ID.
    pub async fn remove_subscription(&self, id: i64) -> Result<()> {
        self.repo.delete(id).await
    }

    /// Toggle use_website flag.
    pub async fn toggle_use_website(&self, id: i64) -> Result<Subscription> {
        self.repo.toggle_use_website(id).await
    }

    /// Toggle auto_classify flag.
    pub async fn toggle_auto_classify(&self, id: i64) -> Result<Subscription> {
        self.repo.toggle_auto_classify(id).await
    }
}

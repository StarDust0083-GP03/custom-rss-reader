use std::sync::Arc;

use crate::error::{AppError, Result};
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

/// Validate an http(s) URL with the same parser used by reqwest at fetch
/// time, rejecting malformed hosts instead of only checking the scheme.
fn validate_http_url(url: &str, field_name: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url).map_err(|_| {
        AppError::Validation(format!(
            "{} must be a valid http(s) URL with a host",
            field_name
        ))
    })?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(AppError::Validation(format!(
            "{} must be a valid http(s) URL with a host",
            field_name
        )));
    }
    Ok(())
}

/// Normalize a clearable URL field: empty string means "clear to NULL",
/// otherwise validate and trim in place.
fn normalize_clearable_url(field: &mut Option<Option<String>>, field_name: &str) -> Result<()> {
    if let Some(Some(url)) = field {
        let trimmed = url.trim().to_string();
        if trimmed.is_empty() {
            *field = Some(None); // empty input clears the column
        } else {
            validate_http_url(&trimmed, field_name)?;
            *url = trimmed;
        }
    }
    Ok(())
}

/// Normalize a plain optional URL on the add path: trim, empty → None,
/// otherwise validate. (`normalize_clearable_url` handles the double-option
/// used by the update path.)
fn normalize_optional_url(field: &mut Option<String>, field_name: &str) -> Result<()> {
    if let Some(url) = field {
        let trimmed = url.trim().to_string();
        if trimmed.is_empty() {
            *field = None;
        } else {
            validate_http_url(&trimmed, field_name)?;
            *url = trimmed;
        }
    }
    Ok(())
}

impl SubscriptionService {
    pub fn new(repo: Arc<dyn SubscriptionRepository>) -> Self {
        Self { repo }
    }

    /// Add a new subscription.
    /// Checks for duplicates before delegating to the repository.
    pub async fn add_subscription(&self, mut input: NewSubscription) -> Result<Subscription> {
        // Normalize the URL BEFORE validate/dedup/insert: trim whitespace and
        // assume https:// when the scheme is entirely missing (users commonly
        // paste bare "example.com/rss.xml"). A non-http scheme like
        // "ftp://..." is preserved so validate() can reject it. Previously
        // validation checked a trimmed copy while the raw string was stored
        // — a URL with stray whitespace passed validation but then failed
        // to fetch.
        input.url = input.url.trim().to_string();
        if !input.url.is_empty() && !input.url.contains("://") {
            input.url = format!("https://{}", input.url);
        }
        normalize_optional_url(&mut input.website_url, "website_url")?;
        normalize_optional_url(&mut input.rsshub_url, "rsshub_url")?;

        // Validate input
        input.validate()?;

        // Business rule: prevent duplicate URLs
        if self.repo.exists_by_url(&input.url).await? {
            return Err(AppError::Duplicate(format!(
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
        mut input: UpdateSubscription,
    ) -> Result<Subscription> {
        normalize_clearable_url(&mut input.website_url, "website_url")?;
        normalize_clearable_url(&mut input.rsshub_url, "rsshub_url")?;

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

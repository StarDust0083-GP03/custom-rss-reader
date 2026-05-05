use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

/// Domain model for a subscription.
/// This is the public contract — distinct from database row types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub id: i64,
    pub url: String,
    pub title: Option<String>,
    pub website_url: Option<String>,
    pub rsshub_url: Option<String>,
    pub use_website: bool,
    pub auto_classify: bool,
    pub opml_attributes: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Input for creating a new subscription.
/// Validation is applied at the boundary via `validate()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewSubscription {
    pub url: String,
    pub title: Option<String>,
    pub website_url: Option<String>,
    pub rsshub_url: Option<String>,
    pub use_website: bool,
    pub auto_classify: bool,
    pub opml_attributes: Option<String>,
}

/// Input for updating an existing subscription.
/// `None` fields are left unchanged by the repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateSubscription {
    pub title: Option<String>,
    pub website_url: Option<String>,
    pub use_website: Option<bool>,
    pub rsshub_url: Option<String>,
}

impl Default for NewSubscription {
    fn default() -> Self {
        Self {
            url: String::new(),
            title: None,
            website_url: None,
            rsshub_url: None,
            use_website: false,
            auto_classify: true,
            opml_attributes: None,
        }
    }
}

impl NewSubscription {
    /// Validate required fields and constraints.
    /// Called by the repository before inserting.
    pub fn validate(&self) -> Result<()> {
        if self.url.trim().is_empty() {
            return Err(AppError::Validation("URL cannot be empty".into()));
        }
        if !self.url.starts_with("http://") && !self.url.starts_with("https://") {
            return Err(AppError::Validation(
                "URL must start with http:// or https://".into(),
            ));
        }
        Ok(())
    }
}

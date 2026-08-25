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
///
/// Optional fields use double-option semantics:
/// - `None` (field absent in the request) → leave unchanged
/// - `Some(None)` (explicit `null`) → clear the column to NULL
/// - `Some(Some(v))` → set to `v`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateSubscription {
    pub title: Option<String>,
    #[serde(default, deserialize_with = "crate::models::de_double_option")]
    pub website_url: Option<Option<String>>,
    pub use_website: Option<bool>,
    #[serde(default, deserialize_with = "crate::models::de_double_option")]
    pub rsshub_url: Option<Option<String>>,
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
        let url = self.url.trim();
        if url.is_empty() {
            return Err(AppError::Validation("URL cannot be empty".into()));
        }
        let parsed = reqwest::Url::parse(url).map_err(|_| {
            AppError::Validation("URL must be a valid http(s) URL with a host".into())
        })?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(AppError::Validation(
                "URL must be a valid http(s) URL with a host".into(),
            ));
        }
        Ok(())
    }
}

use serde::{Deserialize, Serialize};

use crate::error::AppError;

/// AI configuration for LLM API calls.
///
/// `#[serde(default)]` on the struct so config files written by older
/// versions of the app (missing fields added later, e.g.
/// `max_chars_per_segment`) still load — a legacy file must not brick the
/// AI features until the user re-saves the settings.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AiConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub max_chars_per_segment: Option<usize>,
}

impl AiConfig {
    pub fn is_valid(&self) -> Result<(), AppError> {
        if self.api_key.is_empty() {
            return Err(AppError::Validation("API key cannot be empty".into()));
        }
        if self.api_key.contains("****") {
            return Err(AppError::Validation(
                "API key is masked; enter the real key again".into(),
            ));
        }
        if self.base_url.is_empty() {
            return Err(AppError::Validation("Base URL cannot be empty".into()));
        }
        if self.model.is_empty() {
            return Err(AppError::Validation("Model name cannot be empty".into()));
        }
        Ok(())
    }

    /// Construct a default config without an API key (for "not yet configured"
    /// responses, where the key must remain empty).
    pub fn default_for(base_url: &str, model: &str) -> Self {
        Self {
            api_key: String::new(),
            base_url: base_url.to_string(),
            model: model.to_string(),
            max_tokens: None,
            temperature: None,
            max_chars_per_segment: None,
        }
    }
}

/// Request payload for AI classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassificationRequest {
    pub title: String,
    pub description: Option<String>,
    pub content_snippet: Option<String>,
    pub rss_title: Option<String>,
    pub existing_tags: Option<Vec<String>>,
}

/// Response from AI classification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClassificationResponse {
    pub tags: Vec<String>,
    pub category: Option<String>,
}

/// One entry in a batch classification request.
///
/// Title only — batch auto-classify runs on every freshly fetched item, so
/// payload size directly drives both token cost and rate-limit pressure.
/// Neither the description nor `content` is sent.
#[derive(Debug, Clone)]
pub struct BatchClassifyEntry {
    /// Position of this entry in the batch (echoed by the LLM response).
    pub index: usize,
    pub title: String,
}

/// One candidate for the read-recommendation feature.
#[derive(Debug, Clone)]
pub struct RecommendCandidate {
    pub item_id: i64,
    /// Pre-formatted context line (source, title, snippet) built by the caller.
    pub context: String,
}

/// A single recommendation picked by the LLM.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Recommendation {
    pub item_id: i64,
    pub reason: String,
}

/// Max characters per translation segment.
pub const MAX_CHARS_PER_SEGMENT: usize = 3000;
/// Max retry attempts for LLM calls.
pub const MAX_RETRIES: usize = 2;
/// Max articles per batch classification call.
pub const CLASSIFY_BATCH_SIZE: usize = 20;
/// Number of picks the recommendation prompt asks for.
pub const RECOMMEND_PICK_COUNT: usize = 5;

pub mod activity;
pub mod service;

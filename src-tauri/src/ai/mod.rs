use serde::{Deserialize, Serialize};

use crate::error::AppError;

/// AI configuration for LLM API calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
        if self.base_url.is_empty() {
            return Err(AppError::Validation("Base URL cannot be empty".into()));
        }
        if self.model.is_empty() {
            return Err(AppError::Validation("Model name cannot be empty".into()));
        }
        Ok(())
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

/// Max characters per translation segment.
pub const MAX_CHARS_PER_SEGMENT: usize = 3000;
/// Max retry attempts for LLM calls.
pub const MAX_RETRIES: usize = 2;

pub mod service;

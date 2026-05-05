use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::ai::{AiConfig, ClassificationRequest, ClassificationResponse};
use crate::ai::service::{AiService, LlmAiService};
use crate::error::{AppError, Result};

use super::AppState;

/// Default AI configuration values.
const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";
const DEFAULT_MODEL: &str = "deepseek-chat";
/// Translation cache expiry: 3 days.
const TRANSLATION_CACHE_DAYS: i64 = 3;

/// Response struct for AI config queries (masks API key length).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiConfigResponse {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_chars_per_segment: Option<usize>,
}

// ---- Translation ----

#[tauri::command]
pub async fn translate_item_bilingual(
    state: State<'_, AppState>,
    item_id: i64,
) -> Result<String> {
    let item = state.feed_repo.find_by_id(item_id).await?;

    // Check cache validity
    if let Some(ref translated_content) = item.translated_content {
        if let Some(translated_at) = item.translated_at {
            let cache_age = Utc::now() - translated_at;
            if cache_age.num_days() < TRANSLATION_CACHE_DAYS {
                return Ok(format!(
                    r#"<div class="bilingual-content" data-cached="true">{}</div>"#,
                    translated_content
                ));
            }
        }
    }

    let content = item
        .content
        .as_ref()
        .or(item.description.as_ref())
        .ok_or_else(|| AppError::OperationFailed("No content to translate".into()))?;

    let ai_service = get_ai_service(&state).await?;
    let bilingual = ai_service.translate_bilingual(content, "auto", "zh-CN").await?;

    let result = format!(
        r#"<div class="bilingual-content" data-cached="false">{}</div>"#,
        bilingual
    );

    // Save to database
    let now = Utc::now();
    state
        .feed_repo
        .find_by_id(item_id)
        .await?;
    // Use a direct query to update translation fields
    let pool = &state.pool;
    sqlx::query(
        "UPDATE feed_items SET translated_content = $1, translated_at = $2 WHERE id = $3",
    )
    .bind(&result)
    .bind(&now)
    .bind(item_id)
    .execute(pool)
    .await
    .map_err(|e| AppError::Database(e))?;

    Ok(result)
}

#[tauri::command]
pub async fn translate_content_bilingual(
    state: State<'_, AppState>,
    content: String,
    source_lang: String,
    target_lang: String,
) -> Result<String> {
    let ai_service = get_ai_service(&state).await?;
    let bilingual = ai_service
        .translate_bilingual(&content, &source_lang, &target_lang)
        .await?;

    Ok(format!(
        r#"<div class="bilingual-content">{}</div>"#,
        bilingual
    ))
}

#[tauri::command]
pub async fn classify_item(
    state: State<'_, AppState>,
    title: String,
    description: Option<String>,
    content_snippet: Option<String>,
    rss_title: Option<String>,
    existing_tags: Option<Vec<String>>,
) -> Result<ClassificationResponse> {
    let ai_service = get_ai_service(&state).await?;

    let request = ClassificationRequest {
        title,
        description,
        content_snippet,
        rss_title,
        existing_tags,
    };

    Ok(ai_service.classify(request).await?)
}

// ---- Config management ----

#[tauri::command]
pub async fn set_ai_config(
    api_key: String,
    base_url: Option<String>,
    model: Option<String>,
    skip_test: Option<bool>,
    max_chars_per_segment: Option<usize>,
) -> Result<()> {
    let config = AiConfig {
        api_key,
        base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
        model: model.unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        max_tokens: Some(4000),
        temperature: Some(0.3),
        max_chars_per_segment,
    };

    let skip = skip_test.unwrap_or(false);
    if !skip {
        let test_service = LlmAiService::new(config.clone())?;
        test_service.test_connection().await.map_err(|e| {
            AppError::Network(format!(
                "API connection test failed: {}. Please check your base URL and model name.", e
            ))
        })?;
    }

    save_ai_config(&config)?;
    Ok(())
}

#[tauri::command]
pub async fn get_ai_config() -> Result<AiConfigResponse> {
    let home_dir = dirs::home_dir()
        .ok_or_else(|| AppError::Internal("Failed to get home directory".into()))?;
    let config_file = home_dir.join(".rss-reader").join("ai_config.json");

    if config_file.exists() {
        let content = std::fs::read_to_string(&config_file)
            .map_err(|e| AppError::Internal(format!("Failed to read config file: {}", e)))?;
        let config: AiConfig = serde_json::from_str(&content)
            .map_err(|e| AppError::Internal(format!("Failed to parse config file: {}", e)))?;

        Ok(AiConfigResponse {
            api_key: config.api_key,
            base_url: config.base_url,
            model: config.model,
            max_chars_per_segment: config.max_chars_per_segment,
        })
    } else {
        Ok(AiConfigResponse {
            api_key: String::new(),
            base_url: DEFAULT_BASE_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
            max_chars_per_segment: None,
        })
    }
}

// ---- Helpers ----

async fn get_ai_service(state: &AppState) -> Result<Arc<dyn AiService>> {
    if let Some(ref service) = state.ai_service {
        return Ok(Arc::clone(service));
    }

    // Fall back to loading from file
    let config = load_ai_config()?;
    let service = Arc::new(LlmAiService::new(config)?);
    Ok(service)
}

fn load_ai_config() -> Result<AiConfig> {
    let home_dir = dirs::home_dir()
        .ok_or_else(|| AppError::Internal("Failed to get home directory".into()))?;
    let config_file = home_dir.join(".rss-reader").join("ai_config.json");

    if config_file.exists() {
        let content = std::fs::read_to_string(&config_file)
            .map_err(|e| AppError::Internal(format!("Failed to read config file: {}", e)))?;
        let config: AiConfig = serde_json::from_str(&content)
            .map_err(|e| AppError::Internal(format!("Failed to parse config file: {}", e)))?;
        Ok(config)
    } else {
        Err(AppError::OperationFailed(
            "No AI configuration found. Please configure API key first.".into(),
        ))
    }
}

fn save_ai_config(config: &AiConfig) -> Result<()> {
    let home_dir = dirs::home_dir()
        .ok_or_else(|| AppError::Internal("Failed to get home directory".into()))?;
    let app_dir = home_dir.join(".rss-reader");

    std::fs::create_dir_all(&app_dir)
        .map_err(|e| AppError::Internal(format!("Failed to create config dir: {}", e)))?;

    let config_file = app_dir.join("ai_config.json");
    let config_json = serde_json::to_string_pretty(config)
        .map_err(|e| AppError::Internal(format!("Failed to serialize config: {}", e)))?;

    std::fs::write(&config_file, config_json)
        .map_err(|e| AppError::Internal(format!("Failed to write config file: {}", e)))?;

    Ok(())
}

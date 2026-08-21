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

/// Response struct for AI config queries. The API key is masked on the way
/// out so a compromised WebView can never read the plaintext.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiConfigResponse {
    /// API key with the middle replaced by `*`. e.g. `sk-****1234`.
    /// An empty string means "no key configured".
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

    // Persist via the repository (no direct SQL in the command layer).
    state
        .feed_repo
        .update_translation(item_id, None, &result)
        .await?;

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

    ai_service.classify(request).await
}

// ---- Read recommendations (manual trigger, first version) ----

/// A recommendation row as consumed by the frontend — enriched with the
/// display fields so the UI needs no second round trip.
#[derive(Debug, Clone, Serialize)]
pub struct RecommendationResponse {
    pub item_id: i64,
    pub title: String,
    pub link: Option<String>,
    pub source: String,
    pub reason: String,
}

/// How many candidate articles feed the recommendation prompt.
const RECOMMEND_MAX_CANDIDATES: i64 = 60;
/// Snippet length per candidate (characters, after tag stripping).
const RECOMMEND_SNIPPET_CHARS: usize = 140;

/// Manually triggered: pick the most worthwhile reads with ONE LLM call.
///
/// Candidates are the most recent unread items (falling back to recent items
/// regardless of read state when nothing is unread). Each candidate
/// contributes source + title + a 140-char plain-text snippet — full article
/// bodies are deliberately never sent.
#[tauri::command]
pub async fn recommend_reads(
    state: State<'_, AppState>,
) -> Result<Vec<RecommendationResponse>> {
    let mut summaries = state
        .feed_repo
        .get_unread(None, RECOMMEND_MAX_CANDIDATES, 0)
        .await?;
    if summaries.is_empty() {
        // Nothing unread — recommend from the most recent items instead.
        summaries = state
            .feed_repo
            .find_all(None, RECOMMEND_MAX_CANDIDATES, 0)
            .await?;
    }
    if summaries.is_empty() {
        return Ok(Vec::new());
    }

    // Source names give the model useful context (feed ≠ article quality).
    let subscriptions = state.subscription_service.list_subscriptions().await?;
    let source_of = |sub_id: i64| -> String {
        subscriptions
            .iter()
            .find(|s| s.id == sub_id)
            .map(|s| s.title.clone().unwrap_or_else(|| s.url.clone()))
            .unwrap_or_else(|| "Unknown".into())
    };

    let candidates: Vec<crate::ai::RecommendCandidate> = summaries
        .iter()
        .map(|s| {
            let mut context = format!("{} — {}", source_of(s.subscription_id), s.title);
            if let Some(desc) = s.description.as_deref() {
                let plain = crate::chroma::service::strip_html_tags(desc);
                let snippet: String = plain.chars().take(RECOMMEND_SNIPPET_CHARS).collect();
                if !snippet.trim().is_empty() {
                    context.push_str(" — ");
                    context.push_str(snippet.trim());
                }
            }
            crate::ai::RecommendCandidate {
                item_id: s.id,
                context,
            }
        })
        .collect();

    let ai_service = get_ai_service(&state).await?;
    let picks = ai_service.recommend_reads(&candidates).await?;

    // Map picks back to their summaries (LLM order = ranking order).
    let out = picks
        .iter()
        .filter_map(|p| {
            summaries
                .iter()
                .find(|s| s.id == p.item_id)
                .map(|s| RecommendationResponse {
                    item_id: p.item_id,
                    title: s.title.clone(),
                    link: s.link.clone(),
                    source: source_of(s.subscription_id),
                    reason: p.reason.clone(),
                })
        })
        .collect();
    Ok(out)
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
    let config = load_ai_config().unwrap_or_default();
    Ok(AiConfigResponse {
        api_key: mask_api_key(&config.api_key),
        base_url: config.base_url,
        model: config.model,
        max_chars_per_segment: config.max_chars_per_segment,
    })
}

// ---- Helpers ----

/// Build a cached AI service if one is registered in state, else load from
/// the config file. (Caching the constructed service across calls is a
/// longer-term improvement — the AppState.ai_service field already exists
/// for this purpose.)
async fn get_ai_service(state: &AppState) -> Result<Arc<dyn AiService>> {
    if let Some(ref service) = state.ai_service {
        return Ok(Arc::clone(service));
    }
    let config = load_ai_config()?;
    Ok(Arc::new(LlmAiService::new(config)?))
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
        Ok(AiConfig::default_for(DEFAULT_BASE_URL, DEFAULT_MODEL))
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

/// Mask an API key for safe transport to the frontend.
/// - Empty string → empty
/// - ≤ 8 chars     → first 2 + "****"
/// - otherwise      → first 4 + "****" + last 4
fn mask_api_key(key: &str) -> String {
    if key.is_empty() {
        return String::new();
    }
    if key.len() <= 8 {
        let visible = key.chars().take(2).collect::<String>();
        return format!("{}****", visible);
    }
    let chars: Vec<char> = key.chars().collect();
    let head: String = chars.iter().take(4).collect();
    let tail: String = chars.iter().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{}****{}", head, tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_api_key_empty() {
        assert_eq!(mask_api_key(""), "");
    }

    #[test]
    fn test_mask_api_key_short() {
        assert_eq!(mask_api_key("abcd"), "ab****");
        assert_eq!(mask_api_key("12345678"), "12****");
    }

    #[test]
    fn test_mask_api_key_long() {
        assert_eq!(mask_api_key("sk-proj-1234567890abcdef"), "sk-p****cdef");
    }
}
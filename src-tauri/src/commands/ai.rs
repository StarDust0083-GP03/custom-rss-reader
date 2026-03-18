use crate::ai::{AiService, AiConfig, ClassificationRequest};
use tauri::{AppHandle, Manager, Emitter};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use dirs;
use std::fs::OpenOptions;
use std::io::Write;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslationProgress {
    pub total: usize,
    pub completed: usize,
    pub html_chunk: String,
    pub is_complete: bool,
}

// Default configuration
const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";
const DEFAULT_MODEL: &str = "deepseek-chat";
// Translation cache expiry: 3 days
const TRANSLATION_CACHE_DAYS: i64 = 3;

/// Write log message to file
fn log_to_file(message: &str) {
    if let Some(home_dir) = dirs::home_dir() {
        let app_dir = home_dir.join(".rss-reader");
        let _ = std::fs::create_dir_all(&app_dir);
        let log_file = app_dir.join("ai_errors.log");

        let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
        let log_entry = format!("[{}] {}\n", timestamp, message);

        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
        {
            let _ = file.write_all(log_entry.as_bytes());
        }
    }
}

#[tauri::command]
pub async fn translate_item_bilingual(
    app_handle: AppHandle,
    pool: tauri::State<'_, sqlx::SqlitePool>,
    item_id: i64,
) -> Result<String, String> {
    // Get item from database
    let item: crate::database::schema::FeedItem = sqlx::query_as::<_, _>("SELECT * FROM feed_items WHERE id = $1")
        .bind(item_id)
        .fetch_one(pool.inner())
        .await
        .map_err(|e| format!("Failed to get item: {}", e))?;

    // Check if we have a cached translation that's still valid
    if let Some(translated_content) = &item.translated_content {
        if let Some(translated_at) = item.translated_at {
            let cache_age = Utc::now() - translated_at;
            if cache_age.num_days() < TRANSLATION_CACHE_DAYS {
                // Cache is still valid, wrap in bilingual container
                return Ok(format!(
                    r#"<div class="bilingual-content" data-cached="true">{}</div>"#,
                    translated_content
                ));
            }
        }
    }

    // Need to fetch new translation
    let content = item.content.as_ref()
        .or(item.description.as_ref())
        .ok_or_else(|| "No content to translate".to_string())?;

    let config = get_ai_config_internal(&app_handle).await?;
    let ai_service = AiService::new(config)?;

    let bilingual = ai_service.translate_bilingual_segmented(
        content,
        "auto",
        "zh-CN",
    ).await?;

    let result = format!(
        r#"<div class="bilingual-content" data-cached="false">{}</div>"#,
        bilingual
    );

    // Save to database with timestamp
    let now = Utc::now();
    sqlx::query(
        "UPDATE feed_items SET translated_content = $1, translated_at = $2 WHERE id = $3"
    )
    .bind(&result)
    .bind(&now)
    .bind(item_id)
    .execute(pool.inner())
    .await
    .map_err(|e| format!("Failed to save translation: {}", e))?;

    Ok(result)
}

#[tauri::command]
pub async fn translate_content_bilingual(
    app_handle: AppHandle,
    content: String,
    source_lang: String,
    target_lang: String,
) -> Result<String, String> {
    let config = get_ai_config_internal(&app_handle).await?;
    let ai_service = AiService::new(config)?;

    let bilingual = ai_service.translate_bilingual_segmented(
        &content,
        &source_lang,
        &target_lang,
    ).await?;

    Ok(format!(
        r#"<div class="bilingual-content">
{}
</div>"#,
        bilingual
    ))
}

/// Translate HTML content with streaming progress events
/// Used for translating website content loaded in webview
#[tauri::command]
pub async fn translate_html_content_streaming(
    app_handle: AppHandle,
    pool: tauri::State<'_, sqlx::SqlitePool>,
    item_id: i64,
    content: String,
) -> Result<String, String> {
    // Check if we have a cached translation that's still valid
    let cached: Option<(Option<String>, Option<chrono::DateTime<chrono::Utc>>)> = sqlx::query_as(
        "SELECT translated_content, translated_at FROM feed_items WHERE id = $1"
    )
    .bind(item_id)
    .fetch_optional(pool.inner())
    .await
    .map_err(|e| {
        let err = format!("Failed to check cache: {}", e);
        log_to_file(&err);
        err
    })?;

    if let Some((Some(translated_content), Some(translated_at))) = cached {
        let cache_age = chrono::Utc::now() - translated_at;
        if cache_age.num_days() < TRANSLATION_CACHE_DAYS {
            // Emit cache hit event
            let _ = app_handle.emit_to("main", "translation-progress", serde_json::json!({
                "item_id": item_id,
                "total": 1,
                "completed": 1,
                "html_chunk": translated_content,
                "is_complete": true,
                "cached": true
            }));
            return Ok(format!(
                r#"<div class="bilingual-content" data-cached="true">{}</div>"#,
                translated_content
            ));
        }
    }

    let config = get_ai_config_internal(&app_handle).await.map_err(|e| {
        let err = format!("AI config error: {}", e);
        log_to_file(&err);
        let _ = app_handle.emit_to("main", "translation-progress", serde_json::json!({
            "item_id": item_id,
            "total": 0,
            "completed": 0,
            "html_chunk": "",
            "is_complete": true,
            "cached": false,
            "has_error": true,
            "error_messages": [e.clone()]
        }));
        e
    })?;

    let ai_service = AiService::new(config.clone()).map_err(|e| {
        let err = format!("AI service init error: {}", e);
        log_to_file(&err);
        let _ = app_handle.emit_to("main", "translation-progress", serde_json::json!({
            "item_id": item_id,
            "total": 0,
            "completed": 0,
            "html_chunk": "",
            "is_complete": true,
            "cached": false,
            "has_error": true,
            "error_messages": [err.clone()]
        }));
        e
    })?;

    // Extract paragraphs from the provided content
    let paragraphs = ai_service.extract_paragraphs(&content);
    let total = paragraphs.len();
    let mut completed = 0;
    let mut all_chunks = Vec::new();
    let mut first_error: Option<String> = None;

    // Translate each paragraph and emit progress
    for paragraph in &paragraphs {
        if paragraph.trim().is_empty() {
            continue;
        }

        match ai_service.translate_single_paragraph_bilingual(
            paragraph,
            "auto",
            "zh-CN"
        ).await {
            Ok(translated_para) => {
                completed += 1;
                all_chunks.push(translated_para.clone());

                // Emit progress event
                let _ = app_handle.emit_to("main", "translation-progress", serde_json::json!({
                    "item_id": item_id,
                    "total": total,
                    "completed": completed,
                    "html_chunk": translated_para,
                    "is_complete": false,
                    "cached": false
                }));
            }
            Err(e) => {
                let error_msg = format!("Translation failed at paragraph {}: {}", completed + 1, e);
                log_to_file(&error_msg);
                eprintln!("{}", error_msg);
                first_error = Some(error_msg.clone());

                let _ = app_handle.emit_to("main", "translation-error", serde_json::json!({
                    "item_id": item_id,
                    "error": error_msg,
                    "paragraph_index": completed + 1
                }));

                break;
            }
        }
    }

    // Check if we had an error
    if let Some(error) = first_error {
        let _ = app_handle.emit_to("main", "translation-progress", serde_json::json!({
            "item_id": item_id,
            "total": total,
            "completed": completed,
            "html_chunk": "",
            "is_complete": true,
            "cached": false,
            "has_error": true,
            "error_messages": [error],
            "partial_content": all_chunks.join("\n")
        }));

        return Err(format!("Translation failed. Check ~/.rss-reader/ai_errors.log for details."));
    }

    // Success - combine all chunks
    let bilingual = all_chunks.join("\n");
    let result = format!(
        r#"<div class="bilingual-content" data-cached="false">{}</div>"#,
        bilingual
    );

    // Emit final completion event
    let _ = app_handle.emit_to("main", "translation-progress", serde_json::json!({
        "item_id": item_id,
        "total": total,
        "completed": total,
        "html_chunk": "",
        "is_complete": true,
        "cached": false,
        "has_error": false,
        "error_messages": []
    }));

    // Save to database
    let now = chrono::Utc::now();
    sqlx::query(
        "UPDATE feed_items SET translated_content = $1, translated_at = $2 WHERE id = $3"
    )
    .bind(&result)
    .bind(&now)
    .bind(item_id)
    .execute(pool.inner())
    .await
    .map_err(|e| {
        let err = format!("Failed to save translation: {}", e);
        log_to_file(&err);
        err
    })?;

    Ok(result)
}

#[tauri::command]
pub async fn classify_item(
    app_handle: AppHandle,
    title: String,
    description: Option<String>,
    content_snippet: Option<String>,
    rss_title: Option<String>,
    existing_tags: Option<Vec<String>>,
) -> Result<ClassificationResponse, String> {
    let config = get_ai_config_internal(&app_handle).await?;
    let ai_service = AiService::new(config)?;

    let request = ClassificationRequest {
        title,
        description,
        content_snippet,
        rss_title,
        existing_tags,
    };

    Ok(ai_service.classify(request).await?)
}

#[tauri::command]
pub async fn set_ai_config(
    app_handle: AppHandle,
    api_key: String,
    base_url: Option<String>,
    model: Option<String>,
    skip_test: Option<bool>,
) -> Result<(), String> {
    let config = AiConfig {
        api_key,
        base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
        model: model.unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        max_tokens: Some(4000),
        temperature: Some(0.3),
    };

    // Test the configuration before saving (unless skip_test is true)
    let skip = skip_test.unwrap_or(false);
    if !skip {
        let test_service = AiService::new(config.clone())?;
        test_service.test_connection().await.map_err(|e| {
            let error_msg = format!("API connection test failed: {}. Please check your base URL and model name. Common issues:\n\
                      - For OpenAI-compatible APIs, use base URL like: https://api.openai.com/v1\n\
                      - Model name should match your provider (e.g., gpt-4o-mini, gpt-4o, gpt-3.5-turbo)\n\
                      - 404 error usually means the model doesn't exist or base URL is incorrect", e);
            log_to_file(&error_msg);
            error_msg
        })?;
    }

    // Save to persistent storage using home directory for better compatibility
    let home_dir = dirs::home_dir()
        .ok_or_else(|| "Failed to get home directory".to_string())?;

    let app_dir = home_dir.join(".rss-reader");

    std::fs::create_dir_all(&app_dir)
        .map_err(|e| format!("Failed to create config dir: {}", e))?;

    let config_file = app_dir.join("ai_config.json");
    let config_json = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;

    std::fs::write(&config_file, config_json)
        .map_err(|e| format!("Failed to write config file: {}", e))?;

    // Save to app state
    app_handle.manage(config);

    Ok(())
}

#[tauri::command]
pub async fn translate_item_bilingual_streaming(
    app_handle: AppHandle,
    pool: tauri::State<'_, sqlx::SqlitePool>,
    item_id: i64,
) -> Result<String, String> {
    // Get item from database
    let item: crate::database::schema::FeedItem = sqlx::query_as::<_, _>("SELECT * FROM feed_items WHERE id = $1")
        .bind(item_id)
        .fetch_one(pool.inner())
        .await
        .map_err(|e| {
            let err = format!("Failed to get item: {}", e);
            log_to_file(&err);
            err
        })?;

    // Check if we have a cached translation that's still valid
    if let Some(translated_content) = &item.translated_content {
        if let Some(translated_at) = item.translated_at {
            let cache_age = Utc::now() - translated_at;
            if cache_age.num_days() < TRANSLATION_CACHE_DAYS {
                // Emit cache hit event
                let _ = app_handle.emit_to("main", "translation-progress", serde_json::json!({
                    "item_id": item_id,
                    "total": 1,
                    "completed": 1,
                    "html_chunk": translated_content,
                    "is_complete": true,
                    "cached": true
                }));
                return Ok(format!(
                    r#"<div class="bilingual-content" data-cached="true">{}</div>"#,
                    translated_content
                ));
            }
        }
    }

    // Need to fetch new translation
    let content = item.content.as_ref()
        .or(item.description.as_ref())
        .ok_or_else(|| "No content to translate".to_string())?;

    let config = get_ai_config_internal(&app_handle).await.map_err(|e| {
        let err = format!("AI config error: {}", e);
        log_to_file(&err);
        // Emit error event
        let _ = app_handle.emit_to("main", "translation-progress", serde_json::json!({
            "item_id": item_id,
            "total": 0,
            "completed": 0,
            "html_chunk": "",
            "is_complete": true,
            "cached": false,
            "has_error": true,
            "error_messages": [e.clone()]
        }));
        e
    })?;
    let ai_service = AiService::new(config.clone()).map_err(|e| {
        let err = format!("AI service init error: {}", e);
        log_to_file(&err);
        // Emit error event
        let _ = app_handle.emit_to("main", "translation-progress", serde_json::json!({
            "item_id": item_id,
            "total": 0,
            "completed": 0,
            "html_chunk": "",
            "is_complete": true,
            "cached": false,
            "has_error": true,
            "error_messages": [err.clone()]
        }));
        e
    })?;

    // Extract paragraphs
    let paragraphs = ai_service.extract_paragraphs(content);
    let total = paragraphs.len();
    let mut completed = 0;
    let mut all_chunks = Vec::new();
    let mut first_error: Option<String> = None;

    // Translate each paragraph and emit progress
    for paragraph in &paragraphs {
        if paragraph.trim().is_empty() {
            continue;
        }

        match ai_service.translate_single_paragraph_bilingual(
            paragraph,
            "auto",
            "zh-CN"
        ).await {
            Ok(translated_para) => {
                completed += 1;
                all_chunks.push(translated_para.clone());

                // Emit progress event for this paragraph
                let _ = app_handle.emit_to("main", "translation-progress", serde_json::json!({
                    "item_id": item_id,
                    "total": total,
                    "completed": completed,
                    "html_chunk": translated_para,
                    "is_complete": false,
                    "cached": false
                }));
            }
            Err(e) => {
                // On first error, stop immediately and report failure
                let error_msg = format!("Translation failed at paragraph {}: {}", completed + 1, e);
                log_to_file(&error_msg);
                eprintln!("{}", error_msg);
                first_error = Some(error_msg.clone());

                // Emit error event
                let _ = app_handle.emit_to("main", "translation-error", serde_json::json!({
                    "item_id": item_id,
                    "error": error_msg,
                    "paragraph_index": completed + 1
                }));

                // Break immediately on error - don't continue translating
                break;
            }
        }
    }

    // Check if we had an error
    if let Some(error) = first_error {
        // Emit final failure event
        let _ = app_handle.emit_to("main", "translation-progress", serde_json::json!({
            "item_id": item_id,
            "total": total,
            "completed": completed,
            "html_chunk": "",
            "is_complete": true,
            "cached": false,
            "has_error": true,
            "error_messages": [error],
            "partial_content": all_chunks.join("\n")
        }));

        log_to_file(&format!("Translation stopped due to error (item {})", item_id));
        // Return error so frontend knows it failed
        return Err(format!("Translation failed. Check ~/.rss-reader/ai_errors.log for details."));
    }

    // Success - combine all chunks
    let bilingual = all_chunks.join("\n");
    let result = format!(
        r#"<div class="bilingual-content" data-cached="false">{}</div>"#,
        bilingual
    );

    // Emit final completion event
    let _ = app_handle.emit_to("main", "translation-progress", serde_json::json!({
        "item_id": item_id,
        "total": total,
        "completed": total,
        "html_chunk": "",
        "is_complete": true,
        "cached": false,
        "has_error": false,
        "error_messages": []
    }));

    // Save to database
    let now = Utc::now();
    sqlx::query(
        "UPDATE feed_items SET translated_content = $1, translated_at = $2 WHERE id = $3"
    )
    .bind(&result)
    .bind(&now)
    .bind(item_id)
    .execute(pool.inner())
    .await
    .map_err(|e| {
        let err = format!("Failed to save translation: {}", e);
        log_to_file(&err);
        err
    })?;

    Ok(result)
}

#[tauri::command]
pub async fn get_ai_config(
    app_handle: AppHandle,
) -> Result<AiConfigResponse, String> {
    // Try to get from app state
    if let Some(config) = app_handle.try_state::<AiConfig>() {
        let cfg = config.inner();
        return Ok(AiConfigResponse {
            api_key: cfg.api_key.clone(),
            base_url: cfg.base_url.clone(),
            model: cfg.model.clone(),
        });
    }

    // Try to load from persistent storage using home directory for better compatibility
    let home_dir = dirs::home_dir()
        .ok_or_else(|| "Failed to get home directory".to_string())?;

    let app_dir = home_dir.join(".rss-reader");

    let config_file = app_dir.join("ai_config.json");

    if config_file.exists() {
        let content = std::fs::read_to_string(&config_file)
            .map_err(|e| format!("Failed to read config file: {}", e))?;

        let config: AiConfig = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse config file: {}", e))?;

        // Store in app state for future use
        app_handle.manage(config.clone());

        Ok(AiConfigResponse {
            api_key: config.api_key,
            base_url: config.base_url,
            model: config.model,
        })
    } else {
        // Return default values if no config exists
        Ok(AiConfigResponse {
            api_key: String::new(),
            base_url: DEFAULT_BASE_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiConfigResponse {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
}

async fn get_ai_config_internal(app_handle: &AppHandle) -> Result<AiConfig, String> {
    // Try to get from app state
    if let Some(config) = app_handle.try_state::<AiConfig>() {
        return Ok(config.inner().clone());
    }

    // Try to load from persistent storage using home directory for better compatibility
    let home_dir = dirs::home_dir()
        .ok_or_else(|| "Failed to get home directory".to_string())?;

    let app_dir = home_dir.join(".rss-reader");

    std::fs::create_dir_all(&app_dir)
        .map_err(|e| format!("Failed to create config dir: {}", e))?;

    let config_file = app_dir.join("ai_config.json");

    if config_file.exists() {
        let content = std::fs::read_to_string(&config_file)
            .map_err(|e| format!("Failed to read config file: {}", e))?;

        let config: AiConfig = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse config file: {}", e))?;

        // Store in app state
        app_handle.manage(config.clone());

        Ok(config)
    } else {
        Err("No AI configuration found. Please configure API key first.".to_string())
    }
}

pub use crate::ai::ClassificationResponse;

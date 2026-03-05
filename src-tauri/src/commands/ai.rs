use crate::ai::{AiService, AiConfig, ClassificationRequest};
use tauri::{AppHandle, Manager, Emitter};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use dirs;

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

    let config = get_ai_config(&app_handle).await?;
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
    let config = get_ai_config(&app_handle).await?;
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

#[tauri::command]
pub async fn classify_item(
    app_handle: AppHandle,
    title: String,
    description: Option<String>,
    content_snippet: Option<String>,
    rss_title: Option<String>,
    existing_tags: Option<Vec<String>>,
) -> Result<ClassificationResponse, String> {
    let config = get_ai_config(&app_handle).await?;
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
) -> Result<(), String> {
    let config = AiConfig {
        api_key,
        base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
        model: model.unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        max_tokens: Some(4000),
        temperature: Some(0.3),
    };

    // Test the configuration before saving
    let test_service = AiService::new(config.clone())?;
    test_service.test_connection().await.map_err(|e| {
        format!("API connection test failed: {}. Please check your base URL and model name. Common issues:\n\
                  - For OpenAI-compatible APIs, use base URL like: https://api.openai.com/v1\n\
                  - Model name should match your provider (e.g., gpt-4o-mini, gpt-4o, gpt-3.5-turbo)\n\
                  - 404 error usually means the model doesn't exist or base URL is incorrect", e)
    })?;

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
        .map_err(|e| format!("Failed to get item: {}", e))?;

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

    let config = get_ai_config(&app_handle).await?;
    let ai_service = AiService::new(config)?;

    // Extract paragraphs
    let paragraphs = ai_service.extract_paragraphs(content);
    let total = paragraphs.len();
    let mut completed = 0;
    let mut all_chunks = Vec::new();

    // Translate each paragraph and emit progress
    for paragraph in paragraphs {
        if paragraph.trim().is_empty() {
            continue;
        }

        match ai_service.translate_single_paragraph_bilingual(
            &paragraph,
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
                // On error, still emit original paragraph
                eprintln!("Failed to translate paragraph: {}", e);
                completed += 1;
                let fallback = format!(r#"<div class="translation-paragraph"><div class="paragraph-original">{}</div></div>"#, paragraph);
                all_chunks.push(fallback.clone());

                let _ = app_handle.emit_to("main", "translation-progress", serde_json::json!({
                    "item_id": item_id,
                    "total": total,
                    "completed": completed,
                    "html_chunk": fallback,
                    "is_complete": false,
                    "cached": false
                }));
            }
        }
    }

    // Combine all chunks
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
        "cached": false
    }));

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

async fn get_ai_config(app_handle: &AppHandle) -> Result<AiConfig, String> {
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

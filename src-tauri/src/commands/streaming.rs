use std::sync::Arc;

use chrono::Utc;
use tauri::{AppHandle, Emitter};
use tauri::State;

use crate::ai::service::{AiService, extract_blocks, LlmAiService};
use crate::ai::{AiConfig, MAX_CHARS_PER_SEGMENT};
use crate::error::{AppError, Result};

use super::AppState;

/// Translation cache expiry: 3 days.
const TRANSLATION_CACHE_DAYS: i64 = 3;

/// Translate HTML content with streaming progress events.
///
/// Emits `translation-progress` events to the "main" window,
/// allowing the frontend to display partial results in real time.
#[tauri::command]
pub async fn translate_html_content_streaming(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    item_id: i64,
    content: String,
) -> Result<String> {
    // Check cache
    let cached: Option<(Option<String>, Option<chrono::DateTime<Utc>>)> = sqlx::query_as(
        "SELECT translated_content, translated_at FROM feed_items WHERE id = $1",
    )
    .bind(item_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| AppError::Database(e))?;

    if let Some((Some(translated_content), Some(translated_at))) = cached {
        let cache_age = Utc::now() - translated_at;
        if cache_age.num_days() < TRANSLATION_CACHE_DAYS {
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

    // Use the provided HTML content directly.
    // The AI system prompt instructs it to preserve all HTML tags
    // (including <img> and <a>), keeping images and links in the output.
    let translation_content = content;

    let ai_service = get_ai_service(&state).await?;

    let max_chars = MAX_CHARS_PER_SEGMENT;
    let blocks = extract_blocks(&translation_content, max_chars);
    let total = blocks.len();
    let mut completed = 0;
    let mut all_chunks: Vec<String> = Vec::new();
    let mut first_error: Option<String> = None;

    for block in &blocks {
        if block.trim().is_empty() {
            completed += 1;
            continue;
        }

        match ai_service
            .translate_bilingual(block, "auto", "zh-CN")
            .await
        {
            Ok(translated_block) => {
                completed += 1;
                all_chunks.push(translated_block.clone());

                let _ = app_handle.emit_to("main", "translation-progress", serde_json::json!({
                    "item_id": item_id,
                    "total": total,
                    "completed": completed,
                    "html_chunk": translated_block,
                    "is_complete": false,
                    "cached": false
                }));
            }
            Err(e) => {
                let error_msg = format!("Translation failed at block {}: {}", completed + 1, e);
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

    // Handle error case with partial save
    if let Some(error) = first_error {
        let partial_bilingual = strip_images_from_translated(&all_chunks.join("\n"));
        let partial_result = if partial_bilingual.is_empty() {
            String::new()
        } else {
            format!(
                r#"<div class="bilingual-content" data-cached="false" data-partial="true">{}</div>"#,
                partial_bilingual
            )
        };

        if !partial_result.is_empty() {
            let now = Utc::now();
            let _ = sqlx::query(
                "UPDATE feed_items SET translated_content = $1, translated_at = $2 WHERE id = $3",
            )
            .bind(&partial_result)
            .bind(&now)
            .bind(item_id)
            .execute(&state.pool)
            .await;
        }

        let _ = app_handle.emit_to("main", "translation-progress", serde_json::json!({
            "item_id": item_id,
            "total": total,
            "completed": completed,
            "html_chunk": partial_result,
            "is_complete": true,
            "cached": false,
            "has_error": true,
            "error_messages": [error],
            "partial_content": partial_bilingual
        }));

        return Ok(partial_result);
    }

    // Success: combine all chunks and deduplicate images in translated blocks
    let bilingual = strip_images_from_translated(&all_chunks.join("\n"));
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
        "UPDATE feed_items SET translated_content = $1, translated_at = $2 WHERE id = $3",
    )
    .bind(&result)
    .bind(&now)
    .bind(item_id)
    .execute(&state.pool)
    .await
    .map_err(|e| AppError::Database(e))?;

    Ok(result)
}

/// Translate a feed item with streaming progress events.
///
/// Loads the item from the database, extracts content,
/// translates block-by-block, and emits progress events.
#[tauri::command]
pub async fn translate_item_bilingual_streaming(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    item_id: i64,
) -> Result<String> {
    let item = state.feed_repo.find_by_id(item_id).await?;

    // Check cache
    if let Some(ref translated_content) = item.translated_content {
        if let Some(translated_at) = item.translated_at {
            let cache_age = Utc::now() - translated_at;
            if cache_age.num_days() < TRANSLATION_CACHE_DAYS {
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

    // In WebView mode (is_website_content), prefer markdown (content_md) which was
    // generated from the full website article. In RSS mode, prefer raw HTML content.
    let content = if item.is_website_content {
        item.content_md
            .as_ref()
            .or(item.content.as_ref())
            .or(item.description.as_ref())
    } else {
        item.content
            .as_ref()
            .or(item.content_md.as_ref())
            .or(item.description.as_ref())
    }
    .ok_or_else(|| AppError::OperationFailed("No content to translate".into()))?
    .clone();

    let ai_service = get_ai_service(&state).await?;

    let max_chars = MAX_CHARS_PER_SEGMENT;
    let blocks = extract_blocks(&content, max_chars);
    let total = blocks.len();
    let mut completed = 0;
    let mut all_chunks: Vec<String> = Vec::new();
    let mut first_error: Option<String> = None;

    for block in &blocks {
        if block.trim().is_empty() {
            completed += 1;
            continue;
        }

        match ai_service
            .translate_bilingual(block, "auto", "zh-CN")
            .await
        {
            Ok(translated_block) => {
                completed += 1;
                all_chunks.push(translated_block.clone());

                let _ = app_handle.emit_to("main", "translation-progress", serde_json::json!({
                    "item_id": item_id,
                    "total": total,
                    "completed": completed,
                    "html_chunk": translated_block,
                    "is_complete": false,
                    "cached": false
                }));
            }
            Err(e) => {
                let error_msg = format!("Translation failed at block {}: {}", completed + 1, e);
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

    if let Some(error) = first_error {
        let partial_bilingual = strip_images_from_translated(&all_chunks.join("\n"));
        let partial_result = if partial_bilingual.is_empty() {
            String::new()
        } else {
            format!(
                r#"<div class="bilingual-content" data-cached="false" data-partial="true">{}</div>"#,
                partial_bilingual
            )
        };

        if !partial_result.is_empty() {
            let now = Utc::now();
            let _ = sqlx::query(
                "UPDATE feed_items SET translated_content = $1, translated_at = $2 WHERE id = $3",
            )
            .bind(&partial_result)
            .bind(&now)
            .bind(item_id)
            .execute(&state.pool)
            .await;
        }

        let _ = app_handle.emit_to("main", "translation-progress", serde_json::json!({
            "item_id": item_id,
            "total": total,
            "completed": completed,
            "html_chunk": partial_result,
            "is_complete": true,
            "cached": false,
            "has_error": true,
            "error_messages": [error],
            "partial_content": partial_bilingual
        }));

        return Ok(partial_result);
    }

    let bilingual = strip_images_from_translated(&all_chunks.join("\n"));
    let result = format!(
        r#"<div class="bilingual-content" data-cached="false">{}</div>"#,
        bilingual
    );

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

    let now = Utc::now();
    sqlx::query(
        "UPDATE feed_items SET translated_content = $1, translated_at = $2 WHERE id = $3",
    )
    .bind(&result)
    .bind(&now)
    .bind(item_id)
    .execute(&state.pool)
    .await
    .map_err(|e| AppError::Database(e))?;

    Ok(result)
}

// ---- Helpers ----

/// Get an AI service from the state or create one from config file.
async fn get_ai_service(state: &AppState) -> Result<Arc<dyn AiService>> {
    if let Some(ref service) = state.ai_service {
        return Ok(Arc::clone(service));
    }

    let config = load_ai_config()?;
    let service = Arc::new(LlmAiService::new(config)?);
    Ok(service)
}

/// Load AI configuration from `~/.rss-reader/ai_config.json`.
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

/// Strip `<img>` tags from within `.paragraph-translated` divs.
///
/// The AI is instructed to keep translated sections clean (no HTML tags),
/// but some models may still include images in both the original and
/// translated blocks, causing duplicate images in the bilingual display.
fn strip_images_from_translated(html: &str) -> String {
    let marker = r#"<div class="paragraph-translated">"#;
    let mut out = String::with_capacity(html.len());
    let mut pos = 0;

    while let Some(start) = html[pos..].find(marker) {
        let abs_start = pos + start;
        // Copy everything up to and including the marker
        out.push_str(&html[pos..abs_start + marker.len()]);
        pos = abs_start + marker.len();

        // Find the closing </div>
        if let Some(end) = html[pos..].find("</div>") {
            let inner = &html[pos..pos + end];
            let bytes = inner.as_bytes();
            let mut i = 0;
            while i < inner.len() {
                if bytes[i] == b'<' && inner.len().saturating_sub(i) >= 4
                    && (inner[i + 1..].starts_with("img") || inner[i + 1..].starts_with("IMG"))
                {
                    // Skip past the entire <img ... > tag
                    i += 1; // past '<'
                    while i < inner.len() && bytes[i] != b'>' {
                        i += 1;
                    }
                    i += 1; // past '>'
                } else {
                    let ch = inner[i..].chars().next().unwrap_or(' ');
                    let len = ch.len_utf8();
                    out.push_str(&inner[i..i + len]);
                    i += len;
                }
            }
            out.push_str("</div>");
            pos += end + 6; // skip past </div>
        } else {
            out.push_str(&html[pos..]);
            break;
        }
    }

    out.push_str(&html[pos..]);
    out
}

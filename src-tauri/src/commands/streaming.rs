use std::sync::Arc;

use chrono::Utc;
use tauri::{AppHandle, Emitter};
use tauri::State;

use crate::ai::service::{is_html_content, AiService, LlmAiService};
use crate::ai::AiConfig;
use crate::content_processor::clean_markdown;
use crate::error::{AppError, Result};

use super::AppState;

/// Translation cache expiry in days.
const TRANSLATION_CACHE_DAYS: i64 = 3;

/// A pre-chunked translation request.
struct TranslateRequest {
    content: String,
}

/// Translate HTML content (or item content) with streaming progress events.
///
/// `source` is the raw content to translate. When `Some(item_id)`, the
/// translation is cached in the database and the cache is consulted first.
///
/// Emits `translation-progress` events to the "main" window so the frontend
/// can render partial results in real time.
#[tauri::command]
pub async fn translate_html_content_streaming(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    item_id: i64,
    content: String,
    force: Option<bool>,
) -> Result<String> {
    translate_streaming_inner(
        &app_handle,
        &state,
        item_id,
        TranslateRequest { content },
        force.unwrap_or(false),
    )
    .await
}

/// Translate a stored feed item by ID. Equivalent to
/// `translate_html_content_streaming` but reads content from the database
/// (preferring `content_md` for website-mode items).
#[tauri::command]
pub async fn translate_item_bilingual_streaming(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    item_id: i64,
    force: Option<bool>,
) -> Result<String> {
    // Lazily fill content_md first, then ALWAYS prefer it as the translation
    // source: text mode renders exactly this markdown, so the bilingual
    // original side matches the displayed article paragraph for paragraph —
    // identical output regardless of webview/text mode. (Previously RSS
    // items translated raw `content` HTML while the pane showed markdown,
    // so originals and display diverged.)
    let item =
        crate::services::feed_service::ensure_content_md_for_item(&state.feed_repo, item_id)
            .await?;
    let content = item
        .content_md
        .as_ref()
        .filter(|md| !md.trim().is_empty())
        .or(item.content.as_ref())
        .or(item.description.as_ref())
        .ok_or_else(|| AppError::OperationFailed("No content to translate".into()))?
        .clone();
    translate_streaming_inner(
        &app_handle,
        &state,
        item_id,
        TranslateRequest { content },
        force.unwrap_or(false),
    )
    .await
}

// ---------------------------------------------------------------------------
// Shared translation pipeline
// ---------------------------------------------------------------------------

async fn translate_streaming_inner(
    app_handle: &AppHandle,
    state: &AppState,
    item_id: i64,
    request: TranslateRequest,
    force: bool,
) -> Result<String> {
    // 1. Cache lookup. A forced run clears the old value first so an error
    //    cannot leave stale content looking like a fresh result.
    if force {
        state.feed_repo.update_translation(item_id, None, "").await?;
    } else if let Some(cached) = lookup_cached_translation(state, item_id).await? {
        let _ = app_handle.emit_to(
            "main",
            "translation-progress",
            serde_json::json!({
                "item_id": item_id,
                "total": 1,
                "completed": 1,
                "html_chunk": cached,
                "is_complete": true,
                "cached": true
            }),
        );
        return Ok(wrap_bilingual(&cached, true));
    }

    // 2. Clean markdown (strip non-content link patterns) before translation
    let cleaned = clean_markdown(&request.content);

    // 3. Extract blocks ONCE. The LlmAiService::translate_block method skips
    // re-extraction, so we don't pay the double-extract cost.
    let ai_service = get_or_build_ai_service(state).await?;
    let is_html = is_html_content(&cleaned);
    let blocks = crate::ai::service::extract_blocks(
        &cleaned,
        ai_service.config_max_chars,
    );
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
            .service
            .translate_block(block, "auto", "zh-CN", is_html)
            .await
        {
            Ok(translated_block) => {
                completed += 1;
                // Clean both sides before emitting. The frontend renders
                // chunks live, so a final cleanup pass would be too late.
                let stripped = clean_bilingual_block(&translated_block);
                all_chunks.push(stripped.clone());

                let _ = app_handle.emit_to(
                    "main",
                    "translation-progress",
                    serde_json::json!({
                        "item_id": item_id,
                        "total": total,
                        "completed": completed,
                        "html_chunk": stripped,
                        "is_complete": false,
                        "cached": false
                    }),
                );
            }
            Err(e) => {
                let error_msg = format!("Translation failed at block {}: {}", completed + 1, e);
                first_error = Some(error_msg.clone());

                let _ = app_handle.emit_to(
                    "main",
                    "translation-error",
                    serde_json::json!({
                        "item_id": item_id,
                        "error": error_msg,
                        "paragraph_index": completed + 1
                    }),
                );
                break;
            }
        }
    }

    // 4. Do not persist partial output. The frontend receives the error
    //    event and falls back to the original article.
    let result_body = build_translation_result(&all_chunks, first_error.as_deref())?;

    if let Err(e) = state
        .feed_repo
        .update_translation(item_id, None, &result_body)
        .await
    {
        eprintln!("Failed to persist translation for item {}: {}", item_id, e);
    }

    // 5. Final progress event
    let payload = serde_json::json!({
        "item_id": item_id,
        "total": total,
        "completed": completed,
        "is_complete": true,
        "cached": false,
        "has_error": false,
        "error_messages": [],
        "html_chunk": result_body.clone(),
    });
    let _ = app_handle.emit_to("main", "translation-progress", payload);

    Ok(result_body)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn lookup_cached_translation(
    state: &AppState,
    item_id: i64,
) -> Result<Option<String>> {
    let item = state.feed_repo.find_by_id(item_id).await?;
    let Some(translated) = item.translated_content else {
        return Ok(None);
    };
    if translated.trim().is_empty() {
        return Ok(None);
    }
    let Some(translated_at) = item.translated_at else {
        return Ok(None);
    };
    if (Utc::now() - translated_at).num_days() >= TRANSLATION_CACHE_DAYS {
        return Ok(None);
    }
    Ok(Some(translated))
}

fn wrap_bilingual(body: &str, cached: bool) -> String {
    format!(
        r#"<div class="bilingual-content" data-cached="{}">{}</div>"#,
        cached, body
    )
}

/// Build only a successful translation body. An error deliberately returns
/// without a body so callers cannot accidentally persist partial output.
fn build_translation_result(chunks: &[String], first_error: Option<&str>) -> Result<String> {
    if let Some(error) = first_error {
        return Err(AppError::OperationFailed(error.to_string()));
    }
    let raw = chunks.join("\n");
    let bilingual = clean_bilingual_block(&raw);
    Ok(wrap_bilingual(&bilingual, false))
}

/// Clean one bilingual pair block or a whole bilingual document.
///
/// The model sometimes echoes images and links from the original side. The
/// frontend renders chunks as they arrive, so both sides are cleaned before
/// each progress event as well as before persistence.
fn clean_bilingual_block(html: &str) -> String {
    let translated_clean = strip_images_from_translated(html);
    clean_original_side(&translated_clean)
}

/// Remove redundant images and links from `.paragraph-original` blocks while
/// preserving their readable text.
fn clean_original_side(html: &str) -> String {
    let marker = r#"<div class="paragraph-original">"#;
    let mut out = String::with_capacity(html.len());
    let mut pos = 0;

    while let Some(start) = html[pos..].find(marker) {
        let abs_start = pos + start;
        out.push_str(&html[pos..abs_start + marker.len()]);
        let scan_from = abs_start + marker.len();

        if let Some(end) = find_matching_div_close(&html[scan_from..]) {
            let inner = &html[scan_from..scan_from + end];
            let cleaned = strip_anchor_tags(&strip_markdown_links(&strip_markdown_images(
                &strip_img_tags(inner),
            )));
            out.push_str(&cleaned);
            out.push_str("</div>");
            pos = scan_from + end + "</div>".len();
        } else {
            out.push_str(&html[scan_from..]);
            return out;
        }
    }

    out.push_str(&html[pos..]);
    out
}

/// Reduce markdown links `[text](url)` to readable text, including URLs with
/// nested parentheses.
fn strip_markdown_links(s: &str) -> String {
    let bytes = s.as_bytes();
    let len = s.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;
    while i < len {
        if bytes[i] == b'[' {
            let after = &s[i + 1..];
            if let Some(close_bracket) = after.find("](") {
                let text = &s[i + 1..i + 1 + close_bracket];
                let url_start = i + 1 + close_bracket + 2;
                let mut depth = 1usize;
                let mut j = url_start;
                while j < len && depth > 0 {
                    if bytes[j] == b'(' {
                        depth += 1;
                    } else if bytes[j] == b')' {
                        depth -= 1;
                    }
                    j += 1;
                }
                if depth == 0 {
                    out.push_str(text);
                    i = j;
                    continue;
                }
            }
        }
        let ch = s[i..].chars().next().unwrap_or(' ');
        let n = ch.len_utf8();
        out.push_str(&s[i..i + n]);
        i += n;
    }
    out
}

/// Reduce HTML anchors to readable text without touching other tags.
fn strip_anchor_tags(s: &str) -> String {
    let lower = s.to_lowercase();
    let bytes = s.as_bytes();
    let len = s.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;
    while i < len {
        if lower[i..].starts_with("<a") {
            let after = i + 2;
            if after >= len
                || bytes[after] == b' '
                || bytes[after] == b'\t'
                || bytes[after] == b'\n'
                || bytes[after] == b'>'
            {
                let Some(open_end_rel) = lower[after..].find('>') else {
                    break;
                };
                let content_start = after + open_end_rel + 1;
                let Some(close_rel) = lower[content_start..].find("</a>") else {
                    out.push_str(&s[content_start..]);
                    return out;
                };
                let content_end = content_start + close_rel;
                out.push_str(&s[content_start..content_end]);
                i = content_end + "</a>".len();
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap_or(' ');
        let n = ch.len_utf8();
        out.push_str(&s[i..i + n]);
        i += n;
    }
    out
}

/// Get an AI service from the state or create one from config file.
///
/// Returns a thin wrapper that holds both the cached service instance AND the
/// configured `max_chars_per_segment`, so the streaming pipeline can read the
/// latter without re-loading the config.
async fn get_or_build_ai_service(state: &AppState) -> Result<AiHandle> {
    if let Some(ref service) = state.ai_service {
        let cfg_max = service.config_max_chars();
        return Ok(AiHandle {
            service: Arc::clone(service),
            config_max_chars: cfg_max,
        });
    }

    let config = load_ai_config()?;
    let max = config.max_chars_per_segment.unwrap_or(crate::ai::MAX_CHARS_PER_SEGMENT);
    let service = Arc::new(LlmAiService::new(config)?);
    Ok(AiHandle {
        service,
        config_max_chars: max,
    })
}

/// Lightweight wrapper exposing the parts of the AI service the streaming
/// pipeline actually uses.
struct AiHandle {
    service: Arc<dyn AiService>,
    config_max_chars: usize,
}

impl AiHandle {
    async fn translate_block(
        &self,
        block: &str,
        src: &str,
        tgt: &str,
        is_html: bool,
    ) -> Result<String> {
        self.service.translate_block(block, src, tgt, is_html).await
    }
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
///
/// This implementation pairs `<div class="paragraph-translated">` with its
/// matching `</div>` (depth-counted) so nested divs are handled correctly.
fn strip_images_from_translated(html: &str) -> String {
    let marker = r#"<div class="paragraph-translated">"#;
    let mut out = String::with_capacity(html.len());
    let mut pos = 0;

    while let Some(start) = html[pos..].find(marker) {
        let abs_start = pos + start;
        // Copy everything up to and including the marker
        out.push_str(&html[pos..abs_start + marker.len()]);
        let scan_from = abs_start + marker.len();

        // Find the matching </div> using depth counting so nested divs work
        if let Some(end) = find_matching_div_close(&html[scan_from..]) {
            let inner = &html[scan_from..scan_from + end];
            // Remove HTML <img> AND markdown ![alt](url) images inside this
            // translated block — either way the image must not be echoed.
            let stripped = strip_markdown_images(&strip_img_tags(inner));
            out.push_str(&stripped);
            out.push_str("</div>");
            pos = scan_from + end + "</div>".len();
        } else {
            // Unmatched — copy rest verbatim and stop
            out.push_str(&html[scan_from..]);
            return out;
        }
    }

    out.push_str(&html[pos..]);
    out
}

/// Find the position of the `</div>` that pairs with an opening `<div>` at
/// the start of `html`. Returns the position of the closing tag itself.
fn find_matching_div_close(html: &str) -> Option<usize> {
    let mut depth: usize = 1;
    let mut i = 0;
    while i < html.len() {
        let remaining = &html[i..];
        let open = remaining.find("<div").map(|p| i + p);
        let close = remaining.find("</div>").map(|p| i + p);
        match (open, close) {
            (None, None) => return None,
            (None, Some(c)) => {
                depth -= 1;
                if depth == 0 {
                    return Some(c);
                }
                i = c + "</div>".len();
            }
            (Some(_), Some(c)) if open.unwrap() >= c => {
                depth -= 1;
                if depth == 0 {
                    return Some(c);
                }
                i = c + "</div>".len();
            }
            (Some(o), _) => {
                // Verify it's really a <div and not <divsomethignelse>
                let after = o + "<div".len();
                let bytes = html.as_bytes();
                if after >= bytes.len()
                    || bytes[after] == b'>'
                    || bytes[after] == b' '
                    || bytes[after] == b'\t'
                    || bytes[after] == b'\n'
                {
                    depth += 1;
                }
                i = o + "<div".len();
            }
        }
    }
    None
}

/// Remove markdown image syntax `![alt](url)` from a string. Handles
/// parentheses nested inside the URL (e.g. `![a](img(1).png)`) via depth
/// counting. Case: `![` … `](` … `)`.
fn strip_markdown_images(s: &str) -> String {
    let bytes = s.as_bytes();
    let len = s.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;
    while i < len {
        // Look for "!["
        if bytes[i] == b'!' && i + 1 < len && bytes[i + 1] == b'[' {
            // Find "](" following the alt text
            let after = &s[i + 2..];
            if let Some(close_bracket) = after.find("](") {
                let url_start = i + 2 + close_bracket + 2;
                // Depth-count parens to the closing ")"
                let mut depth = 1usize;
                let mut j = url_start;
                while j < len && depth > 0 {
                    if bytes[j] == b'(' {
                        depth += 1;
                    } else if bytes[j] == b')' {
                        depth -= 1;
                    }
                    j += 1;
                }
                if depth == 0 {
                    i = j; // skip the entire ![alt](url)
                    continue;
                }
            }
        }
        let ch = s[i..].chars().next().unwrap_or(' ');
        let n = ch.len_utf8();
        out.push_str(&s[i..i + n]);
        i += n;
    }
    out
}

/// Remove `<img ...>` tags from a string (case-insensitive).
fn strip_img_tags(html: &str) -> String {
    let bytes = html.as_bytes();
    let len = html.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;
    while i < len {
        if bytes[i] == b'<' && i + 3 < len {
            // Case-insensitive match for "img"
            let c1 = bytes[i + 1];
            let c2 = bytes[i + 2];
            let c3 = bytes[i + 3];
            if (c1 == b'i' || c1 == b'I') && (c2 == b'm' || c2 == b'M') && (c3 == b'g' || c3 == b'G') {
                // Skip to past '>'
                while i < len && bytes[i] != b'>' {
                    i += 1;
                }
                i += 1; // past '>'
                continue;
            }
        }
        let ch = html[i..].chars().next().unwrap_or(' ');
        let n = ch.len_utf8();
        out.push_str(&html[i..i + n]);
        i += n;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_images_no_images() {
        let html = r#"<div class="paragraph-original">Some text</div><div class="paragraph-translated">一些文本</div>"#;
        let result = strip_images_from_translated(html);
        assert_eq!(result, html);
    }

    #[test]
    fn test_strip_images_from_translated() {
        let html = r#"<div class="paragraph-translated">一些文本 <img src="img.jpg" /></div>"#;
        let result = strip_images_from_translated(html);
        assert_eq!(result, r#"<div class="paragraph-translated">一些文本 </div>"#);
    }

    #[test]
    fn test_strip_images_preserves_original_images() {
        let html = r#"<div class="paragraph-original"><img src="img.jpg" /> Original</div><div class="paragraph-translated">翻译</div>"#;
        let result = strip_images_from_translated(html);
        assert_eq!(result, html);
    }

    #[test]
    fn test_strip_images_mixed_content() {
        let html = r#"<div class="paragraph-block">
<div class="paragraph-original"><img src="keep.jpg" /> Text</div>
<div class="paragraph-translated"><img src="remove.jpg" /> 文本</div>
</div>"#;
        let result = strip_images_from_translated(html);
        let expected = r#"<div class="paragraph-block">
<div class="paragraph-original"><img src="keep.jpg" /> Text</div>
<div class="paragraph-translated"> 文本</div>
</div>"#;
        assert_eq!(result, expected);
    }

    #[test]
    fn test_strip_images_empty_input() {
        assert_eq!(strip_images_from_translated(""), "");
    }

    #[test]
    fn test_strip_images_no_translated_div() {
        let html = r#"<div class="paragraph-original">Only original</div>"#;
        let result = strip_images_from_translated(html);
        assert_eq!(result, html);
    }

    #[test]
    fn test_strip_images_multiple_translated_blocks() {
        let html = r#"<div class="paragraph-translated">第一段 <img src="a.jpg" /></div>
<div class="paragraph-translated">第二段 <img src="b.jpg" /></div>"#;
        let result = strip_images_from_translated(html);
        let expected = r#"<div class="paragraph-translated">第一段 </div>
<div class="paragraph-translated">第二段 </div>"#;
        assert_eq!(result, expected);
    }

    #[test]
    fn test_strip_images_case_insensitive() {
        let html = r#"<div class="paragraph-translated">text <IMG SRC="x.jpg"></div>"#;
        let result = strip_images_from_translated(html);
        assert_eq!(result, r#"<div class="paragraph-translated">text </div>"#);
    }

    /// Regression: models echo the original's MARKDOWN images into the
    /// translated side — `![alt](url)` must be stripped there too, or the
    /// image displays twice in the bilingual pair.
    #[test]
    fn test_strip_markdown_images_basic() {
        let html = r#"<div class="paragraph-translated">文本 ![alt](https://x.com/a.png) 之后</div>"#;
        let result = strip_images_from_translated(html);
        assert_eq!(
            result,
            r#"<div class="paragraph-translated">文本  之后</div>"#
        );
    }

    #[test]
    fn test_strip_markdown_images_nested_parens_in_url() {
        let input = "before ![alt](https://x.com/img(1).png) after";
        assert_eq!(strip_markdown_images(input), "before  after");
    }

    #[test]
    fn test_strip_markdown_images_preserves_text_and_links() {
        let input = "see ![fig](a.png) and [link](https://x.com) and **bold**";
        let result = strip_markdown_images(input);
        // markdown links and bold survive; only the image syntax goes
        assert_eq!(result, "see  and [link](https://x.com) and **bold**");
    }

    #[test]
    fn test_strip_markdown_images_no_image() {
        let input = "plain text with (parens) but no ![";
        assert_eq!(strip_markdown_images(input), input);
    }

    #[test]
    fn test_strip_markdown_images_unclosed_kept() {
        // Unclosed syntax is kept verbatim rather than eating the rest
        let input = "text ![alt(https://x.com/a.png) tail";
        let result = strip_markdown_images(input);
        assert_eq!(result, "text ![alt(https://x.com/a.png) tail");
    }

    /// Streaming emits must be stripped per-chunk (not just at completion) —
    /// this is enforced by the emit site in translate_streaming_inner, which
    /// pipes every chunk through strip_images_from_translated.
    #[test]
    fn test_strip_images_markdown_inside_translated_div() {
        let html = r#"<div class="paragraph-translated">翻译 ![alt](x.png) 完成</div>"#;
        let result = strip_images_from_translated(html);
        assert_eq!(result, r#"<div class="paragraph-translated">翻译  完成</div>"#);
    }

    /// Regression: nested <div> inside paragraph-translated used to be cut at
    /// the first </div>, eating the closing tag.
    #[test]
    fn test_strip_images_with_nested_div_in_translated() {
        let html = r#"<div class="paragraph-translated">before <div>nested</div> after <img src="x.jpg"/></div>"#;
        let result = strip_images_from_translated(html);
        // Inner <div> must remain, outer wrapper must close, img removed
        assert_eq!(
            result,
            r#"<div class="paragraph-translated">before <div>nested</div> after </div>"#
        );
    }

    #[test]
    fn test_wrap_bilingual_cached() {
        assert_eq!(
            wrap_bilingual("body", true),
            r#"<div class="bilingual-content" data-cached="true">body</div>"#
        );
    }

    #[test]
    fn test_clean_original_side_strips_images_and_links() {
        let html = r#"<div class="paragraph-original">See [the docs](https://x.com/a) and ![fig](x.png) and <a href="https://x.com/b">more</a></div><div class="paragraph-translated">看文档</div>"#;
        assert_eq!(
            clean_bilingual_block(html),
            r#"<div class="paragraph-original">See the docs and  and more</div><div class="paragraph-translated">看文档</div>"#
        );
    }

    #[test]
    fn test_build_translation_result_rejects_partial_output() {
        let chunks = vec!["partial".to_string()];
        let result = build_translation_result(&chunks, Some("provider failed"));
        assert!(result.is_err());
    }

    #[test]
    fn test_build_translation_result_wraps_success() {
        let chunks = vec!["<div class=\"paragraph-original\">Text</div>".to_string()];
        assert_eq!(
            build_translation_result(&chunks, None).unwrap(),
            r#"<div class="bilingual-content" data-cached="false"><div class="paragraph-original">Text</div></div>"#
        );
    }
}
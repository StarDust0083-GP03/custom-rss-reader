use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Semaphore;
use tokio::time::sleep;

use crate::ai::*;
use crate::error::{AppError, Result};

// ---------------------------------------------------------------------------
// LLM concurrency semaphore: max 3 concurrent API calls
// ---------------------------------------------------------------------------
lazy_static::lazy_static! {
    static ref LLM_SEMAPHORE: Semaphore = Semaphore::new(3);
}

// ---------------------------------------------------------------------------
// Chat API types (private to this module)
// ---------------------------------------------------------------------------

#[derive(Clone, serde::Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Clone, serde::Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(serde::Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(serde::Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(serde::Deserialize)]
struct ChatResponseMessage {
    content: String,
}

// ---------------------------------------------------------------------------
// Trait definition
// ---------------------------------------------------------------------------

/// AI service trait — translate and classify content via LLM.
///
/// Like the repository pattern, this trait allows:
/// - A real implementation that calls an actual LLM API
/// - A mock implementation for tests (no API key needed)
#[async_trait]
pub trait AiService: Send + Sync {
    /// Translate content with bilingual (original + translated) format.
    /// Handles both HTML and plain text automatically.
    async fn translate_bilingual(
        &self,
        content: &str,
        source_lang: &str,
        target_lang: &str,
    ) -> Result<String>;

    /// Classify an article: return tags and a category.
    async fn classify(&self, request: ClassificationRequest) -> Result<ClassificationResponse>;

    /// Test the LLM API connection.
    async fn test_connection(&self) -> Result<String>;
}

// ---------------------------------------------------------------------------
// Real implementation: LlmAiService
// ---------------------------------------------------------------------------

/// Real implementation that calls an OpenAI-compatible LLM API.
pub struct LlmAiService {
    config: AiConfig,
    client: reqwest::Client,
}

impl LlmAiService {
    pub fn new(config: AiConfig) -> Result<Self> {
        config.is_valid()?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| AppError::Internal(format!("Failed to build HTTP client: {}", e)))?;
        Ok(Self { config, client })
    }

    /// Send a chat completion request to the LLM API.
    /// Acquires a semaphore permit before sending (max 3 concurrent).
    async fn send_request(&self, request: &ChatRequest) -> Result<String> {
        let _permit = LLM_SEMAPHORE
            .acquire()
            .await
            .map_err(|_| AppError::Internal("LLM semaphore closed".into()))?;

        let url = format!("{}/chat/completions", self.config.base_url.trim_end_matches('/'));

        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.config.api_key)
            .json(request)
            .send()
            .await
            .map_err(|e| AppError::Network(format!("LLM request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AppError::Network(format!(
                "LLM API returned {}: {}",
                status, body
            )));
        }

        let chat_response: ChatResponse = response
            .json()
            .await
            .map_err(|e| AppError::Parse(format!("Failed to parse LLM response: {}", e)))?;

        chat_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| AppError::Parse("LLM returned no choices".into()))
    }

    /// Retry a fallible async operation with exponential backoff.
    async fn with_retry<F, Fut, T>(&self, f: F) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = std::result::Result<T, String>>,
    {
        let mut last_error = String::new();
        for attempt in 0..=MAX_RETRIES {
            match f().await {
                Ok(val) => return Ok(val),
                Err(e) => {
                    last_error = e;
                    if attempt < MAX_RETRIES {
                        let delay = Duration::from_millis(500 * 2_u64.pow(attempt as u32));
                        sleep(delay).await;
                    }
                }
            }
        }
        Err(AppError::Network(format!(
            "LLM call failed after {} retries: {}",
            MAX_RETRIES, last_error
        )))
    }
}

#[async_trait]
impl AiService for LlmAiService {
    async fn translate_bilingual(
        &self,
        content: &str,
        source_lang: &str,
        target_lang: &str,
    ) -> Result<String> {
        let is_html = content.contains('<')
            && (content.contains("</p>")
                || content.contains("</h")
                || content.contains("</div"));

        let max_chars = self.config.max_chars_per_segment.unwrap_or(MAX_CHARS_PER_SEGMENT);

        let blocks = if is_html {
            extract_blocks(content, max_chars)
        } else {
            extract_paragraphs_plain(content)
        };

        let mut results = Vec::new();
        for block in &blocks {
            let block = block.trim();
            if block.is_empty() {
                continue;
            }

            let system_prompt = if is_html {
                format!(
                    "You are a professional translator. Translate the following HTML content from {} to {}.\n\
                    CRITICAL RULES:\n\
                    1. ONLY translate the text provided below. Do NOT add, generate, or retrieve any content from your training data or external knowledge.\n\
                    2. Preserve ALL HTML tags from the original exactly as they are. Do not modify, remove, or add any HTML tags.\n\
                    3. Do NOT generate any HTML structure, CSS classes, or UI elements that were not in the original.\n\
                    4. If the content is just a short snippet or summary, translate ONLY that snippet — do not expand it into a full article.\n\
                    Output format:\n\
                    <div class=\"translation-paragraph\">\n\
                    <div class=\"paragraph-original\">[ORIGINAL]</div>\n\
                    <div class=\"paragraph-translated\">[TRANSLATED]</div>\n\
                    </div>\n\
                    Replace [ORIGINAL] with the original text and [TRANSLATED] with the translation.\
                    Keep all HTML tags from the original inside the paragraph-original div.\
                    The translated version should contain clean text (no HTML tags).",
                    source_lang, target_lang
                )
            } else {
                format!(
                    "You are a professional translator. Translate the following text from {} to {}.\n\
                    Output format:\n\
                    <div class=\"translation-paragraph\">\n\
                    <div class=\"paragraph-original\">[ORIGINAL]</div>\n\
                    <div class=\"paragraph-translated\">[TRANSLATED]</div>\n\
                    </div>",
                    source_lang, target_lang
                )
            };

            let request = ChatRequest {
                model: self.config.model.clone(),
                messages: vec![
                    ChatMessage {
                        role: "system".into(),
                        content: system_prompt,
                    },
                    ChatMessage {
                        role: "user".into(),
                        content: block.to_string(),
                    },
                ],
                max_tokens: self.config.max_tokens,
                temperature: self.config.temperature,
            };

            let response = self
                .with_retry(|| {
                    let req = ChatRequest {
                        model: request.model.clone(),
                        messages: request.messages.clone(),
                        max_tokens: request.max_tokens,
                        temperature: request.temperature,
                    };
                    let this = &*self; // capture self by ref
                    async move { this.send_request(&req).await.map_err(|e| e.to_string()) }
                })
                .await?;

            results.push(response);
        }

        Ok(results.join("\n"))
    }

    async fn classify(&self, request: ClassificationRequest) -> Result<ClassificationResponse> {
        let system_prompt = "You are an article classification assistant. Given an article's title, description, and content snippet, \
            classify it by returning a JSON object with:\n\
            - \"tags\": array of 1-3 relevant tag strings (in English)\n\
            - \"category\": a single category string (e.g., \"technology\", \"science\", \"politics\", \"entertainment\", \"sports\", \"business\", \"health\", \"education\", \"other\")\n\
            Respond with ONLY the JSON object, no other text.";

        let content_snippet = request
            .content_snippet
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(1000)
            .collect::<String>();

        let user_message = format!(
            "Title: {}\nDescription: {}\nContent: {}\nExisting tags: {:?}",
            request.title,
            request.description.as_deref().unwrap_or(""),
            content_snippet,
            request.existing_tags.as_deref().unwrap_or(&[]),
        );

        let chat_request = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".into(),
                    content: system_prompt.into(),
                },
                ChatMessage {
                    role: "user".into(),
                    content: user_message,
                },
            ],
            max_tokens: Some(200),
            temperature: Some(0.1),
        };

        let response = self.send_request(&chat_request).await?;
        parse_classification_json(&response)
    }

    async fn test_connection(&self) -> Result<String> {
        let chat_request = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".into(),
                    content: "Reply with exactly one word: OK".into(),
                },
                ChatMessage {
                    role: "user".into(),
                    content: "Say OK".into(),
                },
            ],
            max_tokens: Some(10),
            temperature: Some(0.0),
        };

        self.send_request(&chat_request).await
    }
}

// ---------------------------------------------------------------------------
// Pure content-processing functions (no LLM calls needed)
// ---------------------------------------------------------------------------

/// Extract block-level HTML elements (p, h1-h6, li, blockquote, etc.)
/// Each block is a separate translation unit. Falls back to plain-text
/// paragraph splitting for non-HTML content.
pub fn extract_blocks(content: &str, max_chars: usize) -> Vec<String> {
    let mut blocks = Vec::new();

    if content.contains('<') {
        blocks = extract_html_blocks(content);
    }

    // Fallback: plain text paragraphs split by double newlines
    if blocks.is_empty() {
        for para in content.split("\n\n") {
            let trimmed = para.trim();
            if !trimmed.is_empty() && trimmed.len() > 5 {
                blocks.push(trimmed.to_string());
            }
        }
    }

    // Final fallback: treat entire content as one block (only if substantial)
    if blocks.is_empty() {
        let trimmed = content.trim();
        if !trimmed.is_empty() && trimmed.len() > 5 {
            blocks.push(trimmed.to_string());
        }
    }

    merge_small_blocks(blocks, max_chars)
}

/// Extract blocks from HTML content using tag-based selection.
fn extract_html_blocks(content: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let block_tags = [
        "p", "h1", "h2", "h3", "h4", "h5", "h6",
        "li", "blockquote", "pre",
        "div", "section", "article",
    ];

    for tag in &block_tags {
        let open_tag = format!("<{}", tag);
        let close_tag = format!("</{}>", tag);
        let mut search_start = 0;

        while let Some(start) = content[search_start..].find(&open_tag) {
            let abs_start = search_start + start;
            let tag_end = match content[abs_start..].find('>') {
                Some(pos) => abs_start + pos + 1,
                None => break,
            };

            let remaining = &content[tag_end..];
            let close_pos = match remaining.find(&close_tag) {
                Some(pos) => tag_end + pos + close_tag.len(),
                None => break,
            };

            let block = content[abs_start..close_pos].to_string();
            if !block.trim().is_empty() {
                blocks.push(block);
            }

            search_start = close_pos;
        }
    }

    // Deduplicate: avoid extracting nested blocks
    let mut deduplicated = Vec::new();
    for block in &blocks {
        let is_nested = blocks.iter().any(|other| {
            if other == block {
                return false;
            }
            other.len() > block.len() && other.contains(block)
        });
        if !is_nested {
            deduplicated.push(block.clone());
        }
    }

    deduplicated
}

/// Extract paragraphs from plain text (double newline separation).
pub fn extract_paragraphs_plain(content: &str) -> Vec<String> {
    let mut paragraphs: Vec<String> = content
        .split("\n\n")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if paragraphs.is_empty() && !content.trim().is_empty() {
        paragraphs.push(content.trim().to_string());
    }

    paragraphs
}

/// Merge small blocks together to reduce translation API calls.
pub fn merge_small_blocks(blocks: Vec<String>, max_chars: usize) -> Vec<String> {
    let mut merged = Vec::new();
    let mut current = String::new();
    let mut current_len = 0;

    let container_closing = [
        "</ul>", "</ol>", "</table>", "</div>", "</section>", "</blockquote>", "</pre>",
    ];

    for block in &blocks {
        let block_len = block.len();

        if block_len > max_chars {
            if !current.is_empty() {
                merged.push(current.clone());
                current.clear();
                current_len = 0;
            }
            let chunks = split_large_paragraph(block, max_chars);
            merged.extend(chunks);
            continue;
        }

        let needs_separator = if current.is_empty() {
            false
        } else {
            let current_ends_container = container_closing
                .iter()
                .any(|tag| current.trim_end().ends_with(tag));
            !current_ends_container
        };

        let sep_len = if needs_separator { 2 } else { 0 };
        if current_len + sep_len + block_len > max_chars && !current.is_empty() {
            merged.push(current.clone());
            current.clear();
            current_len = 0;
        }

        if !current.is_empty() && needs_separator {
            current.push_str("\n\n");
            current_len += 2;
        }
        current.push_str(block);
        current_len += block_len;
    }

    if !current.is_empty() {
        merged.push(current);
    }

    if merged.is_empty() && !blocks.is_empty() {
        return blocks;
    }

    merged
}

/// Split a large paragraph at sentence boundaries.
pub fn split_large_paragraph(paragraph: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut last_sentence_end = 0;

    let sentence_endings = ['。', '！', '？', '.', '!', '?', '；', ';', '…'];

    for c in paragraph.chars() {
        current.push(c);

        if sentence_endings.contains(&c) {
            last_sentence_end = current.len();
        }

        if current.len() >= max_chars {
            if last_sentence_end > max_chars / 2 {
                let content: String = current.drain(..last_sentence_end).collect();
                chunks.push(content);
                last_sentence_end = 0;
            } else {
                chunks.push(current.clone());
                current.clear();
                last_sentence_end = 0;
            }
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

/// Parse a JSON classification response from the LLM.
pub fn parse_classification_json(response: &str) -> Result<ClassificationResponse> {
    let value: serde_json::Value =
        serde_json::from_str(response).map_err(|e| {
            AppError::Parse(format!("Failed to parse classification JSON: {}", e))
        })?;

    let tags = value["tags"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let category = value["category"].as_str().map(String::from);

    Ok(ClassificationResponse { tags, category })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Escape HTML special characters for safe display.
    fn escape_html(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#x27;")
    }

    /// Split an HTML block at text boundaries for long content.
    fn split_html_block(block: &str, max_chars: usize) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut current = String::new();
        let mut current_len = 0;
        let mut in_tag = false;
        let mut tag_buffer = String::new();
        let mut text_buffer = String::new();

        for c in block.chars() {
            match c {
                '<' => {
                    if !in_tag {
                        flush_text_buffer(
                            &mut text_buffer,
                            &mut current,
                            &mut current_len,
                            &mut chunks,
                            max_chars,
                        );
                    }
                    in_tag = true;
                    tag_buffer.push(c);
                }
                '>' => {
                    tag_buffer.push(c);
                    if in_tag {
                        flush_buffer(
                            &mut tag_buffer,
                            &mut current,
                            &mut current_len,
                            &mut chunks,
                            max_chars,
                        );
                        in_tag = false;
                    }
                }
                _ => {
                    if in_tag {
                        tag_buffer.push(c);
                    } else {
                        text_buffer.push(c);
                    }
                }
            }
        }

        flush_text_buffer(
            &mut text_buffer,
            &mut current,
            &mut current_len,
            &mut chunks,
            max_chars,
        );
        if !current.is_empty() {
            chunks.push(current);
        }
        if chunks.is_empty() && !block.is_empty() {
            chunks.push(block.to_string());
        }

        chunks
    }

    fn flush_buffer(
        buf: &mut String,
        current: &mut String,
        current_len: &mut usize,
        chunks: &mut Vec<String>,
        max_chars: usize,
    ) {
        if !buf.is_empty() {
            if *current_len + buf.len() > max_chars && !current.is_empty() {
                chunks.push(current.clone());
                current.clear();
                *current_len = 0;
            }
            if !current.is_empty() {
                current.push_str(buf);
            } else {
                *current = buf.clone();
            }
            *current_len += buf.len();
            buf.clear();
        }
    }

    fn flush_text_buffer(
        buf: &mut String,
        current: &mut String,
        current_len: &mut usize,
        chunks: &mut Vec<String>,
        max_chars: usize,
    ) {
        if !buf.is_empty() {
            if *current_len + buf.len() > max_chars && !current.is_empty() {
                chunks.push(current.clone());
                current.clear();
                *current_len = 0;
            }
            if !current.is_empty() {
                current.push_str(buf);
            } else {
                *current = buf.clone();
            }
            *current_len += buf.len();
            buf.clear();
        }
    }

    /// Check if a translation appears truncated.
    fn is_translation_truncated(original: &str, translated: &str) -> bool {
        if original.len() < 200 {
            return false;
        }
        let min_ratio = 0.4;
        if translated.len() < (original.len() as f32 * min_ratio) as usize {
            return true;
        }
        let trimmed = translated.trim_end();
        if let Some(last_char) = trimmed.chars().last() {
            let proper_endings = ['。', '！', '？', '.', '!', '?', '」', '》', ')', '）', '`', '"', '\''];
            if !proper_endings.contains(&last_char) {
                if let Some(orig_last) = original.trim_end().chars().last() {
                    if proper_endings.contains(&orig_last) && !proper_endings.contains(&last_char) {
                        return true;
                    }
                }
            }
        }
        if translated.matches("**").count() % 2 != 0 {
            return true;
        }
        if translated.matches('`').count() % 2 != 0 {
            return true;
        }
        let open_tags = translated.matches('<').count();
        let close_tags = translated.matches('>').count();
        if open_tags != close_tags {
            return true;
        }
        false
    }

    // -----------------------------------------------------------------------
    // extract_blocks
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_blocks_from_html() {
        let html = r#"
            <div>
                <p>第一段</p>
                <p>第二段</p>
                <p>第三段</p>
            </div>
        "#;
        let blocks = extract_blocks(html, MAX_CHARS_PER_SEGMENT);
        assert!(!blocks.is_empty());
        let all = blocks.join(" ");
        assert!(all.contains("第一段"));
        assert!(all.contains("第二段"));
        assert!(all.contains("第三段"));
    }

    #[test]
    fn test_extract_blocks_preserves_html_structure() {
        let html = r#"
<h1>标题</h1>
<p>第一段，<strong>加粗</strong>和<em>斜体</em>。</p>
<ul>
<li>列表项1</li>
<li>列表项2</li>
</ul>
<blockquote>引用</blockquote>
"#;
        let blocks = extract_blocks(html, MAX_CHARS_PER_SEGMENT);
        let all = blocks.join(" ");
        assert!(all.contains("标题"));
        assert!(all.contains("第一段"));
        assert!(all.contains("加粗"));
        assert!(all.contains("列表项1"));
        assert!(all.contains("引用"));
    }

    #[test]
    fn test_extract_blocks_plain_text_fallback() {
        let text = "第一段。\n\n第二段。\n\n第三段。";
        let blocks = extract_blocks(text, MAX_CHARS_PER_SEGMENT);
        assert!(!blocks.is_empty());
        let joined = blocks.join("\n\n");
        assert!(joined.contains("第一段"));
        assert!(joined.contains("第二段"));
    }

    #[test]
    fn test_extract_blocks_empty_content() {
        assert!(extract_blocks("", MAX_CHARS_PER_SEGMENT).is_empty());
        assert!(extract_blocks("   \n\n   ", MAX_CHARS_PER_SEGMENT).is_empty());
    }

    #[test]
    fn test_extract_blocks_short_content() {
        let blocks = extract_blocks("Hi", MAX_CHARS_PER_SEGMENT);
        assert!(blocks.is_empty(), "Content ≤5 chars should yield no blocks");
    }

    // -----------------------------------------------------------------------
    // extract_paragraphs_plain
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_paragraphs_plain_basic() {
        let text = "第一段。\n\n第二段。\n\n第三段。";
        let paras = extract_paragraphs_plain(text);
        assert_eq!(paras.len(), 3);
    }

    #[test]
    fn test_extract_paragraphs_plain_single() {
        let paras = extract_paragraphs_plain("只有一段。");
        assert_eq!(paras.len(), 1);
    }

    #[test]
    fn test_extract_paragraphs_plain_empty() {
        let paras = extract_paragraphs_plain("");
        assert!(paras.is_empty());
    }

    // -----------------------------------------------------------------------
    // split_large_paragraph
    // -----------------------------------------------------------------------

    fn generate_text(sentence_count: usize, chars_per_sentence: usize) -> String {
        let mut result = String::new();
        for _ in 0..sentence_count {
            let content_len = chars_per_sentence.saturating_sub(1);
            let content: String = "测试内容".chars().cycle().take(content_len).collect();
            result.push_str(&content);
            result.push('。');
        }
        result
    }

    #[test]
    fn test_split_large_paragraph_basic() {
        let text = generate_text(5, 800);
        assert!(text.len() > MAX_CHARS_PER_SEGMENT);
        let chunks = split_large_paragraph(&text, MAX_CHARS_PER_SEGMENT);
        assert!(chunks.len() > 1, "Should split into multiple chunks");

        let valid_endings = ['。', '！', '？', '.', '!', '?', '；', ';'];
        for (i, chunk) in chunks.iter().enumerate() {
            if i < chunks.len() - 1 {
                let last = chunk.trim_end().chars().last().unwrap();
                assert!(
                    valid_endings.contains(&last),
                    "Non-final chunk {} should end with sentence punctuation, got '{}'",
                    i,
                    last
                );
            }
        }
    }

    #[test]
    fn test_split_large_paragraph_preserves_content() {
        let text = generate_text(10, 500);
        let chunks = split_large_paragraph(&text, MAX_CHARS_PER_SEGMENT);
        let merged: String = chunks.join("");
        assert_eq!(text, merged, "Split → join should roundtrip");
    }

    #[test]
    fn test_split_large_paragraph_short_text() {
        let text = "短文本。不分段。";
        let chunks = split_large_paragraph(text, MAX_CHARS_PER_SEGMENT);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn test_split_large_paragraph_mixed_endings() {
        let endings = ['。', '！', '？', '.', '!', '?'];
        let mut text = String::new();
        for i in 0..20 {
            text.push_str(&"内容".repeat(100));
            text.push(endings[i % endings.len()]);
        }
        let chunks = split_large_paragraph(&text, MAX_CHARS_PER_SEGMENT);
        let merged: String = chunks.join("");
        assert_eq!(text, merged);
    }

    // -----------------------------------------------------------------------
    // merge_small_blocks
    // -----------------------------------------------------------------------

    #[test]
    fn test_merge_small_blocks_basic() {
        let blocks: Vec<String> = (0..5).map(|i| format!("<p>段落{}内容</p>", i)).collect();
        let merged = merge_small_blocks(blocks.clone(), MAX_CHARS_PER_SEGMENT);
        let merged_text = merged.join("\n\n");
        for b in &blocks {
            assert!(merged_text.contains(b.as_str()));
        }
    }

    #[test]
    fn test_merge_small_blocks_respects_limit() {
        let blocks: Vec<String> = (0..10)
            .map(|i| format!("<p>{}</p>", "内容".repeat(400)))
            .collect();
        let merged = merge_small_blocks(blocks, MAX_CHARS_PER_SEGMENT);
        for (i, batch) in merged.iter().enumerate() {
            assert!(
                batch.len() <= MAX_CHARS_PER_SEGMENT + 100,
                "Batch {} exceeds limit: {} > {}",
                i,
                batch.len(),
                MAX_CHARS_PER_SEGMENT
            );
        }
    }

    #[test]
    fn test_merge_small_blocks_large_single() {
        let large = format!("<p>{}</p>", "内容".repeat(2000));
        let merged = merge_small_blocks(vec![large], MAX_CHARS_PER_SEGMENT);
        assert!(merged.len() > 1, "Large block should be split");
    }

    // -----------------------------------------------------------------------
    // split_html_block
    // -----------------------------------------------------------------------

    #[test]
    fn test_split_html_block_basic() {
        let html = r#"<p>第一句。第二句。第三句。</p>"#;
        let chunks = split_html_block(html, MAX_CHARS_PER_SEGMENT);
        assert!(!chunks.is_empty());
        let joined: String = chunks.join("");
        assert_eq!(joined, html);
    }

    // -----------------------------------------------------------------------
    // is_translation_truncated
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_translation_truncated_short_original() {
        assert!(!is_translation_truncated("短", "短"));
    }

    #[test]
    fn test_is_translation_truncated_by_length() {
        let original = "很长".repeat(100);
        let truncated = "短";
        assert!(is_translation_truncated(&original, truncated));
    }

    #[test]
    fn test_is_translation_truncated_by_ending() {
        let original = "完整句子。".repeat(50);
        let no_ending = "没有句尾标点".repeat(40);
        assert!(is_translation_truncated(&original, &no_ending));
    }

    #[test]
    fn test_is_translation_truncated_unbalanced_markdown() {
        let original = "**加粗**和`代码`。".repeat(20);
        let unbalanced = "**只有开头".repeat(20);
        assert!(is_translation_truncated(&original, &unbalanced));
    }

    #[test]
    fn test_is_translation_truncated_complete() {
        let original = "完整句子。".repeat(50);
        let complete = "完整翻译。".repeat(60);
        assert!(!is_translation_truncated(&original, &complete));
    }

    // -----------------------------------------------------------------------
    // parse_classification_json
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_classification_json_valid() {
        let json = r#"{"tags":["tech","ai"],"category":"technology"}"#;
        let result = parse_classification_json(json).unwrap();
        assert_eq!(
            result,
            ClassificationResponse {
                tags: vec!["tech".into(), "ai".into()],
                category: Some("technology".into()),
            }
        );
    }

    #[test]
    fn test_parse_classification_json_no_category() {
        let json = r#"{"tags":["news"],"category":null}"#;
        let result = parse_classification_json(json).unwrap();
        assert_eq!(result.tags, vec!["news"]);
        assert_eq!(result.category, None);
    }

    #[test]
    fn test_parse_classification_json_empty_tags() {
        let json = r#"{"tags":[],"category":"other"}"#;
        let result = parse_classification_json(json).unwrap();
        assert!(result.tags.is_empty());
        assert_eq!(result.category, Some("other".into()));
    }

    #[test]
    fn test_parse_classification_json_invalid() {
        let json = r#"not json"#;
        assert!(parse_classification_json(json).is_err());
    }

    #[test]
    fn test_parse_classification_json_missing_fields() {
        let json = r#"{}"#;
        let result = parse_classification_json(json).unwrap();
        assert!(result.tags.is_empty());
        assert_eq!(result.category, None);
    }

    // -----------------------------------------------------------------------
    // escape_html
    // -----------------------------------------------------------------------

    #[test]
    fn test_escape_html_basic() {
        let escaped = escape_html("<p>Test & \"quotes\"</p>");
        assert_eq!(
            escaped,
            "&lt;p&gt;Test &amp; &quot;quotes&quot;&lt;/p&gt;"
        );
    }

    #[test]
    fn test_escape_html_no_special_chars() {
        assert_eq!(escape_html("plain text"), "plain text");
    }

    // -----------------------------------------------------------------------
    // AiConfig validation
    // -----------------------------------------------------------------------

    #[test]
    fn test_ai_config_valid() {
        let config = AiConfig {
            api_key: "sk-test".into(),
            base_url: "https://api.example.com".into(),
            model: "gpt-4".into(),
            max_tokens: Some(1000),
            temperature: Some(0.3),
            max_chars_per_segment: None,
        };
        assert!(config.is_valid().is_ok());
    }

    #[test]
    fn test_ai_config_empty_key_fails() {
        let config = AiConfig {
            api_key: "".into(),
            base_url: "https://api.example.com".into(),
            model: "gpt-4".into(),
            max_tokens: None,
            temperature: None,
            max_chars_per_segment: None,
        };
        assert!(config.is_valid().is_err());
    }

    #[test]
    fn test_ai_config_empty_url_fails() {
        let config = AiConfig {
            api_key: "sk-test".into(),
            base_url: "".into(),
            model: "gpt-4".into(),
            max_tokens: None,
            temperature: None,
            max_chars_per_segment: None,
        };
        assert!(config.is_valid().is_err());
    }
}

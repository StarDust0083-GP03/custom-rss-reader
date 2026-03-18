use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

// 翻译分段配置
// 每段最大字符数 - 设置较小值以提高成功率
// 长段落容易导致 LLM 超时或失败
const MAX_CHARS_PER_SEGMENT: usize = 3000; // 约 2000-2500 tokens
// 最大重试次数
const MAX_RETRIES: usize = 2;

#[derive(Error, Debug)]
pub enum AiError {
    #[error("HTTP error: {0}")]
    HttpError(#[from] reqwest::Error),
    #[error("API error: {0}")]
    ApiError(String),
    #[error("Config error: {0}")]
    ConfigError(String),
    #[error("No API key configured")]
    NoApiKey,
}

impl From<AiError> for String {
    fn from(error: AiError) -> Self {
        error.to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassificationRequest {
    pub title: String,
    pub description: Option<String>,
    pub content_snippet: Option<String>,
    pub rss_title: Option<String>,
    pub existing_tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassificationResponse {
    pub tags: Vec<String>,
    pub category: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

pub struct AiService {
    client: Client,
    config: AiConfig,
}

impl AiService {
    pub fn new(config: AiConfig) -> Result<Self, AiError> {
        if config.api_key.is_empty() {
            return Err(AiError::NoApiKey);
        }

        let client = Client::builder()
            .timeout(Duration::from_secs(120)) // 增加到 2 分钟
            .build()?;

        Ok(Self { client, config })
    }

    async fn chat_completion(&self, messages: Vec<ChatMessage>) -> Result<String, AiError> {
        let request = ChatRequest {
            model: self.config.model.clone(),
            messages,
            max_tokens: self.config.max_tokens,
            temperature: self.config.temperature,
        };

        self.send_request(request).await
    }

    /// Chat completion with custom max_tokens (for translation)
    async fn chat_completion_with_tokens(&self, messages: Vec<ChatMessage>, max_tokens: u32) -> Result<String, AiError> {
        let request = ChatRequest {
            model: self.config.model.clone(),
            messages,
            max_tokens: Some(max_tokens),
            temperature: self.config.temperature,
        };

        self.send_request(request).await
    }

    async fn send_request(&self, request: ChatRequest) -> Result<String, AiError> {
        let response = self
            .client
            .post(format!("{}/chat/completions", self.config.base_url))
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| AiError::ApiError(format!("Request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_else(|_| "Unable to read error".to_string());
            return Err(AiError::ApiError(format!("API returned {}: {}", status, error_text)));
        }

        let chat_response: ChatResponse = response.json().await.map_err(|e| {
            AiError::ApiError(format!("Failed to parse response: {}", e))
        })?;

        chat_response
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .ok_or_else(|| AiError::ApiError("No response from API".to_string()))
    }

    /// Translate content with paragraph-by-paragraph bilingual format
    /// Extracts paragraphs and translates each one separately
    pub async fn translate_bilingual_segmented(
        &self,
        content: &str,
        source_lang: &str,
        target_lang: &str,
    ) -> Result<String, AiError> {
        // Extract paragraphs from HTML content
        let paragraphs = self.extract_paragraphs(content);
        let mut results = Vec::new();

        // Translate each paragraph separately
        for (index, para) in paragraphs.iter().enumerate() {
            if para.trim().is_empty() {
                continue;
            }

            match self.translate_single_paragraph_bilingual(para, source_lang, target_lang).await {
                Ok(result) => {
                    results.push(result);
                }
                Err(e) => {
                    // Log error but continue with other paragraphs
                    eprintln!("Failed to translate paragraph {}: {}", index + 1, e);
                    // Keep original paragraph as fallback
                    results.push(format!(r#"<div class="translation-paragraph">{}</div>"#, para));
                }
            }
        }

        Ok(results.join("\n"))
    }

    /// Extract paragraphs from HTML content by <p> tags or double line breaks
    /// Then merge small paragraphs into batches up to MAX_CHARS_PER_SEGMENT
    pub fn extract_paragraphs(&self, content: &str) -> Vec<String> {
        let mut raw_paragraphs = Vec::new();

        // First: extract raw paragraphs by <p> tags
        if content.contains("<p") {
            let mut current = String::new();
            let mut in_p_tag = false;
            let mut p_depth: i32 = 0;
            let chars = content.chars().collect::<Vec<_>>();
            let mut i = 0;

            while i < chars.len() {
                // Check for opening <p> tag
                if i + 2 < chars.len() && chars[i] == '<' && chars[i + 1] == 'p' {
                    let next_char = if i + 3 < chars.len() { chars[i + 2] } else { ' ' };
                    if next_char == '>' || next_char == ' ' || next_char == '\t' || next_char == '/' {
                        // Save any content before this <p> tag
                        if !in_p_tag && !current.trim().is_empty() {
                            raw_paragraphs.push(current.trim().to_string());
                            current.clear();
                        }
                        in_p_tag = true;
                        if next_char != '/' {
                            p_depth += 1;
                        }
                        // Extract the complete tag
                        while i < chars.len() && chars[i] != '>' {
                            current.push(chars[i]);
                            i += 1;
                        }
                        if i < chars.len() {
                            current.push(chars[i]); // push '>'
                            i += 1;
                        }
                        continue;
                    }
                }
                // Check for closing </p> tag
                else if i + 3 < chars.len() && chars[i] == '<' && chars[i + 1] == '/' && chars[i + 2] == 'p' && chars[i + 3] == '>' {
                    current.push_str("</p>");
                    i += 4;
                    p_depth = p_depth.saturating_sub(1);
                    if p_depth == 0 {
                        in_p_tag = false;
                        if !current.trim().is_empty() {
                            raw_paragraphs.push(current.trim().to_string());
                        }
                        current.clear();
                    }
                    continue;
                }

                if in_p_tag {
                    current.push(chars[i]);
                }
                i += 1;
            }

            // Add any remaining content
            if !current.trim().is_empty() {
                raw_paragraphs.push(current.trim().to_string());
            }
        }

        // Second fallback: split by double newlines (plain text paragraphs)
        if raw_paragraphs.is_empty() {
            for para in content.split("\n\n") {
                let trimmed = para.trim();
                if !trimmed.is_empty() {
                    raw_paragraphs.push(trimmed.to_string());
                }
            }
        }

        // Final fallback: treat entire content as one paragraph
        if raw_paragraphs.is_empty() && !content.trim().is_empty() {
            raw_paragraphs.push(content.trim().to_string());
        }

        // Now merge small paragraphs into batches up to MAX_CHARS_PER_SEGMENT
        self.merge_paragraphs(raw_paragraphs)
    }

    /// Merge small paragraphs into batches up to MAX_CHARS_PER_SEGMENT
    /// Only merges paragraphs smaller than MIN_MERGE_SIZE
    fn merge_paragraphs(&self, paragraphs: Vec<String>) -> Vec<String> {
        let mut merged = Vec::new();
        let mut current_batch = String::new();
        let mut current_length = 0;
        let mut para_count = 0;

        for para in paragraphs {
            let para_length = para.len();

            // If this single paragraph exceeds the limit, split it
            if para_length > MAX_CHARS_PER_SEGMENT {
                // Flush current batch first
                if !current_batch.is_empty() {
                    merged.push(current_batch.clone());
                    current_batch.clear();
                    current_length = 0;
                    para_count = 0;
                }
                // Split the large paragraph
                let chunks = self.split_large_paragraph(&para);
                merged.extend(chunks);
                continue;
            }

            // 更激进的合并策略：只有超过字符限制时才刷新
            // 不限制段落数量，让更多段落合并在一起
            let should_flush = if current_length + para_length > MAX_CHARS_PER_SEGMENT {
                // Would exceed limit, must flush
                true
            } else {
                false
            };

            if should_flush && !current_batch.is_empty() {
                merged.push(current_batch.clone());
                current_batch.clear();
                current_length = 0;
                para_count = 0;
            }

            // Add paragraph to current batch
            if !current_batch.is_empty() {
                current_batch.push_str("\n\n");
                current_length += 2;
            }
            current_batch.push_str(&para);
            current_length += para_length;
            para_count += 1;
        }

        // Add remaining batch
        if !current_batch.is_empty() {
            merged.push(current_batch);
        }

        merged
    }

    /// Split a large paragraph that exceeds MAX_CHARS_PER_SEGMENT
    /// 尽可能在句子末尾断开，避免句子被截断
    fn split_large_paragraph(&self, paragraph: &str) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut current = String::new();
        let mut last_sentence_end = 0; // 记录上一个句子结尾的位置

        // 句子结束标点（中英文）
        let sentence_endings = ['。', '！', '？', '.', '!', '?', '；', ';', '…'];

        let chars: Vec<char> = paragraph.chars().collect();
        let mut i = 0;

        while i < chars.len() {
            let c = chars[i];
            current.push(c);

            // 检查是否是句子结尾
            let is_sentence_end = sentence_endings.contains(&c);

            // 检查特殊标点：省略号
            if c == '…' && i + 1 < chars.len() && chars[i + 1] == '…' {
                // 省略号是句子结尾，继续添加下一个字符
                i += 1;
                current.push('…');
            }

            if is_sentence_end {
                last_sentence_end = current.len();
            }

            // 检查是否需要分割
            if current.len() >= MAX_CHARS_PER_SEGMENT {
                // 策略1：如果在限制内有句子结尾，在那里断开
                if last_sentence_end > MAX_CHARS_PER_SEGMENT / 2 {
                    // 有合适的句子结尾位置
                    let sentence_content: String = current.drain(..last_sentence_end).collect();
                    chunks.push(sentence_content);
                    last_sentence_end = 0;
                } else {
                    // 策略2：向前查找句子结尾（最多看200字符）
                    let mut found = false;
                    let look_ahead = std::cmp::min(200, chars.len() - i - 1);

                    for j in 1..=look_ahead {
                        let next_idx = i + j;
                        if next_idx >= chars.len() {
                            break;
                        }
                        let next_c = chars[next_idx];
                        current.push(next_c);

                        if sentence_endings.contains(&next_c) {
                            // 找到句子结尾，在此断开
                            i = next_idx;
                            chunks.push(current.clone());
                            current.clear();
                            last_sentence_end = 0;
                            found = true;
                            break;
                        }
                    }

                    if !found {
                        // 策略3：找不到句子结尾，强制在当前位置断开
                        chunks.push(current.clone());
                        current.clear();
                        last_sentence_end = 0;
                    }
                }
            }

            i += 1;
        }

        // 添加剩余内容
        if !current.is_empty() {
            chunks.push(current);
        }

        chunks
    }

    /// Translate a single paragraph and return bilingual format
    /// If the paragraph contains multiple sub-paragraphs (separated by \n\n), split them for bilingual display
    /// Handles long paragraphs by splitting if translation is truncated
    pub async fn translate_single_paragraph_bilingual(
        &self,
        paragraph: &str,
        source_lang: &str,
        target_lang: &str,
    ) -> Result<String, AiError> {
        // Check if this is a merged paragraph (contains \n\n)
        if paragraph.contains("\n\n") {
            // Use Box::pin for recursive async call
            return Box::pin(self.translate_merged_paragraph_bilingual(paragraph, source_lang, target_lang)).await;
        }

        // Check if paragraph is too long and should be split proactively
        if paragraph.len() > MAX_CHARS_PER_SEGMENT {
            return self.translate_long_paragraph_bilingual(paragraph, source_lang, target_lang).await;
        }

        let system_prompt = format!(
            "You are a professional translator. Translate the following content from {} to {}. \
            \n\n\
            IMPORTANT FORMATTING RULES: \
            1. Preserve ALL HTML structure, tags, and formatting exactly. \
            2. Preserve ALL Markdown formatting: bold (**text**), italic (*text*), code (`text`), links ([text](url)), lists, headers, etc. \
            3. Only translate the text content, not HTML tags or Markdown syntax. \
            4. Keep the exact same structure - same number of paragraphs, same formatting. \
            5. Return ONLY the translated content, no explanations or notes. \
            6. Translate the COMPLETE content, do not skip or truncate any part.",
            source_lang, target_lang
        );

        // Calculate max_tokens based on paragraph length
        // Translation typically needs 1.5-2x the input tokens
        // Approximate: 1 token ≈ 2 chars for Chinese, 0.75 chars for English
        // Use 1.5x multiplier for safety
        let paragraph_chars = paragraph.len() as u32;
        let estimated_tokens = (paragraph_chars as f32 / 1.5).ceil() as u32;
        let max_tokens = std::cmp::max(estimated_tokens * 2, 1000); // At least 1000 tokens, 2x for translation

        // Retry logic for transient failures
        let mut attempt = 0;
        loop {
            let messages = vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: system_prompt.clone(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: paragraph.to_string(),
                },
            ];

            match self.chat_completion_with_tokens(messages, max_tokens).await {
                Ok(translated) => {
                    // Check if translation appears to be truncated
                    if self.is_translation_truncated(paragraph, &translated) {
                        eprintln!("Warning: Translation appears truncated (input: {}, output: {}). Retrying with split.",
                            paragraph.len(), translated.len());
                        // Try splitting the paragraph and translating in parts
                        return self.translate_long_paragraph_bilingual(paragraph, source_lang, target_lang).await;
                    }

                    // Return paragraph with bilingual format
                    return Ok(format!(
                        r#"<div class="translation-paragraph">
<div class="paragraph-original">{}</div>
<div class="paragraph-translated">{}</div>
</div>"#,
                        paragraph, translated
                    ));
                }
                Err(e) => {
                    if attempt >= MAX_RETRIES {
                        return Err(e);
                    }
                    // Wait before retry (exponential backoff)
                    tokio::time::sleep(Duration::from_millis(500 * (attempt + 1) as u64)).await;
                    eprintln!("Translation attempt {} failed, retrying... Error: {}", attempt + 1, e);
                    attempt += 1;
                }
            }
        }
    }

    /// Check if translation appears to be truncated
    fn is_translation_truncated(&self, original: &str, translated: &str) -> bool {
        // Skip check for short content
        if original.len() < 200 {
            return false;
        }

        // Check 1: Translation is much shorter than expected (less than 40%)
        // This catches obvious truncation from max_tokens limit
        let min_ratio = 0.4;
        if translated.len() < (original.len() as f32 * min_ratio) as usize {
            return true;
        }

        // Check 2: Translation ends mid-sentence (no proper ending punctuation)
        // Common sentence endings: 。！？. ! ?
        let trimmed = translated.trim_end();
        if !trimmed.is_empty() {
            let last_char = trimmed.chars().last().unwrap();
            let proper_endings = ['。', '！', '？', '.', '!', '?', '」', '」', '》', '】', ')', '）', '`', '"', '\''];
            if !proper_endings.contains(&last_char) {
                // Might be truncated mid-sentence
                // But also check if original ends similarly (could be intentional)
                let orig_trimmed = original.trim_end();
                if let Some(orig_last) = orig_trimmed.chars().last() {
                    if proper_endings.contains(&orig_last) && !proper_endings.contains(&last_char) {
                        return true;
                    }
                }
            }
        }

        // Check 3: Unbalanced Markdown/HTML tags (incomplete translation)
        // Count opening vs closing tags/markers
        let open_bold = translated.matches("**").count();
        if open_bold % 2 != 0 {
            return true;
        }

        let open_code = translated.matches('`').count();
        if open_code % 2 != 0 {
            return true;
        }

        // Check for unclosed HTML tags (simple check)
        let open_tags = translated.matches('<').count();
        let close_tags = translated.matches('>').count();
        if open_tags != close_tags {
            return true;
        }

        false
    }

    /// Translate a single chunk without recursion (helper for long paragraphs)
    async fn translate_single_chunk(
        &self,
        chunk: &str,
        source_lang: &str,
        target_lang: &str,
    ) -> Result<String, AiError> {
        let system_prompt = format!(
            "You are a professional translator. Translate the following content from {} to {}. \
            \n\n\
            IMPORTANT FORMATTING RULES: \
            1. Preserve ALL HTML structure, tags, and formatting exactly. \
            2. Preserve ALL Markdown formatting: bold (**text**), italic (*text*), code (`text`), links ([text](url)), lists, headers, etc. \
            3. Only translate the text content, not HTML tags or Markdown syntax. \
            4. Keep the exact same structure - same number of paragraphs, same formatting. \
            5. Return ONLY the translated content, no explanations or notes. \
            6. Translate the COMPLETE content, do not skip or truncate any part.",
            source_lang, target_lang
        );

        let chunk_chars = chunk.len() as u32;
        let estimated_tokens = (chunk_chars as f32 / 1.5).ceil() as u32;
        let max_tokens = std::cmp::max(estimated_tokens * 2, 1000);

        let mut attempt = 0;
        loop {
            let messages = vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: system_prompt.clone(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: chunk.to_string(),
                },
            ];

            match self.chat_completion_with_tokens(messages, max_tokens).await {
                Ok(translated) => {
                    // Check for truncation
                    if self.is_translation_truncated(chunk, &translated) {
                        eprintln!("Warning: Chunk translation truncated (input: {}, output: {}).",
                            chunk.len(), translated.len());
                        // For chunks, we can't split further, so just use what we got
                        // But log a warning
                    }

                    return Ok(format!(
                        r#"<div class="translation-paragraph">
<div class="paragraph-original">{}</div>
<div class="paragraph-translated">{}</div>
</div>"#,
                        chunk, translated
                    ));
                }
                Err(e) => {
                    if attempt >= MAX_RETRIES {
                        return Err(e);
                    }
                    tokio::time::sleep(Duration::from_millis(500 * (attempt + 1) as u64)).await;
                    eprintln!("Chunk translation attempt {} failed, retrying... Error: {}", attempt + 1, e);
                    attempt += 1;
                }
            }
        }
    }

    /// Translate a long paragraph by splitting it into smaller chunks
    async fn translate_long_paragraph_bilingual(
        &self,
        paragraph: &str,
        source_lang: &str,
        target_lang: &str,
    ) -> Result<String, AiError> {
        let chunks = self.split_large_paragraph(paragraph);
        let mut results = Vec::new();

        for chunk in chunks {
            if chunk.trim().is_empty() {
                continue;
            }

            // Use the non-recursive chunk translator
            match self.translate_single_chunk(&chunk, source_lang, target_lang).await {
                Ok(translated) => results.push(translated),
                Err(e) => {
                    // If a chunk fails, keep original
                    eprintln!("Failed to translate chunk: {}", e);
                    results.push(format!(
                        r#"<div class="translation-paragraph">
<div class="paragraph-original">{}</div>
<div class="paragraph-translated">{}</div>
</div>"#,
                        chunk, chunk
                    ));
                }
            }
        }

        Ok(results.join("\n"))
    }

    /// Translate a merged paragraph (contains multiple sub-paragraphs) and return bilingual format
    /// Each sub-paragraph is translated and displayed separately
    /// If translation is truncated (fewer output paragraphs than input), translate remaining individually
    async fn translate_merged_paragraph_bilingual(
        &self,
        merged_paragraph: &str,
        source_lang: &str,
        target_lang: &str,
    ) -> Result<String, AiError> {
        // Split by \n\n to get individual paragraphs
        let sub_paragraphs: Vec<&str> = merged_paragraph.split("\n\n")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();

        if sub_paragraphs.is_empty() {
            return Ok(String::new());
        }

        // If only one paragraph, use single paragraph translation
        if sub_paragraphs.len() == 1 {
            // Use Box::pin for recursive async call
            return Box::pin(self.translate_single_paragraph_bilingual(sub_paragraphs[0], source_lang, target_lang)).await;
        }

        // 构造系统提示：要求 LLM 保持段落结构
        let system_prompt = format!(
            "You are a professional translator. Translate the following content from {} to {}. \
            The content contains multiple paragraphs separated by double newlines (\\n\\n). \
            \n\n\
            IMPORTANT FORMATTING RULES: \
            1. Preserve ALL HTML structure, tags, and formatting exactly. \
            2. Preserve ALL Markdown formatting: bold (**text**), italic (*text*), code (`text`), links ([text](url)), lists, headers, etc. \
            3. Only translate the text content, not HTML tags or Markdown syntax. \
            4. Keep the exact same paragraph structure - use \\n\\n between translated paragraphs. \
            5. You MUST translate ALL paragraphs, do not skip or truncate any part. \
            6. Return ONLY the translated content with \\n\\n separators, no explanations.",
            source_lang, target_lang
        );

        let user_content = sub_paragraphs.join("\n\n");

        // Calculate max_tokens based on content length
        let content_chars = user_content.len() as u32;
        let estimated_tokens = (content_chars as f32 / 1.5).ceil() as u32;
        let max_tokens = std::cmp::max(estimated_tokens * 2, 1000);

        // Retry logic
        let mut attempt = 0;
        let translated = loop {
            let messages = vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: system_prompt.clone(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: user_content.clone(),
                },
            ];

            match self.chat_completion_with_tokens(messages, max_tokens).await {
                Ok(result) => break result,
                Err(e) => {
                    if attempt >= MAX_RETRIES {
                        return Err(e);
                    }
                    tokio::time::sleep(Duration::from_millis(500 * (attempt + 1) as u64)).await;
                    eprintln!("Merged translation attempt {} failed, retrying... Error: {}", attempt + 1, e);
                    attempt += 1;
                }
            }
        };

        // 按段落拆分翻译结果
        let translated_paragraphs: Vec<&str> = translated.split("\n\n")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();

        // 检查是否有漏翻（翻译结果段落数量少于原文）
        if translated_paragraphs.len() < sub_paragraphs.len() {
            eprintln!("Warning: Translation truncated ({} input, {} output paragraphs). Translating remaining individually.",
                sub_paragraphs.len(), translated_paragraphs.len());

            // 翻译结果不完整，需要单独翻译缺失的段落
            let mut results = Vec::new();

            // 先添加已翻译的部分
            for (i, original) in sub_paragraphs.iter().enumerate() {
                if i < translated_paragraphs.len() {
                    results.push(format!(
                        r#"<div class="translation-paragraph">
<div class="paragraph-original">{}</div>
<div class="paragraph-translated">{}</div>
</div>"#,
                        original, translated_paragraphs[i]
                    ));
                } else {
                    // 单独翻译这个段落
                    match self.translate_single_paragraph_bilingual(original, source_lang, target_lang).await {
                        Ok(translated_single) => results.push(translated_single),
                        Err(e) => {
                            // 翻译失败，使用原文
                            eprintln!("Failed to translate remaining paragraph {}: {}", i + 1, e);
                            results.push(format!(
                                r#"<div class="translation-paragraph">
<div class="paragraph-original">{}</div>
<div class="paragraph-translated">{}</div>
</div>"#,
                                original, original
                            ));
                        }
                    }
                }
            }

            return Ok(results.join("\n"));
        }

        // 组合原文和译文，每个段落分别对照
        let mut results = Vec::new();
        for (i, original) in sub_paragraphs.iter().enumerate() {
            let translated_para = if i < translated_paragraphs.len() {
                translated_paragraphs[i]
            } else {
                // 不应该发生，但作为保护
                original
            };

            results.push(format!(
                r#"<div class="translation-paragraph">
<div class="paragraph-original">{}</div>
<div class="paragraph-translated">{}</div>
</div>"#,
                original, translated_para
            ));
        }

        Ok(results.join("\n"))
    }

    /// Translate a single segment and return bilingual format (legacy, kept for compatibility)
    async fn translate_single_segment_bilingual(
        &self,
        content: &str,
        source_lang: &str,
        target_lang: &str,
    ) -> Result<String, AiError> {
        let system_prompt = format!(
            "You are a professional translator. Translate the following HTML content from {} to {}. \
            Preserve ALL HTML structure, tags, and formatting exactly. Only translate the text content, \
            not the HTML tags. Return ONLY the translated HTML, no explanations.",
            source_lang, target_lang
        );

        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: system_prompt,
            },
            ChatMessage {
                role: "user".to_string(),
                content: content.to_string(),
            },
        ];

        let translated = self.chat_completion(messages).await?;

        Ok(format!(
            r#"<div class="translation-segment">
<div class="original-content" data-original="true">
{}</div>
<div class="translated-content" data-translated="true">
{}</div>
</div>"#,
            content, translated
        ))
    }

    pub async fn classify(&self, request: ClassificationRequest) -> Result<ClassificationResponse, AiError> {
        let content_snippet = request.content_snippet
            .as_ref()
            .map(|s| s.chars().take(500).collect::<String>())
            .unwrap_or_default();

        let system_prompt = r#"You are a content classification assistant. Analyze the given article and provide:
1. 3-5 relevant tags (lowercase, comma-separated, use underscores for spaces)
2. A main category (one of: technology, programming, science, business, lifestyle, entertainment, sports, politics, health, education, other)

Return ONLY a JSON object in this exact format:
{
  "tags": ["tag1", "tag2", "tag3"],
  "category": "technology"
}

No additional text or explanation."#.to_string();

        // 构建用户消息，包含已有标签参考
        let existing_tags_info = if let Some(tags) = &request.existing_tags {
            if !tags.is_empty() {
                format!("Existing tags in this RSS: {}", tags.join(", "))
            } else {
                "(no existing tags yet)".to_string()
            }
        } else {
            "(no existing tags yet)".to_string()
        };

        let user_message = format!(
            "RSS Source: {}\n{}\n\nArticle Title: {}\nDescription: {}\nContent: {}",
            request.rss_title.as_ref().unwrap_or(&"(unknown)".to_string()),
            existing_tags_info,
            request.title,
            request.description.as_ref().unwrap_or(&"(no description)".to_string()),
            content_snippet
        );

        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: system_prompt,
            },
            ChatMessage {
                role: "user".to_string(),
                content: user_message,
            },
        ];

        let response = self.chat_completion(messages).await?;

        // Try to parse as JSON
        let result: serde_json::Value = serde_json::from_str(&response).map_err(|_| {
            AiError::ApiError(format!("Failed to parse AI response as JSON: {}", response))
        })?;

        let tags = result["tags"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        let category = result["category"].as_str().map(String::from);

        Ok(ClassificationResponse { tags, category })
    }

    pub async fn test_connection(&self) -> Result<String, AiError> {
        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: "You are a helpful assistant.".to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: "Say 'OK' if you can read this.".to_string(),
            },
        ];

        let response = self.chat_completion(messages).await?;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建测试用的 AiService 实例
    fn create_test_service() -> AiService {
        AiService::new(AiConfig {
            api_key: "test-key".to_string(),
            base_url: "https://test.example.com/v1".to_string(),
            model: "test-model".to_string(),
            max_tokens: Some(1000),
            temperature: Some(0.7),
        }).unwrap()
    }

    /// 生成指定长度的文本，包含句子结尾标点
    fn generate_text_with_sentences(sentence_count: usize, chars_per_sentence: usize) -> String {
        let mut result = String::new();
        for i in 0..sentence_count {
            // 生成句子内容
            let content_len = chars_per_sentence.saturating_sub(1); // 留一个字符给句号
            let content: String = "测试内容".chars().cycle().take(content_len).collect();
            result.push_str(&content);
            result.push('。');
        }
        result
    }

    #[test]
    fn test_split_large_paragraph_basic() {
        let service = create_test_service();

        // 测试：单个段落切分，应该在句子结尾断开
        // 生成 5 个句子，每个句子 800 字符，总计 4000 字符（超过 MAX_CHARS_PER_SEGMENT=3000）
        let text = generate_text_with_sentences(5, 800);
        assert!(text.len() > MAX_CHARS_PER_SEGMENT);

        let chunks = service.split_large_paragraph(&text);

        // 验证：应该切分成多个块
        assert!(chunks.len() > 1, "Should split into multiple chunks");

        // 验证：每个块应该以句子结尾标点结束
        for (i, chunk) in chunks.iter().enumerate() {
            let trimmed = chunk.trim_end();
            if !trimmed.is_empty() {
                let last_char = trimmed.chars().last().unwrap();
                let valid_endings = ['。', '！', '？', '.', '!', '?', '；', ';'];
                assert!(
                    valid_endings.contains(&last_char),
                    "Chunk {} should end with sentence punctuation, got '{}'. Chunk ends with: {:?}",
                    i,
                    last_char,
                    &chunk[chunk.len().saturating_sub(50)..]
                );
            }
        }
    }

    #[test]
    fn test_split_large_paragraph_preserves_content() {
        let service = create_test_service();

        // 测试：切分后合并应该等于原文
        let text = generate_text_with_sentences(10, 500);
        let chunks = service.split_large_paragraph(&text);
        let merged: String = chunks.join("");

        assert_eq!(text, merged, "Merged chunks should equal original text");
    }

    #[test]
    fn test_split_large_paragraph_short_text() {
        let service = create_test_service();

        // 测试：短文本不应该被切分
        let text = "这是一个短文本。不需要切分。";
        let chunks = service.split_large_paragraph(text);

        assert_eq!(chunks.len(), 1, "Short text should not be split");
        assert_eq!(chunks[0], text);
    }

    #[test]
    fn test_split_large_paragraph_exact_boundary() {
        let service = create_test_service();

        // 测试：刚好在边界上的句子
        // 构造一个每句约 1000 字符的文本，使得 MAX_CHARS_PER_SEGMENT 正好在句子之间
        let mut text = String::new();
        for i in 0..5 {
            let sentence = format!("句子{}。", i);
            let padding = "内容".repeat((MAX_CHARS_PER_SEGMENT / 2 - sentence.len()) / 2);
            text.push_str(&padding);
            text.push_str(&sentence);
        }

        let chunks = service.split_large_paragraph(&text);

        // 验证切分是合理的
        for chunk in &chunks {
            assert!(!chunk.is_empty(), "Chunks should not be empty");
        }
    }

    #[test]
    fn test_split_large_paragraph_mixed_endings() {
        let service = create_test_service();

        // 测试：混合句子结尾标点
        // 使用较短的句子，确保能在句子结尾断开
        let mut text = String::new();
        let endings = ['。', '！', '？', '.', '!', '?'];

        for (i, _) in (0..20).enumerate() {
            let padding = "测试内容".repeat(100); // 约 400 字符，确保在限制内有句子结尾
            text.push_str(&padding);
            text.push(endings[i % endings.len()]);
        }

        let chunks = service.split_large_paragraph(&text);

        // 验证：合并后等于原文
        let merged: String = chunks.join("");
        assert_eq!(text, merged, "Merged chunks should equal original text");

        // 验证大多数块以有效标点结束（允许最后一个块不满足）
        let valid_endings = ['。', '！', '？', '.', '!', '?', '；', ';'];
        let mut valid_count = 0;
        for chunk in &chunks {
            if let Some(last_char) = chunk.trim_end().chars().last() {
                if valid_endings.contains(&last_char) {
                    valid_count += 1;
                }
            }
        }
        // 至少 80% 的块应该以句子结尾结束
        let ratio = valid_count as f32 / chunks.len() as f32;
        assert!(ratio >= 0.8, "At least 80% of chunks should end with sentence punctuation, got {}%", ratio * 100.0);
    }

    #[test]
    fn test_merge_paragraphs_basic() {
        let service = create_test_service();

        // 测试：小段落合并
        let paragraphs: Vec<String> = (0..5)
            .map(|i| format!("<p>段落{}内容</p>", i))
            .collect();

        let merged = service.merge_paragraphs(paragraphs.clone());

        // 验证：所有内容都被保留
        let merged_text = merged.join("\n\n");
        for para in &paragraphs {
            assert!(
                merged_text.contains(para),
                "Merged text should contain all original paragraphs"
            );
        }
    }

    #[test]
    fn test_merge_paragraphs_respects_limit() {
        let service = create_test_service();

        // 测试：合并后的每个批次不超过限制
        // 创建多个小段落，总计超过 MAX_CHARS_PER_SEGMENT
        let paragraphs: Vec<String> = (0..10)
            .map(|i| format!("<p>{}</p>", "内容".repeat(400))) // 每个约 800 字符
            .collect();

        let merged = service.merge_paragraphs(paragraphs);

        // 验证每个合并后的批次不超过限制（考虑分隔符）
        for (i, batch) in merged.iter().enumerate() {
            assert!(
                batch.len() <= MAX_CHARS_PER_SEGMENT + 100, // 允许小误差
                "Batch {} exceeds limit: {} > {}",
                i,
                batch.len(),
                MAX_CHARS_PER_SEGMENT
            );
        }
    }

    #[test]
    fn test_merge_paragraphs_large_one() {
        let service = create_test_service();

        // 测试：单个超大段落应该被切分
        let large_paragraph = format!("<p>{}</p>", "内容".repeat(2000)); // 约 4000 字符
        let paragraphs = vec![large_paragraph.clone()];

        let merged = service.merge_paragraphs(paragraphs);

        // 单个大段落应该被切分
        assert!(merged.len() > 1, "Large paragraph should be split");
    }

    #[test]
    fn test_extract_paragraphs_from_html() {
        let service = create_test_service();

        // 测试：从 HTML 提取段落
        let html = r#"
            <div>
                <p class="intro">第一段落内容</p>
                <p>第二段落内容</p>
                <p id="last">第三段落内容</p>
            </div>
        "#;

        let paragraphs = service.extract_paragraphs(html);

        assert!(!paragraphs.is_empty(), "Should extract paragraphs from HTML");

        // 验证提取的内容包含原始段落
        let all_text = paragraphs.join(" ");
        assert!(all_text.contains("第一段落"), "Should contain first paragraph");
        assert!(all_text.contains("第二段落"), "Should contain second paragraph");
        assert!(all_text.contains("第三段落"), "Should contain third paragraph");
    }

    #[test]
    fn test_extract_paragraphs_plain_text() {
        let service = create_test_service();

        // 测试：纯文本按双换行分割
        // 注意：merge_paragraphs 会合并小段落，所以最终段落数可能少于原始段落数
        let text = "第一段落。\n\n第二段落。\n\n第三段落。";

        let paragraphs = service.extract_paragraphs(text);

        // 验证：至少提取出一个段落，且内容被保留
        assert!(!paragraphs.is_empty(), "Should extract at least one paragraph");

        // 验证内容被保留（可能被合并，但内容应该完整）
        let merged_text = paragraphs.join("\n\n");
        assert!(merged_text.contains("第一段落"), "Should contain first paragraph content");
        assert!(merged_text.contains("第二段落"), "Should contain second paragraph content");
        assert!(merged_text.contains("第三段落"), "Should contain third paragraph content");
    }

    #[test]
    fn test_is_translation_truncated() {
        let service = create_test_service();

        // 测试：明显截断的翻译
        let original = "这是一个很长的原始文本，包含足够多的内容来进行截断检测。".repeat(10);
        let truncated = "这是一个翻译"; // 太短

        assert!(
            service.is_translation_truncated(&original, &truncated),
            "Should detect truncation for very short translation"
        );

        // 测试：完整的翻译
        let complete = original.repeat(2); // 翻译通常更长
        assert!(
            !service.is_translation_truncated(&original, &complete),
            "Should not detect truncation for complete translation"
        );
    }

    #[test]
    fn test_is_translation_truncated_by_ending() {
        let service = create_test_service();

        // 测试：句子结尾检测
        let original = "这是一个完整的句子。".repeat(50); // 足够长
        let no_ending = "这是翻译但缺少结尾标点".repeat(40);

        assert!(
            service.is_translation_truncated(&original, &no_ending),
            "Should detect truncation by missing ending punctuation"
        );
    }

    #[test]
    fn test_is_translation_truncated_unbalanced_markdown() {
        let service = create_test_service();

        // 测试：不平衡的 Markdown 标记
        let original = "这是一段**加粗**和`代码`的文本。".repeat(20);
        let unbalanced = "这是翻译**只有开头".repeat(20); // 缺少结尾的 **

        assert!(
            service.is_translation_truncated(&original, &unbalanced),
            "Should detect truncation by unbalanced Markdown"
        );
    }

    /// 模拟翻译测试：验证切分与合并逻辑的完整性
    #[test]
    fn test_split_and_merge_integrity() {
        let service = create_test_service();

        // 创建一个模拟的长文章
        let article = generate_text_with_sentences(20, 300); // 20 个句子，每句约 300 字符

        // 第一步：切分
        let chunks = service.split_large_paragraph(&article);

        // 验证切分完整性
        let reconstructed: String = chunks.join("");
        assert_eq!(article, reconstructed, "Split should preserve all content");

        // 验证每个切分点都是句子结尾
        for (i, chunk) in chunks.iter().enumerate() {
            if i < chunks.len() - 1 {
                // 非最后一块
                let last_char = chunk.trim_end().chars().last().unwrap();
                let valid_endings = ['。', '！', '？', '.', '!', '?', '；', ';'];
                assert!(
                    valid_endings.contains(&last_char),
                    "Non-final chunk {} should end at sentence boundary, got '{}'",
                    i,
                    last_char
                );
            }
        }
    }

    /// 测试边界情况：刚好在 MAX_CHARS_PER_SEGMENT 边界
    #[test]
    fn test_boundary_case() {
        let service = create_test_service();

        // 构造一个刚好超过边界的文本
        let mut text = String::new();
        while text.len() < MAX_CHARS_PER_SEGMENT {
            text.push_str("测试句子内容。");
        }
        // 再添加一些内容确保超过边界
        text.push_str("额外的句子。更多内容。");

        let chunks = service.split_large_paragraph(&text);

        // 验证切分正确
        assert!(chunks.len() > 1, "Should split text that exceeds limit");

        // 验证合并后等于原文
        let merged: String = chunks.join("");
        assert_eq!(text, merged);
    }

    /// 测试嵌套 HTML 标签的处理
    #[test]
    fn test_nested_html_tags() {
        let service = create_test_service();

        let html = r#"<p>外层段落<strong>加粗内容<em>斜体</em></strong>继续文本。</p>"#;
        let paragraphs = service.extract_paragraphs(html);

        assert!(!paragraphs.is_empty(), "Should extract nested HTML");
        assert!(
            paragraphs[0].contains("<strong>") || paragraphs[0].contains("加粗"),
            "Should preserve nested tags or content"
        );
    }
}

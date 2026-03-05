use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

// DeepSeek 上下文限制：每个批次最大字符数
// DeepSeek-V3: 64K tokens, DeepSeek-V3-32K: 32K tokens
// 按 token ≈ 0.75 个字符计算
const MAX_CHARS_PER_SEGMENT: usize = 6000; // 约 4500 tokens，留有余量给系统提示和响应

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

#[derive(Debug, Serialize, Deserialize)]
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
            .timeout(Duration::from_secs(60))
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
    fn split_large_paragraph(&self, paragraph: &str) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut current = String::new();
        let mut chars = paragraph.chars().peekable();

        while chars.peek().is_some() {
            let c = chars.next().unwrap();
            current.push(c);

            // Check if we should split
            if current.len() >= MAX_CHARS_PER_SEGMENT {
                // Look ahead to find a good breaking point
                let mut temp = String::new();
                let mut found_break = false;

                // Check next 100 characters for sentence ending
                for next_c in chars.by_ref().take(100) {
                    temp.push(next_c);
                    // Sentence endings in Chinese and English
                    if "。！？.!?".contains(next_c) {
                        current.push_str(&temp);
                        chunks.push(current.clone());
                        current.clear();
                        temp.clear();
                        found_break = true;
                        break;
                    }
                }

                if !found_break {
                    // No sentence ending found, force split at current position
                    chunks.push(current.clone());
                    current = temp;
                }
            }
        }

        // Add remaining content
        if !current.is_empty() {
            chunks.push(current);
        }

        chunks
    }

    /// Translate a single paragraph and return bilingual format
    /// If the paragraph contains multiple sub-paragraphs (separated by \n\n), split them for bilingual display
    pub async fn translate_single_paragraph_bilingual(
        &self,
        paragraph: &str,
        source_lang: &str,
        target_lang: &str,
    ) -> Result<String, AiError> {
        // Check if this is a merged paragraph (contains \n\n)
        if paragraph.contains("\n\n") {
            return self.translate_merged_paragraph_bilingual(paragraph, source_lang, target_lang).await;
        }

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
                content: paragraph.to_string(),
            },
        ];

        let translated = self.chat_completion(messages).await?;

        // Return paragraph with bilingual format
        Ok(format!(
            r#"<div class="translation-paragraph">
<div class="paragraph-original">{}</div>
<div class="paragraph-translated">{}</div>
</div>"#,
            paragraph, translated
        ))
    }

    /// Translate a merged paragraph (contains multiple sub-paragraphs) and return bilingual format
    /// Each sub-paragraph is translated and displayed separately
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

        // 构造系统提示：要求 LLM 保持段落结构
        let system_prompt = format!(
            "You are a professional translator. Translate the following HTML content from {} to {}. \
            The content contains multiple paragraphs separated by double newlines (\\n\\n). \
            \n\n\
            IMPORTANT: \
            1. Preserve ALL HTML structure, tags, and formatting exactly. \
            2. Only translate the text content, not the HTML tags. \
            3. Keep the exact same paragraph structure - use \\n\\n between translated paragraphs. \
            4. Return ONLY the translated HTML with \\n\\n separators, no explanations.",
            source_lang, target_lang
        );

        let user_content = sub_paragraphs.join("\n\n");

        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: system_prompt,
            },
            ChatMessage {
                role: "user".to_string(),
                content: user_content,
            },
        ];

        // 一次性翻译所有段落
        let translated = self.chat_completion(messages).await?;

        // 按段落拆分翻译结果
        let translated_paragraphs: Vec<&str> = translated.split("\n\n")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();

        // 组合原文和译文，每个段落分别对照
        let mut results = Vec::new();
        for (i, original) in sub_paragraphs.iter().enumerate() {
            let translated_para = if i < translated_paragraphs.len() {
                translated_paragraphs[i]
            } else {
                // 如果翻译结果段落不够，使用原文
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

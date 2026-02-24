use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

// DeepSeek 上下文限制：每个段落最大字符数
// DeepSeek-V3: 64K tokens, DeepSeek-V3-32K: 32K tokens
// 按 token ≈ 0.75 个字符计算，设置保守的上限
const MAX_CHARS_PER_SEGMENT: usize = 8000; // 约 6000 tokens，留有余量

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
    /// Splits paragraphs that exceed MAX_CHARS_PER_SEGMENT into smaller chunks
    pub fn extract_paragraphs(&self, content: &str) -> Vec<String> {
        let mut paragraphs = Vec::new();

        // First try: extract by <p> tags
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
                            self.split_and_push_paragraph(&mut paragraphs, current.trim().to_string());
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
                            self.split_and_push_paragraph(&mut paragraphs, current.trim().to_string());
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
                self.split_and_push_paragraph(&mut paragraphs, current.trim().to_string());
            }
        }

        // Second fallback: split by double newlines (plain text paragraphs)
        if paragraphs.is_empty() {
            for para in content.split("\n\n") {
                let trimmed = para.trim();
                if !trimmed.is_empty() {
                    self.split_and_push_paragraph(&mut paragraphs, trimmed.to_string());
                }
            }
        }

        // Final fallback: treat entire content as one paragraph
        if paragraphs.is_empty() && !content.trim().is_empty() {
            self.split_and_push_paragraph(&mut paragraphs, content.trim().to_string());
        }

        paragraphs
    }

    /// Split a paragraph if it exceeds MAX_CHARS_PER_SEGMENT
    fn split_and_push_paragraph(&self, paragraphs: &mut Vec<String>, paragraph: String) {
        if paragraph.len() <= MAX_CHARS_PER_SEGMENT {
            paragraphs.push(paragraph);
            return;
        }

        // Need to split the paragraph
        // Try to split at sentence boundaries (。！？.!? etc)
        let mut chunks = Vec::new();
        let mut current_chunk = String::new();
        let mut chars = paragraph.chars().peekable();

        while chars.peek().is_some() {
            let c = chars.next().unwrap();
            current_chunk.push(c);

            // Check if we should split
            if current_chunk.len() >= MAX_CHARS_PER_SEGMENT {
                // Look ahead to find a good breaking point
                let mut temp = String::new();
                let mut found_break = false;

                // Check next 100 characters for sentence ending
                for next_c in chars.by_ref().take(100) {
                    temp.push(next_c);
                    // Sentence endings in Chinese and English
                    if "。！？.!?".contains(next_c) {
                        current_chunk.push_str(&temp);
                        chunks.push(current_chunk.clone());
                        current_chunk.clear();
                        temp.clear();
                        found_break = true;
                        break;
                    }
                }

                if !found_break {
                    // No sentence ending found, force split at current position
                    chunks.push(current_chunk.clone());
                    current_chunk = temp;
                }
            }
        }

        // Add remaining content
        if !current_chunk.is_empty() {
            chunks.push(current_chunk);
        }

        paragraphs.extend(chunks);
    }

    /// Translate a single paragraph and return bilingual format
    pub async fn translate_single_paragraph_bilingual(
        &self,
        paragraph: &str,
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

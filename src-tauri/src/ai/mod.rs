use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

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

    /// Translate content with segmentation for long content
    /// Returns bilingual format with original and translated segments paired
    pub async fn translate_bilingual_segmented(
        &self,
        content: &str,
        source_lang: &str,
        target_lang: &str,
    ) -> Result<String, AiError> {
        // Estimate token count (rough approximation: 1 token ≈ 4 characters for English, less for Chinese)
        // Use a conservative estimate and split if content is likely too long
        const MAX_SEGMENT_CHARS: usize = 3000;

        if content.len() <= MAX_SEGMENT_CHARS {
            // Short content, translate directly
            return self.translate_single_segment_bilingual(content, source_lang, target_lang).await;
        }

        // Split content into segments by HTML tags or paragraphs
        let segments = self.split_html_content(content, MAX_SEGMENT_CHARS);
        let mut results = Vec::new();

        for (index, segment) in segments.iter().enumerate() {
            if segment.trim().is_empty() {
                continue;
            }

            match self.translate_single_segment_bilingual(segment, source_lang, target_lang).await {
                Ok(result) => {
                    results.push(result);
                }
                Err(e) => {
                    // Log error but continue with other segments
                    eprintln!("Failed to translate segment {}: {}", index + 1, e);
                    // Keep original segment as fallback
                    results.push(format!(
                        r#"<div class="translation-segment-failed"><!-- Segment {} translation failed --><div class="original-content">{}</div></div>"#,
                        index + 1, segment
                    ));
                }
            }
        }

        Ok(results.join("\n"))
    }

    /// Translate a single segment and return bilingual format
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

    /// Split HTML content into segments while preserving structure
    fn split_html_content(&self, content: &str, max_chars: usize) -> Vec<String> {
        let mut segments = Vec::new();
        let mut current_segment = String::new();
        let mut depth: i32 = 0; // Track HTML tag depth
        let mut chars_since_last_split = 0;

        let mut iter = content.chars().peekable();
        let mut in_tag = false;

        while let Some(c) = iter.next() {
            current_segment.push(c);

            match c {
                '<' => {
                    in_tag = true;
                    if let Some(&'/') = iter.peek() {
                        // Closing tag
                        if let Some(next_c) = iter.next() {
                            current_segment.push(next_c);
                        }
                    } else {
                        // Opening tag - increase depth
                        depth += 1;
                    }
                }
                '>' => {
                    in_tag = false;
                    // Check if we should split here
                    if depth == 0 && chars_since_last_split > max_chars {
                        // Look ahead to see if we're at a good breaking point
                        let remaining: String = iter.clone().collect();
                        if remaining.starts_with("</p>") || remaining.starts_with("</div>") ||
                           remaining.starts_with("<p") || remaining.starts_with("<div") ||
                           remaining.starts_with("<h") || remaining.starts_with("</h") {
                            segments.push(current_segment.clone());
                            current_segment.clear();
                            chars_since_last_split = 0;
                        }
                    }
                }
                _ => {
                    if !in_tag {
                        chars_since_last_split += c.len_utf8();
                        // Decrease depth on closing tags
                        if c == '/' && current_segment.ends_with("</") {
                            depth = depth.saturating_sub(1);
                        }
                    }
                }
            }
        }

        // Add remaining content
        if !current_segment.is_empty() {
            segments.push(current_segment);
        }

        // Fallback: if segmentation failed, just split by character count
        if segments.is_empty() || segments.len() == 1 && segments[0].len() > max_chars {
            segments.clear();
            for chunk in content.as_bytes().chunks(max_chars) {
                segments.push(String::from_utf8_lossy(chunk).to_string());
            }
        }

        segments
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

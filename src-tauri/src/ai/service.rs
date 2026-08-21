use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{oneshot, Mutex, RwLock};
use tokio::time::{sleep, Instant};

use crate::ai::*;
use crate::ai::activity::current_ai_task;
use crate::error::{AppError, Result};

// ---------------------------------------------------------------------------
// LLM call throttling and task visibility.
//
// Every model request goes through this gate. Calls stay serialized and are
// spaced apart, while interactive work jumps ahead of background batches.
// The task-local activity context lets this lower layer update the same
// status snapshot that the frontend sees, including queue waiting.
// ---------------------------------------------------------------------------

/// Minimum spacing between the start of two consecutive LLM calls.
const LLM_MIN_INTERVAL_MS: u64 = 1200;

struct LlmGateState {
    busy: bool,
    last_start: Option<Instant>,
    queue: VecDeque<oneshot::Sender<()>>,
}

struct LlmGate {
    state: Mutex<LlmGateState>,
}

fn llm_gate() -> &'static LlmGate {
    static GATE: OnceLock<LlmGate> = OnceLock::new();
    GATE.get_or_init(|| LlmGate {
        state: Mutex::new(LlmGateState {
            busy: false,
            last_start: None,
            queue: VecDeque::new(),
        }),
    })
}

fn min_interval() -> Duration {
    Duration::from_millis(LLM_MIN_INTERVAL_MS)
}

struct LlmPermit {
    gate: Option<&'static LlmGate>,
}

impl Drop for LlmPermit {
    fn drop(&mut self) {
        let Some(gate) = self.gate.take() else {
            return;
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                gate.wake_next().await;
            });
        }
    }
}

/// Acquire the serialized slot. Priority callers are inserted before queued
/// background classification calls.
async fn llm_acquire(priority: bool) -> LlmPermit {
    let gate = llm_gate();
    let task = current_ai_task();
    let priority = task.as_ref().map(|task| task.priority()).unwrap_or(priority);
    let (tx, rx) = oneshot::channel();
    let (fast_path, initial_wait) = {
        let mut state = gate.state.lock().await;
        let wait = state
            .last_start
            .map(|started| min_interval().saturating_sub(started.elapsed()))
            .unwrap_or(Duration::ZERO);
        if !state.busy && state.queue.is_empty() {
            // Reserve the slot while sleeping out the rate-limit interval.
            // Without this reservation, a caller arriving between two
            // requests could enqueue forever after the previous permit had
            // already been dropped.
            state.busy = true;
            if wait.is_zero() {
                state.last_start = Some(Instant::now());
                (true, None)
            } else {
                (true, Some(wait))
            }
        } else {
            if priority {
                state.queue.push_front(tx);
            } else {
                state.queue.push_back(tx);
            }
            (false, None)
        }
    };

    if fast_path {
        if let Some(task) = &task {
            if initial_wait.is_some() {
                task.waiting().await;
            }
        }
        if let Some(wait) = initial_wait {
            sleep(wait).await;
            let mut state = gate.state.lock().await;
            state.last_start = Some(Instant::now());
        }
        if let Some(task) = task {
            task.running().await;
        }
        return LlmPermit { gate: Some(gate) };
    }

    if let Some(task) = &task {
        task.waiting().await;
    }
    let _ = rx.await;
    if let Some(task) = task {
        task.running().await;
    }
    LlmPermit { gate: Some(gate) }
}

impl LlmGate {
    async fn wake_next(&self) {
        let (tx, wait) = {
            let mut state = self.state.lock().await;
            let Some(tx) = state.queue.pop_front() else {
                state.busy = false;
                return;
            };
            state.busy = true;
            let wait = state
                .last_start
                .map(|started| min_interval().saturating_sub(started.elapsed()))
                .unwrap_or(Duration::ZERO);
            (tx, wait)
        };
        if !wait.is_zero() {
            sleep(wait).await;
        }
        {
            let mut state = self.state.lock().await;
            state.last_start = Some(Instant::now());
        }
        let _ = tx.send(());
    }
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

/// Categories of LLM/HTTP failures. Drives the retry policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LlmErrorKind {
    /// Retryable: 5xx, 408, 429, network errors
    Transient,
    /// Non-retryable: 4xx (auth, bad request, not found) and parse errors
    Permanent,
}

// ---------------------------------------------------------------------------
// Trait definition
// ---------------------------------------------------------------------------

/// AI service trait — translate and classify content via LLM.
#[async_trait]
pub trait AiService: Send + Sync {
    /// Translate a content string with bilingual output (original + translated).
    /// Detects HTML automatically and re-extracts blocks internally.
    async fn translate_bilingual(
        &self,
        content: &str,
        source_lang: &str,
        target_lang: &str,
    ) -> Result<String>;

    /// Translate an already-prepared block (no further extraction or merging).
    /// Used by the streaming pipeline, which chunks content once and then
    /// hands each chunk directly to the LLM.
    async fn translate_block(
        &self,
        block: &str,
        source_lang: &str,
        target_lang: &str,
        is_html: bool,
    ) -> Result<String>;

    /// Classify an article: return tags and a category.
    async fn classify(&self, request: ClassificationRequest) -> Result<ClassificationResponse>;

    /// Classify many articles in ONE LLM call.
    ///
    /// Returns one response per entry, aligned with the input order. Entries
    /// the model skipped or mis-indexed come back as empty tags (never an
    /// error), so one bad row can't fail the whole batch. `existing_tags`
    /// is the current global canonical vocabulary offered to the model.
    async fn classify_batch(
        &self,
        entries: &[crate::ai::BatchClassifyEntry],
        existing_tags: &[String],
    ) -> Result<Vec<ClassificationResponse>>;

    /// Recommend the most worthwhile reads from a candidate list (one LLM
    /// call). Returns `(item_id, reason)` picks in the model's ranking
    /// order; candidates the model mis-indexed are skipped, never fatal.
    async fn recommend_reads(
        &self,
        candidates: &[crate::ai::RecommendCandidate],
    ) -> Result<Vec<crate::ai::Recommendation>>;

    /// Test the LLM API connection.
    async fn test_connection(&self) -> Result<String>;

    /// Expose the effective `max_chars_per_segment` so callers (e.g. the
    /// streaming pipeline) can chunk content consistently with what the
    /// service would do internally.
    fn config_max_chars(&self) -> usize;
}

/// Shared, replaceable AI service used by commands and the feed pipeline.
///
/// Keeping the slot behind an `Arc<RwLock<...>>` means saving AI settings can
/// update auto-classification immediately without restarting the app.
pub type SharedAiService = Arc<RwLock<Option<Arc<dyn AiService>>>>;

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

    fn max_chars(&self) -> usize {
        self.config.max_chars_per_segment.unwrap_or(MAX_CHARS_PER_SEGMENT)
    }

    /// Send an interactive chat completion request.
    async fn send_request(&self, request: &ChatRequest) -> Result<String> {
        self.send_request_with_priority(request, true).await
    }

    /// Send a background request, behind interactive work in the queue.
    async fn send_request_background(&self, request: &ChatRequest) -> Result<String> {
        self.send_request_with_priority(request, false).await
    }

    async fn send_request_with_priority(
        &self,
        request: &ChatRequest,
        priority: bool,
    ) -> Result<String> {
        let _permit = llm_acquire(priority).await;

        let url = format!("{}/chat/completions", self.config.base_url.trim_end_matches('/'));

        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.config.api_key)
            .json(request)
            .send()
            .await
            .map_err(|e| AppError::Network(format!("LLM request failed: {}", e)))?;

        let status = response.status();
        if !status.is_success() {
            // Read but truncate the body so a KB-scale HTML error page can't
            // blow up the AppError payload.
            let body = response
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(500)
                .collect::<String>();
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
            .map(|c| strip_think_tags(&c.message.content))
            .ok_or_else(|| AppError::Parse("LLM returned no choices".into()))
    }

    /// Build the (system, user) prompt pair for translating a single block.
    fn build_translation_prompts(
        &self,
        block: &str,
        source_lang: &str,
        target_lang: &str,
        is_html: bool,
    ) -> (String, String) {
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
                The text may contain Markdown formatting (**bold**, [links](url), # headings, etc.).\n\
                CRITICAL RULES:\n\
                1. In the paragraph-original div, PRESERVE all Markdown formatting syntax exactly as-is.\n\
                2. In the paragraph-translated div, output only clean translated text — do NOT add HTML or Markdown formatting.\n\
                3. Do NOT wrap the content in HTML tags like <p> or <span>.\n\
                Output format:\n\
                <div class=\"translation-paragraph\">\n\
                <div class=\"paragraph-original\">[ORIGINAL]</div>\n\
                <div class=\"paragraph-translated\">[TRANSLATED]</div>\n\
                </div>",
                source_lang, target_lang
            )
        };
        (system_prompt, block.to_string())
    }

    /// Classify a network/HTTP error as transient (retry) or permanent (give up).
    fn classify_error(err: &AppError) -> LlmErrorKind {
        match err {
            AppError::Network(msg) => {
                let lower = msg.to_lowercase();
                if lower.contains("connect")
                    || lower.contains("timeout")
                    || lower.contains("429")
                    || lower.contains("408")
                    || lower.contains(" 5")
                {
                    LlmErrorKind::Transient
                } else {
                    LlmErrorKind::Permanent
                }
            }
            AppError::Parse(_) => LlmErrorKind::Permanent,
            _ => LlmErrorKind::Permanent,
        }
    }

    /// Retry a fallible async operation, but only for transient errors.
    async fn with_retry<F, Fut, T>(&self, f: F) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let mut last_err: Option<AppError> = None;
        for attempt in 0..=MAX_RETRIES {
            match f().await {
                Ok(val) => return Ok(val),
                Err(e) => {
                    let kind = Self::classify_error(&e);
                    if kind == LlmErrorKind::Permanent || attempt == MAX_RETRIES {
                        return Err(e);
                    }
                    last_err = Some(e);
                    let delay = Duration::from_millis(500 * 2_u64.pow(attempt as u32));
                    sleep(delay).await;
                }
            }
        }
        Err(last_err.unwrap_or_else(|| AppError::Internal("retry loop exited unexpectedly".into())))
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
        let is_html = is_html_content(content);
        let blocks = extract_blocks(content, self.max_chars());

        let mut results = Vec::new();
        for block in &blocks {
            let trimmed = block.trim();
            if trimmed.is_empty() {
                continue;
            }
            let translated = self
                .translate_block(trimmed, source_lang, target_lang, is_html)
                .await?;
            results.push(translated);
        }

        Ok(results.join("\n"))
    }

    async fn translate_block(
        &self,
        block: &str,
        source_lang: &str,
        target_lang: &str,
        is_html: bool,
    ) -> Result<String> {
        let (system_prompt, user_prompt) =
            self.build_translation_prompts(block, source_lang, target_lang, is_html);

        let model = self.config.model.clone();
        let max_tokens = self.config.max_tokens;
        let temperature = self.config.temperature;

        self.with_retry(|| async {
            let req = ChatRequest {
                model: model.clone(),
                messages: vec![
                    ChatMessage {
                        role: "system".into(),
                        content: system_prompt.clone(),
                    },
                    ChatMessage {
                        role: "user".into(),
                        content: user_prompt.clone(),
                    },
                ],
                max_tokens,
                temperature,
            };
            self.send_request(&req).await
        })
        .await
    }

    async fn classify(&self, request: ClassificationRequest) -> Result<ClassificationResponse> {
        let system_prompt = "You are an article classification assistant. Given an article's title, description, and content snippet, \
            classify it by returning a JSON object with:\n\
            - \"tags\": array of 1-3 durable subject tags\n\
            - \"category\": a single category string (e.g., \"technology\", \"science\", \"politics\", \"entertainment\", \"sports\", \"business\", \"health\", \"education\", \"other\")\n\
            Prefer an existing canonical tag exactly when it describes the same or a closely related subject. Only propose a new tag when no existing tag represents the subject. New tags must be lowercase English snake_case. Do not use generic labels such as news, article, or important. Respond with ONLY the JSON object, no other text.";

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

        let model = self.config.model.clone();
        let req = ChatRequest {
            model: model.clone(),
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

        let response = self.send_request(&req).await?;
        parse_classification_json(&response)
    }

    async fn classify_batch(
        &self,
        entries: &[crate::ai::BatchClassifyEntry],
        existing_tags: &[String],
    ) -> Result<Vec<ClassificationResponse>> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }

        let system_prompt = format!("You are an article classification assistant. You will receive a numbered list of article titles. For EACH article, classify it by title alone and return a JSON array where every element is:\n\
            {{\"index\": <the article number>, \"tags\": [1-3 durable subject tags], \"category\": \"<one of: technology, science, politics, entertainment, sports, business, health, education, other>\"}}\n\
            Reuse an existing canonical tag exactly when it describes the same or a closely related subject. Only propose a new tag when no existing tag represents the subject. New tags must be lowercase English snake_case. Avoid generic labels such as news, article, or important. Existing canonical tags: {}\n\
            Respond with ONLY the JSON array, one element per input article, no other text.", existing_tags.join(", "));

        let mut user_message = String::new();
        for e in entries {
            user_message.push_str(&format!("[{}] {}\n", e.index, e.title));
        }

        let req = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![
                ChatMessage { role: "system".into(), content: system_prompt },
                ChatMessage { role: "user".into(), content: user_message },
            ],
            max_tokens: Some(2000),
            temperature: Some(0.1),
        };

        let response = self.send_request_background(&req).await?;
        Ok(parse_classification_batch_json(&response, entries.len()))
    }

    async fn recommend_reads(
        &self,
        candidates: &[crate::ai::RecommendCandidate],
    ) -> Result<Vec<crate::ai::Recommendation>> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let system_prompt = format!(
            "You are a discerning editor curating a personal reading list. From the numbered candidate articles, \
            select the {} most worth reading now — prioritize substance, insight and novelty over clickbait. \
            Respond with ONLY a JSON array, one element per pick, in priority order (best first):\n\
            {{\"index\": <candidate number>, \"reason\": \"<one short sentence in 简体中文 saying why it is worth reading>\"}}\n\
            No other text.",
            crate::ai::RECOMMEND_PICK_COUNT.min(candidates.len()).max(1)
        );

        let mut user_message = String::new();
        for (i, c) in candidates.iter().enumerate() {
            user_message.push_str(&format!("[{}] {}\n", i, c.context));
        }

        let req = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![
                ChatMessage { role: "system".into(), content: system_prompt },
                ChatMessage { role: "user".into(), content: user_message },
            ],
            max_tokens: Some(600),
            temperature: Some(0.3),
        };

        let response = self.send_request(&req).await?;
        Ok(parse_recommendation_json(&response, candidates))
    }

    async fn test_connection(&self) -> Result<String> {
        let req = ChatRequest {
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

        self.send_request(&req).await
    }

    fn config_max_chars(&self) -> usize {
        self.max_chars()
    }
}

// ---------------------------------------------------------------------------
// Pure content-processing functions (no LLM calls needed)
// ---------------------------------------------------------------------------

/// Heuristic: does the content look like HTML rather than plain text?
pub fn is_html_content(content: &str) -> bool {
    content.contains('<')
        && (content.contains("</p>")
            || content.contains("</h")
            || content.contains("</div>")
            || content.contains("<br"))
}

/// Extract block-level HTML elements (p, h1-h6, li, blockquote, etc.)
/// Each block is a separate translation unit. Falls back to plain-text
/// paragraph splitting for non-HTML content.
pub fn extract_blocks(content: &str, max_chars: usize) -> Vec<String> {
    let mut blocks = Vec::new();

    if is_html_content(content) {
        blocks = extract_html_blocks(content);
    }

    if blocks.is_empty() {
        blocks = split_markdown_blocks(content);
    }

    if blocks.is_empty() {
        let trimmed = content.trim();
        if !trimmed.is_empty() && trimmed.len() > 5 {
            blocks.push(trimmed.to_string());
        }
    }

    merge_small_blocks(blocks, max_chars)
}

/// Is this line an ATX markdown header (`#` … `######` + text)?
fn is_atx_header(line: &str) -> bool {
    let t = line.trim_start();
    let hashes = t.chars().take_while(|&c| c == '#').count();
    if hashes == 0 || hashes > 6 {
        return false;
    }
    // A space after the hashes is required (or a bare `#` empty header).
    t.as_bytes().get(hashes) == Some(&b' ') || t.len() == hashes
}

/// Is this BLOCK a standalone header translation unit? True for single-line
/// ATX markdown headers and single `<h1>`–`<h6>` HTML blocks — both render
/// as standalone headings, so both must stay individual paragraphs in the
/// bilingual output.
fn is_header_block(block: &str) -> bool {
    let mut lines = block.lines();
    let Some(first) = lines.next() else { return false };
    if lines.next().is_some() {
        return false; // multi-line → a paragraph, not a bare header
    }
    if is_atx_header(first) {
        return true;
    }
    let lower = first.trim_start().to_ascii_lowercase();
    (1..=6).any(|n| {
        lower.starts_with(&format!("<h{}>", n)) || lower.starts_with(&format!("<h{} ", n))
    })
}

/// Split markdown / plain-text content into paragraph blocks.
///
/// Blank lines separate paragraphs. ATX headers (`# Title`) are ALWAYS
/// individual blocks — even without a blank line after them — because they
/// render as standalone headings and must also be standalone translation
/// units (otherwise the header merges with its body into one bilingual
/// paragraph and the display order breaks).
fn split_markdown_blocks(content: &str) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut current = String::new();

    fn flush(current: &mut String, blocks: &mut Vec<String>) {
        let trimmed = current.trim().to_string();
        // Preserve the original >5-char filter for plain paragraphs so tiny
        // fragments ("ok.", "—") stay out of the translation pipeline.
        if trimmed.len() > 5 {
            blocks.push(trimmed);
        }
        current.clear();
    }

    for line in content.lines() {
        if is_atx_header(line) {
            flush(&mut current, &mut blocks);
            // Headers are exempt from the length filter — even `# News`.
            blocks.push(line.trim().to_string());
        } else if line.trim().is_empty() {
            flush(&mut current, &mut blocks);
        } else {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line.trim_end());
        }
    }
    flush(&mut current, &mut blocks);
    blocks
}

/// Extract blocks from HTML content in document order.
/// Uses linear scanning with strict tag boundary checking so `<p` does not
/// accidentally match `<pre>`/`<picture>`/`<path>`.
fn extract_html_blocks(content: &str) -> Vec<String> {
    let block_tags = [
        "p", "h1", "h2", "h3", "h4", "h5", "h6",
        "li", "blockquote", "pre",
        "div", "section", "article",
    ];

    let mut candidates: Vec<(usize, usize, String)> = Vec::new();

    for tag in &block_tags {
        let open_tag = format!("<{}", tag);
        let mut search_start = 0;

        while let Some(start) = content[search_start..].find(&open_tag) {
            let abs_start = search_start + start;
            let bytes = content.as_bytes();

            // Boundary check: the char right after `<tag` must be `>`,
            // whitespace, or `/` (i.e., end-of-tag or attribute start).
            // Otherwise it's a prefix match like `<p` matching `<pre>`.
            let after = abs_start + open_tag.len();
            if after >= content.len() {
                break;
            }
            let next_ch = bytes[after];
            if !(next_ch == b'>' || next_ch == b' ' || next_ch == b'\t' || next_ch == b'\n' || next_ch == b'/') {
                // Advance past this false hit and keep scanning
                search_start = abs_start + 1;
                continue;
            }

            let tag_end = match content[abs_start..].find('>') {
                Some(pos) => abs_start + pos + 1,
                None => break,
            };

            let remaining = &content[tag_end..];
            let close_pos = match find_matching_close(remaining, tag) {
                Some(p) => tag_end + p,
                None => break,
            };

            let block = content[abs_start..close_pos].to_string();
            if !block.trim().is_empty() {
                candidates.push((abs_start, close_pos, block));
            }

            search_start = close_pos;
        }
    }

    candidates.sort_by_key(|(start, _, _)| *start);

    // Deduplicate nested blocks: keep only the outermost wrapper when one
    // block fully contains another.
    let mut deduplicated: Vec<(usize, usize, String)> = Vec::new();
    'outer: for &(start, end, ref block) in &candidates {
        for &(other_start, other_end, ref other) in &candidates {
            if other_start == start && other_end == end {
                continue;
            }
            if other_start <= start && other_end >= end && other.len() > block.len() {
                continue 'outer;
            }
        }
        deduplicated.push((start, end, block.clone()));
    }

    deduplicated.into_iter().map(|(_, _, block)| block).collect()
}

/// Find the position of the `</tag>` that pairs with the (already-opened)
/// `<tag` at the start of `html`, respecting nesting for the same tag.
/// Returns the position immediately after the closing tag.
fn find_matching_close(html: &str, tag: &str) -> Option<usize> {
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let mut depth: usize = 1;
    let mut pos = 0;
    while pos < html.len() {
        let next_open = html[pos..].find(&open).map(|p| pos + p);
        let next_close = html[pos..].find(&close).map(|p| pos + p);
        match (next_open, next_close) {
            (None, None) => return None,
            (None, Some(c)) => {
                depth -= 1;
                if depth == 0 {
                    return Some(c + close.len());
                }
                pos = c + close.len();
            }
            (Some(_), Some(c)) if next_open.unwrap() >= c => {
                depth -= 1;
                if depth == 0 {
                    return Some(c + close.len());
                }
                pos = c + close.len();
            }
            (Some(o), _) => {
                let after = o + open.len();
                let bytes = html.as_bytes();
                if after < bytes.len() {
                    let ch = bytes[after];
                    if ch == b'>' || ch == b' ' || ch == b'\t' || ch == b'\n' || ch == b'/' {
                        depth += 1;
                    }
                }
                pos = o + open.len();
            }
        }
    }
    None
}

/// Merge small blocks together to reduce translation API calls.
/// For HTML blocks larger than `max_chars`, uses [`split_html_block`] (tag-aware)
/// so the split point doesn't fall inside an attribute value or tag name.
/// Falls back to [`split_large_paragraph`] for plain text.
pub fn merge_small_blocks(blocks: Vec<String>, max_chars: usize) -> Vec<String> {
    let mut merged = Vec::new();
    let mut current = String::new();
    let mut current_len = 0;

    let container_closing = [
        "</ul>", "</ol>", "</table>", "</div>", "</section>", "</blockquote>", "</pre>",
    ];

    for block in &blocks {
        let block_len = block.len();

        // Headers are standalone translation units — never merged with
        // neighbouring paragraphs. A merged "# Title + body" block renders
        // as ONE bilingual paragraph and breaks the heading/body pairing.
        if is_header_block(block) {
            if !current.is_empty() {
                merged.push(current.clone());
                current.clear();
                current_len = 0;
            }
            merged.push(block.clone());
            continue;
        }

        if block_len > max_chars {
            if !current.is_empty() {
                merged.push(current.clone());
                current.clear();
                current_len = 0;
            }
            // Tag-aware split for HTML, plain-text split for everything else
            let looks_html = is_html_content(block);
            let chunks = if looks_html {
                split_html_block(block, max_chars)
            } else {
                split_large_paragraph(block, max_chars)
            };
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

/// Tag-aware splitter for oversized HTML blocks. Splits only at text-content
/// boundaries (between elements), never inside a tag or attribute value.
pub fn split_html_block(block: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len = 0;
    let mut in_tag = false;
    let mut tag_buffer = String::new();

    for c in block.chars() {
        match c {
            '<' => {
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
                    if current_len + 1 > max_chars && !current.is_empty() {
                        chunks.push(current.clone());
                        current.clear();
                        current_len = 0;
                    }
                    current.push(c);
                    current_len += 1;
                }
            }
        }
    }

    flush_buffer(
        &mut tag_buffer,
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
    if buf.is_empty() {
        return;
    }
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

/// Split a large paragraph at sentence boundaries (plain text only — does not
/// understand HTML structure).
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

/// Parse a JSON classification response from the LLM. Tolerates ``` fenced
/// responses (which many models emit despite the prompt's instructions).
pub fn parse_classification_json(response: &str) -> Result<ClassificationResponse> {
    let trimmed = response.trim();

    // Strip ``` fence (```json ... ``` or ``` ... ```) if present
    let unbraced = if trimmed.starts_with("```") {
        let after_open = trimmed
            .find('\n')
            .map(|i| i + 1)
            .unwrap_or(3);
        let close = trimmed.rfind("```").unwrap_or(trimmed.len());
        trimmed[after_open..close].trim()
    } else {
        trimmed
    };

    // Take the substring between the first { and the last }
    let start = unbraced.find('{');
    let end = unbraced.rfind('}');
    let json_slice = match (start, end) {
        (Some(s), Some(e)) if e > s => &unbraced[s..=e],
        _ => unbraced,
    };

    let value: serde_json::Value = serde_json::from_str(json_slice).map_err(|e| {
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

/// Parse the JSON-array response of a batch classification call.
///
/// Tolerates ``` fences and preamble text, and maps each element back to its
/// input position via the echoed `index` field. Missing/duplicate indices
/// yield empty responses at those positions so the result always has exactly
/// `expected_len` elements aligned with the input.
pub fn parse_classification_batch_json(
    response: &str,
    expected_len: usize,
) -> Vec<ClassificationResponse> {
    let mut out: Vec<ClassificationResponse> = (0..expected_len)
        .map(|_| ClassificationResponse { tags: Vec::new(), category: None })
        .collect();

    let trimmed = response.trim();
    let unbraced = if trimmed.starts_with("```") {
        let after_open = trimmed.find('\n').map(|i| i + 1).unwrap_or(3);
        let close = trimmed.rfind("```").unwrap_or(trimmed.len());
        trimmed[after_open..close].trim()
    } else {
        trimmed
    };

    let start = unbraced.find('[');
    let end = unbraced.rfind(']');
    let json_slice = match (start, end) {
        (Some(s), Some(e)) if e > s => &unbraced[s..=e],
        _ => return out,
    };

    let Ok(serde_json::Value::Array(arr)) = serde_json::from_str::<serde_json::Value>(json_slice)
    else {
        return out;
    };

    for el in arr {
        let Some(idx) = el["index"].as_u64() else { continue };
        if idx as usize >= expected_len {
            continue;
        }
        let tags = el["tags"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let category = el["category"].as_str().map(String::from);
        out[idx as usize] = ClassificationResponse { tags, category };
    }

    out
}

/// Parse the JSON-array response of a read-recommendation call.
///
/// Same tolerance contract as [`parse_classification_batch_json`]: ``` fences
/// and preamble text are stripped; indices are mapped back to candidate
/// `item_id`s; out-of-range and duplicate picks are dropped. Elements
/// without a usable reason are dropped too (an empty reason row in the UI
/// is worse than one fewer pick).
pub fn parse_recommendation_json(
    response: &str,
    candidates: &[crate::ai::RecommendCandidate],
) -> Vec<crate::ai::Recommendation> {
    let trimmed = response.trim();
    let unbraced = if trimmed.starts_with("```") {
        let after_open = trimmed.find('\n').map(|i| i + 1).unwrap_or(3);
        let close = trimmed.rfind("```").unwrap_or(trimmed.len());
        trimmed[after_open..close].trim()
    } else {
        trimmed
    };

    let start = unbraced.find('[');
    let end = unbraced.rfind(']');
    let json_slice = match (start, end) {
        (Some(s), Some(e)) if e > s => &unbraced[s..=e],
        _ => return Vec::new(),
    };

    let Ok(serde_json::Value::Array(arr)) = serde_json::from_str::<serde_json::Value>(json_slice)
    else {
        return Vec::new();
    };

    let mut out: Vec<crate::ai::Recommendation> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for el in arr {
        let Some(idx) = el["index"].as_u64() else { continue };
        let Some(cand) = candidates.get(idx as usize) else { continue };
        let Some(reason) = el["reason"].as_str() else { continue };
        let reason = reason.trim();
        if reason.is_empty() || !seen.insert(cand.item_id) {
            continue;
        }
        out.push(crate::ai::Recommendation {
            item_id: cand.item_id,
            reason: reason.to_string(),
        });
    }
    out
}

/// Strip `<think>...</think>` blocks from reasoning model responses.
fn strip_think_tags(response: &str) -> String {
    if !response.contains("<think>") {
        return response.to_string();
    }

    let mut result = String::with_capacity(response.len());
    let mut pos = 0;

    while let Some(start) = response[pos..].find("<think>") {
        let abs_start = pos + start;
        result.push_str(&response[pos..abs_start]);

        if let Some(end) = response[abs_start..].find("</think>") {
            pos = abs_start + end + "</think>".len();
        } else {
            pos = response.len();
            break;
        }
    }

    result.push_str(&response[pos..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Regression for the `<p` prefix bug: `<p` was matching `<pre>`,
    /// `<picture>`, `<path>` and swallowing unrelated content.
    #[test]
    fn test_extract_html_blocks_does_not_match_pre_with_p_prefix() {
        let html = r#"<pre>print('hi')</pre><p>real paragraph</p>"#;
        let blocks = extract_html_blocks(html);
        // The pre block must contain "print('hi')"
        let all = blocks.join("\n");
        assert!(all.contains("print('hi')"), "got: {}", all);
        assert!(all.contains("real paragraph"));
    }

    #[test]
    fn test_extract_html_blocks_preserves_document_order() {
        let html = r#"<h1>Title</h1><p>first</p><h2>Sub</h2><p>second</p>"#;
        let blocks = extract_html_blocks(html);
        assert!(blocks.len() >= 4);
        // The first block should contain "Title"
        assert!(blocks[0].contains("Title"));
    }

    // -----------------------------------------------------------------------
    // headers as individual translation paragraphs
    // -----------------------------------------------------------------------

    /// Regression (issue #2 follow-up): a markdown header used to merge with
    /// the following paragraph in `merge_small_blocks`, so the bilingual
    /// output showed "# Title + body" as ONE paragraph.
    #[test]
    fn test_extract_blocks_header_is_individual_paragraph() {
        let md = "# Chapter One\n\nThis is the first body paragraph of the chapter.";
        let blocks = extract_blocks(md, MAX_CHARS_PER_SEGMENT);
        assert!(blocks.len() >= 2, "header must not merge with body, got {:?}", blocks);
        assert_eq!(blocks[0], "# Chapter One");
        assert!(blocks[1].contains("first body paragraph"));
    }

    /// Header directly followed by body text (no blank line) must still be
    /// its own block — the old `split("\n\n")` kept them glued together.
    #[test]
    fn test_extract_blocks_header_without_blank_line() {
        let md = "## Section Title\nBody starts right here without a blank line.";
        let blocks = extract_blocks(md, MAX_CHARS_PER_SEGMENT);
        assert!(blocks.len() >= 2, "got {:?}", blocks);
        assert_eq!(blocks[0], "## Section Title");
        assert!(blocks[1].contains("without a blank line"));
    }

    /// Short headers translate too — exempt from the >5-char paragraph filter.
    #[test]
    fn test_extract_blocks_short_header_kept() {
        let blocks = extract_blocks("# News\n\nA longer body paragraph follows here.", MAX_CHARS_PER_SEGMENT);
        assert!(blocks.iter().any(|b| b == "# News"), "got {:?}", blocks);
    }

    /// Header levels 1-6 and setext-lookalikes are classified correctly.
    #[test]
    fn test_is_atx_header() {
        assert!(is_atx_header("# Top"));
        assert!(is_atx_header("###### Six"));
        assert!(is_atx_header("  ## Indented"));
        assert!(!is_atx_header("####### Seven")); // 7 hashes = not a header
        assert!(!is_atx_header("#NoSpace"));      // requires space
        assert!(!is_atx_header("Plain text"));
        assert!(!is_atx_header("C# code"));
    }

    /// HTML headers stay individual too (same display rule as markdown).
    #[test]
    fn test_extract_blocks_html_header_not_merged() {
        let html = "<h2>Heading</h2><p>Body paragraph content here.</p>";
        let blocks = extract_blocks(html, MAX_CHARS_PER_SEGMENT);
        assert!(blocks.len() >= 2, "got {:?}", blocks);
        assert!(blocks[0].contains("<h2>"));
        assert!(blocks[1].contains("<p>"));
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
            .map(|_| format!("<p>{}</p>", "内容".repeat(400)))
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

    /// Regression: large HTML blocks must now use split_html_block and never
    /// be cut inside an attribute value or tag name.
    #[test]
    fn test_merge_small_blocks_large_html_does_not_cut_attributes() {
        let html = format!(
            r#"<img src="https://example.com/{}.png" alt="{}" />"#,
            "x".repeat(2000),
            "alt text",
        );
        let merged = merge_small_blocks(vec![html.clone()], MAX_CHARS_PER_SEGMENT);
        // Join everything and confirm no attr is left half-open
        let joined = merged.join("");
        // Tag boundaries should remain balanced
        assert_eq!(joined.matches('<').count(), joined.matches('>').count());
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

    #[test]
    fn test_split_html_block_does_not_split_inside_tag() {
        // Attribute value longer than max_chars — must NOT be cut
        let html = format!(r#"<a href="{}">link</a>"#, "x".repeat(MAX_CHARS_PER_SEGMENT + 100));
        let chunks = split_html_block(&html, MAX_CHARS_PER_SEGMENT);
        let joined: String = chunks.join("");
        assert_eq!(joined, html);
        // Tag/attribute should still be intact in some chunk
        assert!(chunks.iter().any(|c| c.starts_with("<a ")));
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

    /// Regression: many models wrap JSON in ```json ... ``` fences.
    #[test]
    fn test_parse_classification_json_strips_fenced_block() {
        let wrapped = "```json\n{\"tags\":[\"a\"],\"category\":\"x\"}\n```";
        let result = parse_classification_json(wrapped).unwrap();
        assert_eq!(result.tags, vec!["a"]);
        assert_eq!(result.category, Some("x".into()));
    }

    /// Regression: models occasionally add preamble like "Here is the JSON:".
    #[test]
    fn test_parse_classification_json_extracts_json_substring() {
        let wrapped = "Here you go: {\"tags\":[\"a\"]} -- that's it.";
        let result = parse_classification_json(wrapped).unwrap();
        assert_eq!(result.tags, vec!["a"]);
    }

    // -----------------------------------------------------------------------
    // parse_classification_batch_json
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_batch_valid() {
        let resp = r#"[{"index":0,"tags":["rust"],"category":"technology"},{"index":1,"tags":["ai"],"category":"science"}]"#;
        let out = parse_classification_batch_json(resp, 2);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].tags, vec!["rust"]);
        assert_eq!(out[1].category.as_deref(), Some("science"));
    }

    #[test]
    fn test_parse_batch_missing_entry_fills_empty() {
        // Model only answered index 1 — index 0 must come back empty, not
        // shift into the wrong slot.
        let resp = r#"[{"index":1,"tags":["ai"],"category":"other"}]"#;
        let out = parse_classification_batch_json(resp, 2);
        assert_eq!(out.len(), 2);
        assert!(out[0].tags.is_empty());
        assert_eq!(out[1].tags, vec!["ai"]);
    }

    #[test]
    fn test_parse_batch_out_of_range_index_ignored() {
        let resp = r#"[{"index":5,"tags":["x"],"category":"other"}]"#;
        let out = parse_classification_batch_json(resp, 2);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|r| r.tags.is_empty()));
    }

    #[test]
    fn test_parse_batch_fenced_and_preamble() {
        let resp = "Here is the result:\n```json\n[{\"index\":0,\"tags\":[\"a\",\"b\"],\"category\":null}]\n```";
        let out = parse_classification_batch_json(resp, 1);
        assert_eq!(out[0].tags, vec!["a", "b"]);
        assert_eq!(out[0].category, None);
    }

    #[test]
    fn test_parse_batch_garbage_returns_all_empty() {
        let out = parse_classification_batch_json("not json at all", 3);
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|r| r.tags.is_empty()));
    }

    // -----------------------------------------------------------------------
    // parse_recommendation_json
    // -----------------------------------------------------------------------

    fn rec_cands() -> Vec<crate::ai::RecommendCandidate> {
        (0..3)
            .map(|i| crate::ai::RecommendCandidate {
                item_id: 100 + i,
                context: format!("ctx {}", i),
            })
            .collect()
    }

    #[test]
    fn test_parse_recommendation_valid() {
        let resp = r#"[{"index":2,"reason":"深度好文"},{"index":0,"reason":"时效性强"}]"#;
        let out = parse_recommendation_json(resp, &rec_cands());
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].item_id, 102);
        assert_eq!(out[0].reason, "深度好文");
        assert_eq!(out[1].item_id, 100);
    }

    #[test]
    fn test_parse_recommendation_out_of_range_and_dup_dropped() {
        let resp = r#"[{"index":9,"reason":"x"},{"index":0,"reason":"a"},{"index":0,"reason":"b"}]"#;
        let out = parse_recommendation_json(resp, &rec_cands());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].item_id, 100);
        assert_eq!(out[0].reason, "a");
    }

    #[test]
    fn test_parse_recommendation_empty_reason_dropped() {
        let resp = r#"[{"index":0,"reason":"  "},{"index":1,"reason":"ok"}]"#;
        let out = parse_recommendation_json(resp, &rec_cands());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].item_id, 101);
    }

    #[test]
    fn test_parse_recommendation_fenced_and_preamble() {
        let resp = "Here are my picks:\n```json\n[{\"index\":1,\"reason\":\"值得读\"}]\n```";
        let out = parse_recommendation_json(resp, &rec_cands());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].item_id, 101);
        assert_eq!(out[0].reason, "值得读");
    }

    #[test]
    fn test_parse_recommendation_garbage_returns_empty() {
        assert!(parse_recommendation_json("nope", &rec_cands()).is_empty());
        assert!(parse_recommendation_json("[]", &rec_cands()).is_empty());
    }

    // -----------------------------------------------------------------------
    // strip_think_tags
    // -----------------------------------------------------------------------

    #[test]
    fn test_strip_think_tags_basic() {
        let result = strip_think_tags("before<think>internal reasoning</think>after");
        assert_eq!(result, "beforeafter");
    }

    #[test]
    fn test_strip_think_tags_no_tags() {
        let text = "normal response without think tags";
        assert_eq!(strip_think_tags(text), text);
    }

    #[test]
    fn test_strip_think_tags_multiple() {
        let result = strip_think_tags("a<think>first</think>b<think>second</think>c");
        assert_eq!(result, "abc");
    }

    #[test]
    fn test_strip_think_tags_unclosed() {
        let result = strip_think_tags("before<think>no closing");
        assert_eq!(result, "before");
    }

    #[test]
    fn test_strip_think_tags_empty_input() {
        assert_eq!(strip_think_tags(""), "");
    }

    // -----------------------------------------------------------------------
    // is_html_content
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_html_content_basic() {
        assert!(is_html_content("<p>hi</p>"));
        assert!(is_html_content("<div>x</div>"));
        assert!(!is_html_content("plain text"));
        assert!(!is_html_content("some <strong>bold</strong> with no closing"));
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
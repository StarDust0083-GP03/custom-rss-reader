use serde::{Deserialize, Serialize};

use crate::error::AppError;

/// AI configuration for LLM API calls.
///
/// `#[serde(default)]` on the struct so config files written by older
/// versions of the app (missing fields added later, e.g.
/// `max_chars_per_segment`) still load — a legacy file must not brick the
/// AI features until the user re-saves the settings.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AiConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub max_chars_per_segment: Option<usize>,
}

impl AiConfig {
    pub fn is_valid(&self) -> Result<(), AppError> {
        if self.api_key.is_empty() {
            return Err(AppError::Validation("API key cannot be empty".into()));
        }
        if self.api_key.contains("****") {
            return Err(AppError::Validation(
                "API key is masked; enter the real key again".into(),
            ));
        }
        if self.base_url.is_empty() {
            return Err(AppError::Validation("Base URL cannot be empty".into()));
        }
        if self.model.is_empty() {
            return Err(AppError::Validation("Model name cannot be empty".into()));
        }
        Ok(())
    }

    /// Turn the stored base URL into the endpoint requests are sent to.
    ///
    /// Providers document a full endpoint (`https://api.example.com/v1/chat/completions`)
    /// and users paste exactly that into "Base URL". Appending the path again
    /// sent every request to `…/chat/completions/chat/completions`, which the
    /// provider answers with a 404 — the failure looks like a bad key or a dead
    /// network and is impossible to spot from the UI. Accepting both forms is
    /// cheaper than explaining the difference.
    pub fn chat_endpoint(&self) -> String {
        let base = self.base_url.trim().trim_end_matches('/');
        let base = base
            .strip_suffix("/chat/completions")
            .unwrap_or(base)
            .trim_end_matches('/');
        format!("{base}/chat/completions")
    }

    /// Rewrite the stored base URL to the canonical form.
    pub fn normalize_base_url(&mut self) {
        let base = self.base_url.trim().trim_end_matches('/');
        let base = base
            .strip_suffix("/chat/completions")
            .unwrap_or(base)
            .trim_end_matches('/');
        self.base_url = base.to_string();
        if self.api_key.starts_with("Bearer ") {
            self.api_key = self.api_key.trim_start_matches("Bearer ").trim().to_string();
        }
    }

    /// Effective output budget, defaulted for reasoning models.
    ///
    /// Models such as MiniMax-M2 write their chain of thought into the same
    /// `content` field as the answer and no request parameter turns it off, so
    /// a budget sized for the answer alone is spent before the answer starts.
    pub fn max_tokens_or_default(&self) -> u32 {
        self.max_tokens.filter(|budget| *budget > 0).unwrap_or(DEFAULT_MAX_TOKENS)
    }

    /// Chars per segment that fit the output budget.
    ///
    /// A bilingual request must return the original *and* the translation, so
    /// the output is roughly twice the input plus reasoning overhead. Chunking
    /// to this bound prevents the truncation instead of recovering from it.
    pub fn segment_chars_for_budget(&self) -> usize {
        chars_for_tokens(self.max_tokens_or_default()).min(self.max_chars_or_default())
    }

    pub fn max_chars_or_default(&self) -> usize {
        self.max_chars_per_segment
            .filter(|chars| *chars > 0)
            .unwrap_or(MAX_CHARS_PER_SEGMENT)
    }

    /// Construct a default config without an API key (for "not yet configured"
    /// responses, where the key must remain empty).
    pub fn default_for(base_url: &str, model: &str) -> Self {
        Self {
            api_key: String::new(),
            base_url: base_url.to_string(),
            model: model.to_string(),
            max_tokens: None,
            temperature: None,
            max_chars_per_segment: None,
        }
    }
}

/// One LLM-written definition for a tag.
///
/// The local encoder cannot relate short bare names such as
/// `machine_learning` to `ai`, which is why grouping on names alone leaves most
/// tags as singletons. A definition gives the encoder real semantics to work
/// with, and persisting it lets the result be inspected and reused.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TagExplanation {
    pub name: String,
    pub explanation: String,
}

/// Max tag names per explanation call.
///
/// Measured against a reasoning model (MiniMax-M2.7-highspeed): 10 definitions
/// take ~35s and ~2.5k completion tokens, almost all of it chain of thought.
/// Larger batches blow past the HTTP client's 120s timeout, which then looks
/// like a frozen UI while the request is silently retried.
pub const EXPLAIN_TAGS_BATCH_SIZE: i64 = 10;

/// Output budget for one explanation call.
///
/// Enough for 10 definitions plus the reasoning text a model such as MiniMax-M2
/// emits into the same field; a truncated answer is reported as an error rather
/// than stored as a short one.
pub const EXPLAIN_MAX_TOKENS: u32 = 4_000;

/// Bumped when the explanation prompt changes, so a dictionary built by an
/// older prompt is recognisable instead of silently reused.
pub const TAG_EXPLANATION_PROMPT_VERSION: i64 = 1;

/// Max stored characters per explanation: enough for a definition plus
/// synonyms, bounded so one verbose answer cannot bloat the index.
pub const MAX_EXPLANATION_CHARS: usize = 300;

/// One tag's place in the topic catalog, as proposed by the model.
///
/// `state` carries the same three values the database accepts, so a proposal is
/// either directly storable or explicitly waiting for a human. `reason` is kept
/// for review: it is the part that lets someone check a proposal without
/// re-reading the prompt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TopicSuggestion {
    pub name: String,
    /// `Some` exactly when `state == "assigned"`.
    pub category_id: Option<i64>,
    pub state: String,
    pub reason: String,
}

/// Max tags per topic-suggestion call.
///
/// Topic placement is intentionally a larger page than dictionary writing: the
/// response is one short verdict per tag, so 40 tags gives useful progress
/// without sending the whole vocabulary in one request.
pub const TOPIC_SUGGEST_BATCH_SIZE: i64 = 40;

/// Output budget for one topic-suggestion call: 40 verdicts plus reasoning.
pub const TOPIC_SUGGEST_MAX_TOKENS: u32 = 8_000;

/// Bumped when the topic-suggestion prompt or its contract changes, so a cached
/// proposal from an older prompt is not treated as current.
pub const TOPIC_SUGGEST_PROMPT_VERSION: i64 = 1;

/// One selectable topic, as handed to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct TopicChoice {
    pub id: i64,
    pub label: String,
    pub definition: String,
}

/// One word to place, with the definition that makes placement possible.
#[derive(Debug, Clone, PartialEq)]
pub struct TopicWordInput {
    pub name: String,
    pub explanation: String,
    pub usage_count: i64,
}

/// Default output budget: enough for a bilingual answer plus reasoning text.
pub const DEFAULT_MAX_TOKENS: u32 = 16_000;

/// Reasoning text a model may emit before the answer, in tokens. Reserved
/// rather than measured: it varies per request and provider.
const REASONING_RESERVE_TOKENS: u32 = 1_500;

/// Conservative tokens-per-character for CJK-heavy text (1 token ≈ 1 char).
const TOKENS_PER_CHAR: u32 = 1;

/// How many characters of source text fit in `max_tokens` of output.
///
/// Output = original + translation (≈ 2 × chars × TOKENS_PER_CHAR) plus the
/// reasoning reserve. Never returns less than a floor, or a tiny budget would
/// produce unusable 20-character segments instead of a clear error.
pub fn chars_for_tokens(max_tokens: u32) -> usize {
    let usable = max_tokens.saturating_sub(REASONING_RESERVE_TOKENS);
    ((usable / (2 * TOKENS_PER_CHAR)) as usize).max(MIN_CHARS_PER_SEGMENT)
}

/// Floor for chunking: below this a segment carries too little context to
/// translate well, so a too-small budget surfaces as a clear error instead.
pub const MIN_CHARS_PER_SEGMENT: usize = 400;



/// Path of the AI configuration file (`~/.rss-reader/ai_config.json`).
pub fn config_path() -> Result<std::path::PathBuf, AppError> {
    let home = dirs::home_dir()
        .ok_or_else(|| AppError::Internal("Failed to get home directory".into()))?;
    Ok(home.join(".rss-reader").join("ai_config.json"))
}

/// Load the saved AI configuration.
///
/// A missing file is not an error: it means "not configured yet", which every
/// caller treats as "AI features are unavailable". Old files that predate a
/// field still load, because `AiConfig` is `#[serde(default)]`.
pub fn load_config() -> Result<AiConfig, AppError> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(AiConfig::default_for(
            crate::commands::ai_commands::DEFAULT_BASE_URL,
            crate::commands::ai_commands::DEFAULT_MODEL,
        ));
    }
    let contents = std::fs::read_to_string(&path)
        .map_err(|e| AppError::Internal(format!("Failed to read config file: {}", e)))?;
    serde_json::from_str(&contents)
        .map_err(|e| AppError::Internal(format!("Failed to parse config file: {}", e)))
}

/// Build an AI service from the saved configuration, if one is usable.
///
/// Both the command layer and the background worker use this. The worker in
/// particular must be able to pick up a configuration that was written *after*
/// the app started, or configuring AI would require a restart before any
/// automatic classification begins.
pub fn load_configured_service() -> Option<std::sync::Arc<dyn service::AiService>> {
    use crate::ai::service::LlmAiService;

    let config = match load_config() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("[ai] cannot read AI configuration: {}", error);
            return None;
        }
    };
    if config.api_key.trim().is_empty() || config.base_url.trim().is_empty() {
        return None;
    }
    match LlmAiService::new(config) {
        Ok(service) => Some(std::sync::Arc::new(service) as std::sync::Arc<dyn service::AiService>),
        Err(error) => {
            eprintln!("[ai] configured service unavailable: {}", error);
            None
        }
    }
}

/// Request payload for AI classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassificationRequest {
    pub title: String,
    pub description: Option<String>,
    pub content_snippet: Option<String>,
    pub rss_title: Option<String>,
    pub existing_tags: Option<Vec<String>>,
}

/// Response from AI classification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClassificationResponse {
    pub tags: Vec<String>,
}

/// One entry in a batch classification request.
///
/// Title only — batch auto-classify runs on every freshly fetched item, so
/// payload size directly drives both token cost and rate-limit pressure.
/// Neither the description nor `content` is sent.
#[derive(Debug, Clone)]
pub struct BatchClassifyEntry {
    /// Position of this entry in the batch (echoed by the LLM response).
    pub index: usize,
    pub title: String,
}

/// One candidate for the read-recommendation feature.
#[derive(Debug, Clone)]
pub struct RecommendCandidate {
    pub item_id: i64,
    /// Pre-formatted context line (source, title, snippet) built by the caller.
    pub context: String,
}

/// A single recommendation picked by the LLM.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Recommendation {
    pub item_id: i64,
    pub reason: String,
}

/// Identity of the text a translation was produced from.
///
/// The cache compares hashes rather than text: comparing would cost as much as
/// hashing and gets whitespace/line-ending differences wrong. The migration
/// that backfills historic rows uses this same function, so a hash written by
/// the upgrade path is directly comparable with one computed at request time.
pub fn translation_source_hash(source: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Provenance recorded for translations cached before validity tracking
/// existed. Such rows are accepted when their source hash matches, but they
/// never claim to come from a specific model or prompt revision.
pub const LEGACY_TRANSLATION_PROVENANCE: &str = "legacy";

/// Max characters per translation segment.
pub const MAX_CHARS_PER_SEGMENT: usize = 3000;
/// Max retry attempts for LLM calls.
pub const MAX_RETRIES: usize = 2;
/// Max articles per batch classification call.
pub const CLASSIFY_BATCH_SIZE: usize = 20;
/// Number of picks the recommendation prompt asks for.
pub const RECOMMEND_PICK_COUNT: usize = 5;

pub mod activity;
pub mod service;

#[cfg(test)]
mod config_tests {
    use super::*;

    fn config(base_url: &str, max_tokens: Option<u32>, max_chars: Option<usize>) -> AiConfig {
        AiConfig {
            api_key: "k".into(),
            base_url: base_url.into(),
            model: "m".into(),
            max_tokens,
            temperature: None,
            max_chars_per_segment: max_chars,
        }
    }

    /// Users paste the full endpoint from the provider's docs. Appending the
    /// path again sent every request to a 404 that looked like a bad key.
    #[test]
    fn chat_endpoint_accepts_a_full_endpoint_as_the_base_url() {
        assert_eq!(
            config("https://api.minimax.cn/v1", None, None).chat_endpoint(),
            "https://api.minimax.cn/v1/chat/completions"
        );
        for base in [
            "https://api.minimax.cn/v1/chat/completions",
            "https://api.minimax.cn/v1/chat/completions/",
            "  https://api.minimax.cn/v1/chat/completions  ",
        ] {
            assert_eq!(
                config(base, None, None).chat_endpoint(),
                "https://api.minimax.cn/v1/chat/completions",
                "base URL: {base}"
            );
        }
    }

    #[test]
    fn normalize_stores_the_canonical_base_url_and_bare_key() {
        let mut c = config("https://api.minimax.cn/v1/chat/completions/", None, None);
        c.api_key = "Bearer sk-abc".into();
        c.normalize_base_url();
        assert_eq!(c.base_url, "https://api.minimax.cn/v1");
        assert_eq!(c.api_key, "sk-abc");
    }

    #[test]
    fn segment_size_follows_the_output_budget() {
        // Default budget: room for the 3000-character default segment.
        let default = config("https://x/v1", None, None);
        assert_eq!(default.max_tokens_or_default(), DEFAULT_MAX_TOKENS);
        assert_eq!(default.segment_chars_for_budget(), MAX_CHARS_PER_SEGMENT);

        // The budget that truncated real articles shrinks the segment instead.
        let small = config("https://x/v1", Some(4_000), Some(3_000));
        assert_eq!(small.segment_chars_for_budget(), 1_250);

        // A tiny budget never produces unusably small segments.
        assert_eq!(chars_for_tokens(100), MIN_CHARS_PER_SEGMENT);

        // An explicit smaller char limit still wins.
        let capped = config("https://x/v1", Some(16_000), Some(800));
        assert_eq!(capped.segment_chars_for_budget(), 800);
    }
}

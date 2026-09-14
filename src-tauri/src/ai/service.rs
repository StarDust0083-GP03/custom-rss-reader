use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::RwLock;
use tokio::time::{sleep, Instant};

use crate::ai::activity::current_ai_task;
use crate::ai::*;
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

/// Largest LLM response body accepted (bytes).
const MAX_LLM_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Check that a model answer really is one bilingual pair.
///
/// The downstream pipeline (chunk cleanup, streaming render, cache) assumes
/// each response contains a `.paragraph-original` and a `.paragraph-translated`
/// node. Without this check, a refusal or a plain echo was stored as the
/// article's "translation" — the UI then showed bilingual content with no
/// translation in it, and the cache kept serving it.
fn validate_bilingual_block(raw: &str) -> Result<()> {
    if raw.trim().is_empty() {
        return Err(AppError::OperationFailed(
            "LLM returned an empty translation".into(),
        ));
    }
    if !raw.contains("paragraph-original") || !raw.contains("paragraph-translated") {
        let preview: String = raw.chars().take(200).collect();
        return Err(AppError::OperationFailed(format!(
            "LLM returned text that is not a bilingual pair (missing paragraph-original/paragraph-translated): {preview}"
        )));
    }
    // A translated side that is empty means the model produced the wrapper but
    // no text; treat it as a failed block rather than caching a blank column.
    if let Some(translated) = raw.split_once("paragraph-translated") {
        let body = translated
            .1
            .trim_start_matches(|c: char| c == '>' || c == '\"' || c.is_whitespace());
        if body.trim_start().starts_with("</div>") {
            return Err(AppError::OperationFailed(
                "LLM returned an empty translated side".into(),
            ));
        }
    }
    Ok(())
}

/// Marker prefix handed to the model, e.g. `###P3###`.
///
/// Batching several paragraphs into one request is what keeps the call count
/// (and the reasoning overhead per call) low, but it also means the model has
/// to reproduce paragraph structure on its own — and it does not always do
/// that: a real 13-paragraph article came back as a single pair, which renders
/// as "all the original, then all the translation".
///
/// Numbering the paragraphs gives the model a stable anchor to repeat and gives
/// us something exact to verify. Measured against MiniMax-M2.7: the same
/// article that collapsed without markers returned 14 markers, 14 pairs and 14
/// verbatim originals with them.
const PARAGRAPH_MARKER_PREFIX: &str = "###P";
const PARAGRAPH_MARKER_SUFFIX: &str = "###";

/// Prefix each paragraph of a block with its marker line.
fn mark_paragraphs(block: &str) -> String {
    let mut out = String::with_capacity(block.len() + 32);
    let mut index = 0usize;
    let mut current = String::new();

    let flush = |out: &mut String, index: &mut usize, current: &mut String| {
        if current.trim().is_empty() {
            current.clear();
            return;
        }
        *index += 1;
        out.push_str(&format!("{PARAGRAPH_MARKER_PREFIX}{index}{PARAGRAPH_MARKER_SUFFIX}\n"));
        out.push_str(current);
        current.clear();
    };

    for line in block.split_inclusive('\n') {
        if line.trim().is_empty() {
            flush(&mut out, &mut index, &mut current);
            out.push_str(line);
        } else {
            current.push_str(line);
        }
    }
    flush(&mut out, &mut index, &mut current);

    if index == 0 {
        // Nothing paragraph-shaped to mark; send the block untouched.
        return block.to_string();
    }
    out
}

/// Byte index just past a marker starting at `start`, if one starts there.
fn marker_end_at(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if !text[start..].starts_with(PARAGRAPH_MARKER_PREFIX) {
        return None;
    }
    let mut end = start + PARAGRAPH_MARKER_PREFIX.len();
    let digits_start = end;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end == digits_start || !text[end..].starts_with(PARAGRAPH_MARKER_SUFFIX) {
        return None;
    }
    Some(end + PARAGRAPH_MARKER_SUFFIX.len())
}

/// Remove marker tokens from a model answer.
///
/// The markers exist to keep the structure honest; they must never reach the
/// reader. A marker that occupied its own line takes the line's newline with
/// it, so no stray blank line is left behind (and blank lines inside the
/// preserved markdown are not touched).
fn strip_paragraph_markers(answer: &str) -> String {
    let mut out = String::with_capacity(answer.len());
    let mut i = 0usize;
    while i < answer.len() {
        if let Some(after) = marker_end_at(answer, i) {
            i = after;
            if answer[i..].starts_with('\n') {
                i += 1;
            }
            continue;
        }
        let ch = answer[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Marker numbers present in a model answer, in order.
fn paragraph_markers_in(answer: &str) -> Vec<usize> {
    let mut found = Vec::new();
    let mut i = 0usize;
    while i < answer.len() {
        if let Some(after) = marker_end_at(answer, i) {
            let digits = &answer[i + PARAGRAPH_MARKER_PREFIX.len()..after - PARAGRAPH_MARKER_SUFFIX.len()];
            if let Ok(number) = digits.parse::<usize>() {
                found.push(number);
            }
            i = after;
            continue;
        }
        let ch = answer[i..].chars().next().unwrap_or(' ');
        i += ch.len_utf8();
    }
    found
}

/// Paragraphs in a source block: non-empty runs separated by a blank line.
fn count_paragraphs(text: &str) -> usize {
    let mut count = 0;
    let mut in_paragraph = false;
    for line in text.lines() {
        if line.trim().is_empty() {
            in_paragraph = false;
        } else if !in_paragraph {
            in_paragraph = true;
            count += 1;
        }
    }
    count
}

/// Bilingual pairs in a model answer.
pub(crate) fn count_translation_pairs(answer: &str) -> usize {
    answer.matches("paragraph-original").count()
}

/// Did the model collapse a multi-paragraph block into a single pair?
///
/// The prompt asks for one pair per paragraph so the reader sees each
/// paragraph next to its translation. A model that returns the whole block as
/// one pair produces "the entire article, then the entire translation", which
/// is a different (and much worse) reading experience — and it used to be
/// cached as-is.
fn collapses_paragraphs(answer: &str, block: &str) -> bool {
    let paragraphs = count_paragraphs(block);
    if paragraphs < 2 {
        return false;
    }
    let pairs = count_translation_pairs(answer);
    let markers = paragraph_markers_in(answer);

    // Markers make the expected count exact: the model was given one per
    // paragraph and repeated some of them. Fewer pairs than paragraphs means
    // part of the block was merged away — or silently dropped, which the
    // ratio heuristic below would happily accept (8 pairs for 14 paragraphs
    // still passes `pairs * 2 >= paragraphs`).
    if !markers.is_empty() && pairs < paragraphs {
        println!(
            "[translate] {paragraphs} paragraphs but only {pairs} pair(s) and {} marker(s)",
            markers.len()
        );
        return true;
    }

    // Without markers (a model that ignored them) fall back to the ratio: one
    // pair for a whole multi-paragraph block is the case that produced "all the
    // original, then all the translation", and fewer than half the expected
    // pairs is the same problem in a milder form. A couple of merged short
    // lines are tolerated, because splitting costs another model call.
    pairs == 1 || pairs * 2 < paragraphs
}

/// Read a response body, refusing anything past `limit`.
///
/// `reqwest::Response::json()` buffers without a bound; the reader must not
/// let one bad endpoint decide how much memory the app uses.
async fn read_bounded(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    if let Some(length) = response.content_length() {
        if length as usize > limit {
            return Err(AppError::OperationFailed(format!(
                "response body of {} bytes exceeds the {} byte limit",
                length, limit
            )));
        }
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| AppError::Network(format!("failed to read response body: {}", e)))?
    {
        if body.len() + chunk.len() > limit {
            return Err(AppError::OperationFailed(format!(
                "response body exceeds the {} byte limit",
                limit
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Default)]
struct GateInner {
    /// Ticket currently holding the slot (`None` = free).
    serving: Option<u64>,
    /// Next ticket id to hand out.
    next_ticket: u64,
    /// Queued interactive callers, in arrival order.
    interactive: VecDeque<u64>,
    /// Queued background callers, in arrival order.
    background: VecDeque<u64>,
    last_start: Option<Instant>,
}

impl GateInner {
    fn is_front(&self, ticket: u64) -> bool {
        self.interactive
            .front()
            .or_else(|| self.background.front())
            .is_some_and(|front| *front == ticket)
    }

    fn remove(&mut self, ticket: u64) {
        self.interactive.retain(|id| *id != ticket);
        self.background.retain(|id| *id != ticket);
    }

    /// Time left before the next request may start.
    fn pacing_wait(&self) -> Duration {
        self.last_start
            .map(|started| min_interval().saturating_sub(started.elapsed()))
            .unwrap_or(Duration::ZERO)
    }
}

struct LlmGate {
    /// Plain mutex: every critical section is a few queue operations with no
    /// `.await` inside, which lets `Drop` release the slot synchronously.
    inner: std::sync::Mutex<GateInner>,
    notify: tokio::sync::Notify,
}

fn llm_gate() -> &'static LlmGate {
    static GATE: OnceLock<LlmGate> = OnceLock::new();
    GATE.get_or_init(|| LlmGate {
        inner: std::sync::Mutex::new(GateInner::default()),
        // At most LLM_MAX_CONCURRENCY + 1 waiters are ever notified by name;
        // `notify_waiters` wakes all currently-registered ones, so any number
        // of queued callers is fine.
        notify: tokio::sync::Notify::new(),
    })
}

fn min_interval() -> Duration {
    // Tests would otherwise spend seconds pacing requests they don't send.
    if cfg!(test) {
        return Duration::from_millis(5);
    }
    Duration::from_millis(LLM_MIN_INTERVAL_MS)
}

/// Proof that the caller holds the model slot. Dropping it releases the slot
/// and wakes the next queued caller — including when the task is cancelled or
/// panics mid-request, because `Drop` cannot be skipped.
struct LlmPermit {
    gate: Option<&'static LlmGate>,
}

impl Drop for LlmPermit {
    fn drop(&mut self) {
        let Some(gate) = self.gate.take() else {
            return;
        };
        if let Ok(mut inner) = gate.inner.lock() {
            inner.serving = None;
            inner.last_start = Some(Instant::now());
        }
        gate.notify.notify_waiters();
    }
}

/// A queued place in line. If the caller is cancelled before acquiring, its
/// ticket is removed and the next waiter is woken — the previous
/// implementation left a `oneshot::Sender` in the queue whose receiver was
/// gone, and `tx.send()` failing left the gate permanently busy.
struct Ticket {
    gate: &'static LlmGate,
    id: u64,
    /// Cleared once the ticket becomes the permit, so `Drop` does not remove
    /// a ticket that is legitimately being served.
    queued: bool,
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if !self.queued {
            return;
        }
        if let Ok(mut inner) = self.gate.inner.lock() {
            inner.remove(self.id);
        }
        self.gate.notify.notify_waiters();
    }
}

/// Register in the queue behind everyone already waiting in this priority
/// class (FIFO within a priority, interactive ahead of background).
///
/// Takes the already-locked state so no `MutexGuard` ever lives across an
/// `.await` in the caller.
fn enqueue_locked(gate: &'static LlmGate, inner: &mut GateInner, priority: bool) -> Ticket {
    let id = inner.next_ticket;
    inner.next_ticket += 1;
    if priority {
        inner.interactive.push_back(id);
    } else {
        inner.background.push_back(id);
    }
    Ticket {
        gate,
        id,
        queued: true,
    }
}

/// Whether the slot is free and nobody is waiting for it.
enum Slot {
    Acquired,
    Queued(Ticket),
}

/// Acquire the serialized slot. Interactive callers are served before queued
/// background classification, and same-class callers are served in arrival
/// order.
async fn llm_acquire(priority: bool) -> LlmPermit {
    let gate = llm_gate();
    let task = current_ai_task();
    let priority = task
        .as_ref()
        .map(|task| task.priority())
        .unwrap_or(priority);

    // First request (or nobody waiting): take the slot without queueing.
    let slot = {
        let mut inner = gate.inner.lock().expect("LLM gate lock poisoned");
        if inner.serving.is_none() && inner.interactive.is_empty() && inner.background.is_empty() {
            inner.serving = Some(inner.next_ticket);
            inner.next_ticket += 1;
            Slot::Acquired
        } else {
            Slot::Queued(enqueue_locked(gate, &mut inner, priority))
        }
    };

    let mut ticket = match slot {
        Slot::Acquired => {
            if let Some(task) = &task {
                task.running().await;
            }
            return LlmPermit { gate: Some(gate) };
        }
        Slot::Queued(ticket) => ticket,
    };

    if let Some(task) = &task {
        task.waiting().await;
    }

    loop {
        // Register interest BEFORE re-checking, so a release between the check
        // and the wait cannot be missed.
        let notified = gate.notify.notified();
        let waited: Option<Duration> = {
            let mut inner = gate.inner.lock().expect("LLM gate lock poisoned");
            if inner.serving.is_none() && inner.is_front(ticket.id) {
                let wait = inner.pacing_wait();
                inner.remove(ticket.id);
                inner.serving = Some(ticket.id);
                ticket.queued = false;
                Some(wait)
            } else {
                None
            }
        };

        if let Some(wait) = waited {
            if !wait.is_zero() {
                sleep(wait).await;
                if let Ok(mut inner) = gate.inner.lock() {
                    inner.last_start = Some(Instant::now());
                }
            }
            if let Some(task) = &task {
                task.running().await;
            }
            return LlmPermit { gate: Some(gate) };
        }

        notified.await;
    }
}

// ---------------------------------------------------------------------------
// Output budgets and block splitting
// ---------------------------------------------------------------------------

/// Classification answer: a small JSON object, plus the reasoning text a
/// reasoning model emits first (which is why 200 tokens was not enough).
const CLASSIFY_MAX_TOKENS: u32 = 1_500;

/// Batch classification answer: one JSON object per article (up to 20).
const CLASSIFY_BATCH_MAX_TOKENS: u32 = 8_000;

/// Recommendation answer: a short list of picks and reasons.
const RECOMMEND_MAX_TOKENS: u32 = 3_000;

/// Connection probe: only a few tokens of actual answer are needed, but a
/// reasoning model may spend a few dozen on its chain of thought first.
const PROBE_MAX_TOKENS: u32 = 256;

/// How many times a block may be halved before the truncation is reported.
const MAX_SPLIT_DEPTH: usize = 2;

/// Shortest block worth splitting; below this the pieces lose too much context.
const MIN_SPLITTABLE_CHARS: usize = 600;

fn is_truncation_error(error: &AppError) -> bool {
    matches!(error, AppError::OperationFailed(message) if message.contains("truncated (finish_reason=length)"))
}

/// Split a block in half at a paragraph boundary, or `None` when it is too
/// short to split usefully.
///
/// Indices are BYTES throughout: the block is cut with `str::split_at`, which
/// requires a char boundary. Computing the boundary in characters (as this
/// used to) cut CJK text at the wrong place — roughly one third of the way in —
/// and could panic outright when the byte index landed inside a character.
fn split_for_translation(block: &str) -> Option<(String, String)> {
    let char_count = block.chars().count();
    if char_count < MIN_SPLITTABLE_CHARS {
        return None;
    }

    let total_bytes = block.len();
    let mut boundaries: Vec<usize> = Vec::new();
    let mut offset = 0usize;
    for line in block.split_inclusive('\n') {
        offset += line.len();
        if line.trim().is_empty() {
            boundaries.push(offset);
        }
    }

    let target = total_bytes / 2;
    let split_at = boundaries
        .iter()
        .copied()
        .filter(|position| *position > total_bytes / 5 && *position < total_bytes * 4 / 5)
        .min_by_key(|position| position.abs_diff(target))
        .or_else(|| {
            // No usable paragraph break: fall back to the last char boundary
            // before the middle, then to a word boundary there.
            let safe = block
                .char_indices()
                .map(|(index, _)| index)
                .take_while(|index| *index < target)
                .last()
                .unwrap_or(0);
            block[..safe].rfind(' ')
        })?;
    if split_at == 0 || split_at >= total_bytes {
        return None;
    }
    let (first, second) = block.split_at(split_at);
    if first.trim().is_empty() || second.trim().is_empty() {
        return None;
    }
    Some((first.to_string(), second.to_string()))
}

// ---------------------------------------------------------------------------
// Chat API types (private to this module)
// ---------------------------------------------------------------------------

/// What to do when the model hits its output limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Truncation {
    /// The content is the result: a length-limited answer is unusable.
    Reject,
    /// Only reachability matters (connection test).
    Allow,
}

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
    /// Why the model stopped. `"length"` means the output was cut off
    /// mid-answer, which must never be cached as a finished translation.
    #[serde(default)]
    finish_reason: Option<String>,
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
    /// error), so one bad row can't fail the whole batch. Generated names are
    /// matched to the vocabulary locally after this call.
    async fn classify_batch(
        &self,
        entries: &[crate::ai::BatchClassifyEntry],
    ) -> Result<Vec<ClassificationResponse>>;

    /// Recommend the most worthwhile reads from a candidate list (one LLM
    /// call). Returns `(item_id, reason)` picks in the model's ranking
    /// order; candidates the model mis-indexed are skipped, never fatal.
    async fn recommend_reads(
        &self,
        candidates: &[crate::ai::RecommendCandidate],
    ) -> Result<Vec<crate::ai::Recommendation>>;

    /// Write a one-line definition for each given tag name (ONE LLM call).
    ///
    /// Names the model skipped or invented are dropped rather than guessed, so
    /// the caller only ever stores definitions for tags it asked about.
    async fn explain_tags(&self, names: &[String]) -> Result<Vec<crate::ai::TagExplanation>>;

    /// Place each word in one of the given topics (ONE LLM call).
    ///
    /// The catalog is passed in and frozen for the call: the model chooses an
    /// id, it never invents a topic. Proposals that name a topic outside the
    /// catalog, skip a word or contradict their own state are dropped by the
    /// parser, so the caller can store the result or show it for review
    /// without re-checking it.
    async fn suggest_topics(
        &self,
        catalog: &[TopicChoice],
        words: &[TopicWordInput],
    ) -> Result<Vec<crate::ai::TopicSuggestion>>;

    /// Test the LLM API connection.
    async fn test_connection(&self) -> Result<String>;

    /// Expose the effective `max_chars_per_segment` so callers (e.g. the
    /// streaming pipeline) can chunk content consistently with what the
    /// service would do internally.
    fn config_max_chars(&self) -> usize;

    /// The configured model name. Cached translations record it so switching
    /// models invalidates results produced by the previous one.
    fn config_model(&self) -> String;
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
    /// Translate one block, recovering from the two ways a model can hand back
    /// an unusable answer.
    ///
    /// 1. **Truncated** — the output budget ran out (reasoning models spend
    ///    part of it on chain-of-thought before the answer starts).
    /// 2. **Collapsed** — several source paragraphs came back as a single
    ///    pair, which renders as one wall of original text followed by one wall
    ///    of translation instead of paragraph-by-paragraph.
    ///
    /// Both are recovered the same way: halve the block at a paragraph
    /// boundary and ask again, recursively but bounded. A single long
    /// paragraph must not be able to fail an article, and a coarse answer is
    /// still better than a hard failure once the depth budget is spent.
    #[allow(clippy::too_many_arguments)]
    async fn translate_block_sized(
        &self,
        block: &str,
        system_prompt: String,
        user_prompt: String,
        model: String,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
        depth: usize,
    ) -> Result<String> {
        self.translate_with_recovery(block, depth, |part: String| {
            // Markers are added here, not upstream, so every entry point
            // (streaming pipeline, whole-article translate, retries) sends the
            // same contract.
            let (system, user) = if part == block {
                (system_prompt.clone(), user_prompt.clone())
            } else {
                self.build_translation_prompts(&part, "auto", "zh-CN", false)
            };
            // Marked exactly once: the original user prompt is unmarked, and a
            // re-prompt for a split half is built fresh above.
            let user = mark_paragraphs(&user);
            let request = ChatRequest {
                model: model.clone(),
                messages: vec![
                    ChatMessage {
                        role: "system".into(),
                        content: system,
                    },
                    ChatMessage {
                        role: "user".into(),
                        content: user,
                    },
                ],
                max_tokens,
                temperature,
            };
            // Transient provider errors (429, 5xx, timeouts) are retried here;
            // the recovery loop above only handles truncation and collapse.
            async move { self.with_retry(|| self.send_request(&request)).await }
        })
        .await
    }

    /// Ask for a block and recover from truncation or paragraph collapse.
    ///
    /// `call` is the request function, injected so the recovery behaviour can
    /// be tested without a network.
    async fn translate_with_recovery<F, Fut>(
        &self,
        block: &str,
        depth: usize,
        call: F,
    ) -> Result<String>
    where
        F: Fn(String) -> Fut + Clone,
        Fut: std::future::Future<Output = Result<String>>,
    {
        let answer = call(block.to_string()).await;
        let failure = match &answer {
            Err(error) if is_truncation_error(error) => Some("ran out of output budget"),
            Ok(answer) if collapses_paragraphs(answer, block) => {
                Some("collapsed several paragraphs into one pair")
            }
            _ => None,
        };

        let Some(reason) = failure else {
            let answer = answer?;
            validate_bilingual_block(&answer)?;
            return Ok(strip_paragraph_markers(&answer));
        };

        if depth >= MAX_SPLIT_DEPTH {
            // Out of recovery attempts: keep a correct-but-coarse answer rather
            // than failing the whole article.
            if let Ok(answer) = answer {
                println!(
                    "[translate] block of {} chars still {reason} at depth {depth}; keeping it",
                    block.chars().count()
                );
                validate_bilingual_block(&answer)?;
                return Ok(strip_paragraph_markers(&answer));
            }
            return answer;
        }

        let Some((first, second)) = split_for_translation(block) else {
            return answer;
        };
        println!(
            "[translate] block of {} chars {reason}; splitting into {} + {}",
            block.chars().count(),
            first.chars().count(),
            second.chars().count()
        );
        let second_call = call.clone();
        let a = Box::pin(self.translate_with_recovery(&first, depth + 1, call)).await?;
        let b = Box::pin(self.translate_with_recovery(&second, depth + 1, second_call)).await?;
        Ok(format!("{a}\n{b}"))
    }


    pub fn new(config: AiConfig) -> Result<Self> {
        config.is_valid()?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| AppError::Internal(format!("Failed to build HTTP client: {}", e)))?;
        Ok(Self { config, client })
    }

    fn max_chars(&self) -> usize {
        self.config
            .max_chars_per_segment
            .unwrap_or(MAX_CHARS_PER_SEGMENT)
    }

    /// Send an interactive chat completion request.
    async fn send_request(&self, request: &ChatRequest) -> Result<String> {
        self.send_request_with_priority(request, true, Truncation::Reject).await
    }

    /// Send a background request, behind interactive work in the queue.
    async fn send_request_background(&self, request: &ChatRequest) -> Result<String> {
        self.send_request_with_priority(request, false, Truncation::Reject).await
    }

    /// Reachability probe: any answer proves the endpoint, key and model work.
    ///
    /// A reasoning model spends a few tokens on its chain of thought before the
    /// requested word, so the reply can hit the token limit before it says
    /// anything. Failing the probe for that would report a working
    /// configuration as broken — which is exactly what happened with
    /// MiniMax-M2 and a 10-token budget.
    async fn send_request_probe(&self, request: &ChatRequest) -> Result<String> {
        self.send_request_with_priority(request, true, Truncation::Allow).await
    }

    async fn send_request_with_priority(
        &self,
        request: &ChatRequest,
        priority: bool,
        truncation: Truncation,
    ) -> Result<String> {
        let _permit = llm_acquire(priority).await;

        let url = self.config.chat_endpoint();

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

        // Bound the body: a misconfigured base URL can answer with a
        // multi-megabyte HTML page, and `json()` would buffer all of it.
        let body = read_bounded(response, MAX_LLM_RESPONSE_BYTES)
            .await
            .map_err(|e| AppError::Parse(format!("Failed to read LLM response: {}", e)))?;
        let chat_response: ChatResponse = serde_json::from_slice(&body)
            .map_err(|e| AppError::Parse(format!("Failed to parse LLM response: {}", e)))?;

        let choice = chat_response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| AppError::Parse("LLM returned no choices".into()))?;

        // A truncated answer is a FAILED request, not a short translation.
        // Returning it as success cached half a paragraph as the article's
        // finished translation.
        if choice.finish_reason.as_deref() == Some("length") && truncation == Truncation::Reject {
            let budget = request.max_tokens.unwrap_or_default();
            let source_chars: usize = request
                .messages
                .iter()
                .map(|message| message.content.chars().count())
                .sum();
            return Err(AppError::OperationFailed(format!(
                "LLM response was truncated (finish_reason=length) for a {source_chars}-character                  request with max_tokens={budget}. Reasoning models such as MiniMax-M2 write their                  chain of thought into the same content field and no parameter disables it, so part                  of the budget is spent before the answer starts. Raise max_tokens or lower                  max_chars_per_segment."
            )));
        }

        Ok(strip_think_tags(&choice.message.content))
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
        let max_tokens = Some(self.config.max_tokens_or_default());
        let temperature = self.config.temperature;

        self.translate_block_sized(block, system_prompt, user_prompt, model, max_tokens, temperature, 0)
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
            "Title: {}\nDescription: {}\nContent: {}",
            request.title,
            request.description.as_deref().unwrap_or(""),
            content_snippet,
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
            max_tokens: Some(CLASSIFY_MAX_TOKENS),
            temperature: Some(0.1),
        };

        let response = self.send_request(&req).await?;
        parse_classification_json(&response)
    }

    async fn classify_batch(
        &self,
        entries: &[crate::ai::BatchClassifyEntry],
    ) -> Result<Vec<ClassificationResponse>> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }

        let system_prompt = "You are an article classification assistant. You will receive a numbered list of article titles. For EACH article, classify it by title alone and return a JSON array where every element is:\n\
            {\"index\": <the article number>, \"tags\": [1-3 durable subject tags], \"category\": \"<one of: technology, science, politics, entertainment, sports, business, health, education, other>\"}\n\
            Tags must be durable subjects in lowercase English snake_case. Avoid generic labels such as news, article, or important. The application matches generated names to its vocabulary locally.\n\
            Respond with ONLY the JSON array, one element per input article, no other text."
            .to_string();

        let mut user_message = String::new();
        for e in entries {
            user_message.push_str(&format!("[{}] {}\n", e.index, e.title));
        }

        let req = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".into(),
                    content: system_prompt,
                },
                ChatMessage {
                    role: "user".into(),
                    content: user_message,
                },
            ],
            max_tokens: Some(CLASSIFY_BATCH_MAX_TOKENS),
            temperature: Some(0.1),
        };

        let response = self.send_request_background(&req).await?;
        Ok(parse_classification_batch_json(&response, entries.len()))
    }

    /// One call: define every tag in the batch so the local encoder has real
    /// semantics to work with. Interactive priority — the user is waiting on
    /// the tag workspace when this runs.
    async fn explain_tags(&self, names: &[String]) -> Result<Vec<crate::ai::TagExplanation>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }

        let system_prompt = "You maintain the controlled topic vocabulary of a personal RSS reader. \
            For EACH tag below, write one short definition (max 20 words) of what the tag means as a subject, \
            written in the same language as the tag itself. Append at most 3 common synonyms or translations \
            in parentheses. Return a JSON array where every element is \
            {\"name\": \"<the tag exactly as given>\", \"explanation\": \"<the definition>\"}. \
            Respond with ONLY the JSON array, one element per tag, no other text.";

        let mut user_message = String::new();
        for name in names {
            user_message.push_str(&format!("- {name}\n"));
        }

        let req = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".into(),
                    content: system_prompt.to_string(),
                },
                ChatMessage {
                    role: "user".into(),
                    content: user_message,
                },
            ],
            max_tokens: Some(crate::ai::EXPLAIN_MAX_TOKENS),
            temperature: Some(0.2),
        };

        let response = self.send_request(&req).await?;
        Ok(parse_tag_explanations_json(&response, names))
    }

    /// One call: place a batch of words into the frozen topic catalog.
    ///
    /// The catalog travels with every call (it is ~40 short lines), so the
    /// model cannot drift into inventing topics across a long run, and a
    /// renamed topic takes effect on the next batch rather than at the end.
    async fn suggest_topics(
        &self,
        catalog: &[crate::ai::TopicChoice],
        words: &[crate::ai::TopicWordInput],
    ) -> Result<Vec<crate::ai::TopicSuggestion>> {
        if words.is_empty() || catalog.is_empty() {
            return Ok(Vec::new());
        }

        let system_prompt = "You file tags into a FIXED topic catalog for a personal RSS reader. \
            For EACH tag, choose the single best-fitting topic from the catalog. \
            Rules: use the topic id exactly as listed; never invent a topic; never rename one. \
            If the tag is a content format, quality or genre word (opinion, advice, tutorial, \
            newsletter, review, weekly) that says nothing about the subject, answer \
            {\"state\": \"context_only\"}. If you genuinely cannot tell, answer {\"state\": \"review\"}. \
            A tag may well fit two topics; pick the one a reader browsing the library would look under. \
            Respond with ONLY a JSON array, one element per tag, no other text:\n\
            {\"name\": \"<the tag exactly as given>\", \"category_id\": <topic id or null>, \
            \"state\": \"assigned\" | \"context_only\" | \"review\", \"reason\": \"<max 12 words>\"}".to_string();

        let mut user_message = String::from("Topics:\n");
        for choice in catalog {
            user_message.push_str(&format!(
                "[{}] {} — {}\n",
                choice.id, choice.label, choice.definition
            ));
        }
        user_message.push_str("\nTags to file:\n");
        for word in words {
            let explanation = word.explanation.trim();
            if explanation.is_empty() {
                user_message.push_str(&format!("- {} ({} articles)\n", word.name, word.usage_count));
            } else {
                user_message.push_str(&format!(
                    "- {} ({} articles): {}\n",
                    word.name, word.usage_count, explanation
                ));
            }
        }

        let req = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".into(),
                    content: system_prompt,
                },
                ChatMessage {
                    role: "user".into(),
                    content: user_message,
                },
            ],
            max_tokens: Some(crate::ai::TOPIC_SUGGEST_MAX_TOKENS),
            temperature: Some(0.1),
        };

        let names: Vec<String> = words.iter().map(|word| word.name.clone()).collect();
        let allowed_ids: Vec<i64> = catalog.iter().map(|choice| choice.id).collect();
        let response = self.send_request(&req).await?;
        Ok(parse_topic_suggestions_json(&response, &names, &allowed_ids))
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
                ChatMessage {
                    role: "system".into(),
                    content: system_prompt,
                },
                ChatMessage {
                    role: "user".into(),
                    content: user_message,
                },
            ],
            max_tokens: Some(RECOMMEND_MAX_TOKENS),
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
            max_tokens: Some(PROBE_MAX_TOKENS),
            temperature: Some(0.0),
        };

        self.send_request_probe(&req).await
    }

    /// Characters per segment that the configured output budget can actually
    /// carry back. A bilingual answer repeats the source and reasoning models
    /// also emit chain-of-thought text, so chunking by the raw setting is what
    /// produced `finish_reason=length` on real articles.
    fn config_max_chars(&self) -> usize {
        self.config.segment_chars_for_budget()
    }

    fn config_model(&self) -> String {
        self.config.model.clone()
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
    let Some(first) = lines.next() else {
        return false;
    };
    if lines.next().is_some() {
        return false; // multi-line → a paragraph, not a bare header
    }
    if is_atx_header(first) {
        return true;
    }
    let lower = first.trim_start().to_ascii_lowercase();
    (1..=6)
        .any(|n| lower.starts_with(&format!("<h{}>", n)) || lower.starts_with(&format!("<h{} ", n)))
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
        "p",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "li",
        "blockquote",
        "pre",
        "div",
        "section",
        "article",
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
            if !(next_ch == b'>'
                || next_ch == b' '
                || next_ch == b'\t'
                || next_ch == b'\n'
                || next_ch == b'/')
            {
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

    deduplicated
        .into_iter()
        .map(|(_, _, block)| block)
        .collect()
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
        "</ul>",
        "</ol>",
        "</table>",
        "</div>",
        "</section>",
        "</blockquote>",
        "</pre>",
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
        let after_open = trimmed.find('\n').map(|i| i + 1).unwrap_or(3);
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

    let value: serde_json::Value = serde_json::from_str(json_slice)
        .map_err(|e| AppError::Parse(format!("Failed to parse classification JSON: {}", e)))?;

    let tags = value["tags"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    Ok(ClassificationResponse { tags })
}

/// Parse the JSON-array response of a topic-suggestion call.
///
/// Three things are checked here rather than trusted: the name must be one that
/// was asked about, `state` must be a value the database accepts, and
/// `category_id` must come from the catalog that was sent AND agree with the
/// state. A proposal failing any of these is dropped — a wrong row silently
/// written into the navigation layer is worse than a word that needs a second
/// pass.
pub fn parse_topic_suggestions_json(
    response: &str,
    requested: &[String],
    allowed_ids: &[i64],
) -> Vec<crate::ai::TopicSuggestion> {
    let trimmed = strip_code_fence(response);
    let start = trimmed.find('[');
    let end = trimmed.rfind(']');
    let slice = match (start, end) {
        (Some(s), Some(e)) if e > s => &trimmed[s..=e],
        _ => trimmed,
    };

    let Ok(value) = serde_json::from_str::<serde_json::Value>(slice) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for item in value.as_array().map(Vec::as_slice).unwrap_or_default() {
        let Some(name) = item["name"].as_str() else {
            continue;
        };
        let Some(canonical) = requested
            .iter()
            .find(|candidate| candidate.eq_ignore_ascii_case(name.trim()))
        else {
            continue;
        };
        let state = item["state"].as_str().unwrap_or("review").trim();
        let reason = item["reason"].as_str().unwrap_or_default().trim();
        let candidate = crate::ai::TopicSuggestion {
            name: canonical.clone(),
            category_id: item["category_id"].as_i64(),
            state: state.to_string(),
            reason: reason.chars().take(160).collect(),
        };
        if let Some(valid) = validate_topic_suggestion(candidate, allowed_ids) {
            out.push(valid);
        }
    }
    out
}

/// Apply the placement rules to one candidate, wherever it came from.
///
/// Shared by the response parser and the cache reader, so a proposal stored by
/// an older prompt cannot slip in through a path that checks less: the state
/// must be one the table accepts, an `assigned` verdict must name a topic from
/// the catalog, and any other verdict must not name one at all.
pub fn validate_topic_suggestion(
    candidate: crate::ai::TopicSuggestion,
    allowed_ids: &[i64],
) -> Option<crate::ai::TopicSuggestion> {
    if !matches!(candidate.state.as_str(), "assigned" | "context_only" | "review") {
        return None;
    }
    let category_id = match (candidate.state.as_str(), candidate.category_id) {
        ("assigned", Some(id)) if allowed_ids.contains(&id) => Some(id),
        // An assigned verdict without a usable topic is not a verdict.
        ("assigned", _) => return None,
        (_, Some(_)) => return None,
        (_, None) => None,
    };
    Some(crate::ai::TopicSuggestion {
        name: candidate.name,
        category_id,
        state: candidate.state,
        reason: candidate.reason,
    })
}

/// Parse the JSON-array response of an explanation call.
///
/// Matched back to the requested names case-insensitively, so the stored
/// spelling always comes from the database rather than from the model. Names
/// the model invented are dropped; a malformed response yields an empty list
/// instead of failing the whole dictionary build.
pub fn parse_tag_explanations_json(
    response: &str,
    requested: &[String],
) -> Vec<crate::ai::TagExplanation> {
    let trimmed = strip_code_fence(response);
    let start = trimmed.find('[');
    let end = trimmed.rfind(']');
    let slice = match (start, end) {
        (Some(s), Some(e)) if e > s => &trimmed[s..=e],
        _ => trimmed,
    };

    let Ok(value) = serde_json::from_str::<serde_json::Value>(slice) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for item in value.as_array().map(Vec::as_slice).unwrap_or_default() {
        let (Some(name), Some(explanation)) =
            (item["name"].as_str(), item["explanation"].as_str())
        else {
            continue;
        };
        let explanation = explanation.trim();
        if explanation.is_empty() {
            continue;
        }
        let Some(canonical) = requested
            .iter()
            .find(|candidate| candidate.eq_ignore_ascii_case(name.trim()))
        else {
            continue;
        };
        out.push(crate::ai::TagExplanation {
            name: canonical.clone(),
            explanation: explanation.chars().take(crate::ai::MAX_EXPLANATION_CHARS).collect(),
        });
    }
    out
}

/// Strip a ``` code fence, which models emit despite being told not to.
fn strip_code_fence(response: &str) -> &str {
    let trimmed = response.trim();
    if !trimmed.starts_with("```") {
        return trimmed;
    }
    let after_open = trimmed.find('\n').map(|index| index + 1).unwrap_or(3);
    let close = trimmed.rfind("```").unwrap_or(trimmed.len());
    trimmed[after_open..close].trim()
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
        .map(|_| ClassificationResponse {
            tags: Vec::new(),
        })
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
        let Some(idx) = el["index"].as_u64() else {
            continue;
        };
        if idx as usize >= expected_len {
            continue;
        }
        let tags = el["tags"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        out[idx as usize] = ClassificationResponse { tags };
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
        let Some(idx) = el["index"].as_u64() else {
            continue;
        };
        let Some(cand) = candidates.get(idx as usize) else {
            continue;
        };
        let Some(reason) = el["reason"].as_str() else {
            continue;
        };
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
        assert!(
            blocks.len() >= 2,
            "header must not merge with body, got {:?}",
            blocks
        );
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
        let blocks = extract_blocks(
            "# News\n\nA longer body paragraph follows here.",
            MAX_CHARS_PER_SEGMENT,
        );
        assert!(blocks.iter().any(|b| b == "# News"), "got {:?}", blocks);
    }

    /// Header levels 1-6 and setext-lookalikes are classified correctly.
    #[test]
    fn test_is_atx_header() {
        assert!(is_atx_header("# Top"));
        assert!(is_atx_header("###### Six"));
        assert!(is_atx_header("  ## Indented"));
        assert!(!is_atx_header("####### Seven")); // 7 hashes = not a header
        assert!(!is_atx_header("#NoSpace")); // requires space
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
        let html = format!(
            r#"<a href="{}">link</a>"#,
            "x".repeat(MAX_CHARS_PER_SEGMENT + 100)
        );
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
            }
        );
    }

    #[test]
    fn test_parse_classification_json_no_category() {
        let json = r#"{"tags":["news"],"category":null}"#;
        let result = parse_classification_json(json).unwrap();
        assert_eq!(result.tags, vec!["news"]);
    }

    #[test]
    fn test_parse_classification_json_empty_tags() {
        let json = r#"{"tags":[],"category":"other"}"#;
        let result = parse_classification_json(json).unwrap();
        assert!(result.tags.is_empty());
    }

    #[test]
    fn test_parse_tag_explanations_matches_only_requested_names() {
        let requested = vec!["machine_learning".to_string(), "cooking".to_string()];
        let response = "```json\n[{\"name\":\"machine_learning\",\"explanation\":\"Branch of AI.\"},{\"name\":\"INVENTED\",\"explanation\":\"x\"},{\"name\":\"Cooking\",\"explanation\":\"Preparing food.\"}]\n```";

        let parsed = parse_tag_explanations_json(response, &requested);
        assert_eq!(parsed.len(), 2, "invented names must be dropped");
        assert_eq!(parsed[0].name, "machine_learning");
        // A case-insensitive match keeps the spelling stored in the database.
        assert_eq!(parsed[1].name, "cooking");
        assert_eq!(parsed[1].explanation, "Preparing food.");
    }

    #[test]
    fn test_parse_tag_explanations_degrades_on_bad_input() {
        let requested = vec!["cooking".to_string()];
        assert!(parse_tag_explanations_json("no json here", &requested).is_empty());
        // A blank definition is not worth indexing.
        let blank = "[{\"name\":\"cooking\",\"explanation\":\"   \"}]";
        assert!(parse_tag_explanations_json(blank, &requested).is_empty());
        // An overlong answer is bounded rather than stored whole.
        let long = format!(
            "[{{\"name\":\"cooking\",\"explanation\":\"{}\"}}]",
            "x".repeat(1_000)
        );
        let parsed = parse_tag_explanations_json(&long, &requested);
        assert_eq!(
            parsed[0].explanation.chars().count(),
            crate::ai::MAX_EXPLANATION_CHARS
        );
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
    }

    /// Regression: many models wrap JSON in ```json ... ``` fences.
    #[test]
    fn test_parse_classification_json_strips_fenced_block() {
        let wrapped = "```json\n{\"tags\":[\"a\"],\"category\":\"x\"}\n```";
        let result = parse_classification_json(wrapped).unwrap();
        assert_eq!(result.tags, vec!["a"]);
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
        let resp =
            r#"[{"index":9,"reason":"x"},{"index":0,"reason":"a"},{"index":0,"reason":"b"}]"#;
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
        assert!(!is_html_content(
            "some <strong>bold</strong> with no closing"
        ));
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

    #[test]
    fn test_ai_config_masked_key_fails() {
        let config = AiConfig {
            api_key: "sk-****1234".into(),
            base_url: "https://api.example.com".into(),
            model: "gpt-4".into(),
            max_tokens: None,
            temperature: None,
            max_chars_per_segment: None,
        };
        assert!(config.is_valid().is_err());
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;
    use tokio::sync::mpsc;

    /// The gate must never lose a slot: a cancelled waiter used to leave a
    /// sender in the queue whose receiver was gone, and `send` failing left
    /// `busy = true` forever.
    #[tokio::test]
    async fn cancelled_waiter_does_not_deadlock_the_gate() {
        let hold = llm_acquire(false).await;

        let queued = tokio::spawn(async { llm_acquire(false).await });
        // Let the task reach the queue before cancelling it.
        tokio::time::sleep(Duration::from_millis(30)).await;
        queued.abort();
        let _ = queued.await;
        drop(hold);

        let permit = tokio::time::timeout(Duration::from_secs(2), llm_acquire(false))
            .await
            .expect("gate must recover after a cancelled waiter");
        drop(permit);
    }

    /// Interactive work jumps ahead of background batches, and callers in the
    /// same class are served in arrival order.
    #[tokio::test]
    async fn interactive_preempts_background_and_same_class_is_fifo() {
        let (tx, mut rx) = mpsc::unbounded_channel::<&'static str>();
        let hold = llm_acquire(false).await;

        let spawn = |label: &'static str, priority: bool| {
            let tx = tx.clone();
            tokio::spawn(async move {
                let _permit = llm_acquire(priority).await;
                let _ = tx.send(label);
            })
        };
        spawn("background-1", false);
        spawn("background-2", false);
        spawn("interactive", true);
        // Let all three reach the queue (they arrive in this order).
        tokio::time::sleep(Duration::from_millis(50)).await;

        drop(hold);

        let mut order = Vec::new();
        for _ in 0..3 {
            order.push(
                tokio::time::timeout(Duration::from_secs(2), rx.recv())
                    .await
                    .expect("gate should keep serving")
                    .expect("channel open"),
            );
        }
        assert_eq!(order, ["interactive", "background-1", "background-2"]);
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A service with no network: `new` only builds a client.
    fn service() -> LlmAiService {
        LlmAiService::new(AiConfig {
            api_key: "test-key".into(),
            base_url: "https://example.invalid/v1".into(),
            model: "test-model".into(),
            max_tokens: Some(16_000),
            temperature: None,
            max_chars_per_segment: Some(3_000),
        })
        .expect("service")
    }

    fn pair(original: &str) -> String {
        format!(
            "<div class=\"translation-paragraph\">\n\
             <div class=\"paragraph-original\">{original}</div>\n\
             <div class=\"paragraph-translated\">译文</div>\n</div>"
        )
    }

    fn paragraph(text: &str) -> String {
        format!("{text} {}

", "内容".repeat(200))
    }

    /// CJK text is three bytes per character, so a byte index computed from a
    /// character count lands inside a character: the split drifted to roughly a
    /// third of the way in (and `split_at` would panic on an unlucky boundary).
    #[test]
    fn splitting_is_byte_safe_and_balanced() {
        let block = format!("{}{}", paragraph("第一段"), paragraph("第二段"));
        let (first, second) = split_for_translation(&block).expect("splittable");

        let total = block.chars().count();
        let a = first.chars().count();
        let b = second.chars().count();
        assert!(a + b == total, "nothing lost: {a} + {b} != {total}");
        assert!(
            a > total / 3 && b > total / 3,
            "split must be balanced, got {a} + {b} of {total}"
        );
        assert_eq!(count_paragraphs(&first), 1, "split on the paragraph boundary");
        assert_eq!(count_paragraphs(&second), 1);
    }

    #[test]
    fn marking_and_stripping_round_trip() {
        let block = "第一段。\n\n第二段。\n\n第三段。";
        let marked = mark_paragraphs(block);
        assert_eq!(paragraph_markers_in(&marked), vec![1, 2, 3]);
        assert!(marked.contains("###P1###\n第一段。"), "{marked}");
        assert_eq!(strip_paragraph_markers(&marked), block, "round-trip is lossless");

        // A single paragraph still gets a marker.
        assert_eq!(paragraph_markers_in(&mark_paragraphs("只有一段")), vec![1]);
        // Nothing paragraph-shaped: leave the block alone.
        assert_eq!(mark_paragraphs("   \n\n  "), "   \n\n  ");
    }

    #[test]
    fn stripping_removes_markers_without_touching_content() {
        // Marker on its own line: the line disappears entirely.
        assert_eq!(
            strip_paragraph_markers("###P1###\n正文一\n###P2###\n正文二"),
            "正文一\n正文二"
        );
        // Marker inline (some models put it inside the div): only the token goes.
        assert_eq!(
            strip_paragraph_markers("<div>###P7###正文</div>"),
            "<div>正文</div>"
        );
        // Blank lines inside preserved markdown survive.
        let answer = "###P1###\n<pre>a\n\nb</pre>";
        assert_eq!(strip_paragraph_markers(answer), "<pre>a\n\nb</pre>");
        // Something that only looks like a marker is left alone.
        assert_eq!(strip_paragraph_markers("###Px###"), "###Px###");
        assert_eq!(strip_paragraph_markers("###P12##"), "###P12##");
    }

    /// The contract the prompt asks for: markers repeated, one pair each.
    #[tokio::test]
    async fn marked_answers_keep_one_pair_per_paragraph() {
        let svc = service();
        let block = format!("{}{}{}", paragraph("第一段"), paragraph("第二段"), paragraph("第三段"));

        let answer = svc
            .translate_with_recovery(&block, 0, move |marked: String| async move {
                // A well-behaved model: repeat each marker, one pair per block.
                let mut out = String::new();
                for chunk in marked.split("\n\n") {
                    let chunk = chunk.trim();
                    if chunk.is_empty() {
                        continue;
                    }
                    let (marker, text) = match chunk.split_once('\n') {
                        Some((head, rest)) if head.starts_with("###P") => (head, rest),
                        _ => ("", chunk),
                    };
                    if !marker.is_empty() {
                        out.push_str(marker);
                        out.push('\n');
                    }
                    out.push_str(&pair(text.trim()));
                    out.push('\n');
                }
                Ok(out)
            })
            .await
            .expect("well-formed answer");

        assert_eq!(count_translation_pairs(&answer), 3);
        assert!(
            !answer.contains("###P"),
            "markers must never reach the reader: {answer}"
        );
    }

    /// A model that answers only part of the block must not be accepted just
    /// because the pairs it did return look proportionate.
    #[test]
    fn missing_paragraphs_are_detected_from_the_markers() {
        let block = (1..=14)
            .map(|n| format!("段落{n} {}\n\n", "内容".repeat(40)))
            .collect::<String>();
        assert_eq!(count_paragraphs(&block), 14);

        // 8 of 14 answered: `8 * 2 >= 14` would pass the ratio check.
        let partial: String = (1..=8)
            .map(|n| format!("###P{n}###\n{}", pair(&format!("第{n}段"))))
            .collect::<String>();
        assert!(
            collapses_paragraphs(&partial, &block),
            "a partial answer must be treated as a structural failure"
        );

        // All 14 answered in 14 pairs: accepted.
        let complete: String = (1..=14)
            .map(|n| format!("###P{n}###\n{}", pair(&format!("第{n}段"))))
            .collect::<String>();
        assert!(!collapses_paragraphs(&complete, &block));

        // Markers ignored but the pairs are all there: accepted on the ratio.
        let unmarked: String = (1..=14).map(|n| pair(&format!("第{n}段"))).collect();
        assert!(!collapses_paragraphs(&unmarked, &block));
    }

    #[test]
    fn paragraph_and_pair_counting() {
        let block = "one

still one


 two

three";
        assert_eq!(
            count_paragraphs(block),
            4,
            "blank-line-separated runs are paragraphs; consecutive blanks collapse"
        );
        assert_eq!(count_paragraphs("   \n\n  "), 0, "whitespace is not a paragraph");
        assert_eq!(count_translation_pairs(&pair("x")), 1);
        assert_eq!(count_translation_pairs(&format!("{}{}", pair("x"), pair("y"))), 2);
    }

    /// The model returned the whole block as one pair: the reader would see all
    /// the original text followed by all the translation instead of paragraph
    /// pairs. Recovery splits the block so each paragraph gets its own pair.
    #[tokio::test]
    async fn collapsed_answer_is_recovered_by_splitting_at_paragraph_boundaries() {
        let svc = service();
        let block = format!("{}{}", paragraph("第一段"), paragraph("第二段"));
        assert_eq!(count_paragraphs(&block), 2);
        assert!(block.chars().count() > MIN_SPLITTABLE_CHARS);

        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let answer = svc
            .translate_with_recovery(&block, 0, move |part: String| {
                counter.fetch_add(1, Ordering::SeqCst);
                // Always one pair, whatever the input — the collapse case.
                async move { Ok(pair(&part)) }
            })
            .await
            .expect("recovery should succeed");

        assert!(
            count_translation_pairs(&answer) >= 2,
            "the collapsed answer must be split into per-paragraph pairs: {answer}"
        );
        assert!(calls.load(Ordering::SeqCst) >= 2, "the block was re-asked in parts");
    }

    /// A model that answers correctly is not split (no wasted calls).
    #[tokio::test]
    async fn a_correct_multi_pair_answer_is_used_as_is() {
        let svc = service();
        let block = format!("{}{}", paragraph("第一段"), paragraph("第二段"));

        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let answer = svc
            .translate_with_recovery(&block, 0, move |part: String| {
                counter.fetch_add(1, Ordering::SeqCst);
                let per_paragraph: String = part
                    .split("\n\n")
                    .filter(|p| !p.trim().is_empty())
                    .map(pair)
                    .collect::<Vec<_>>()
                    .join("\n");
                async move { Ok(per_paragraph) }
            })
            .await
            .expect("passthrough");

        assert_eq!(calls.load(Ordering::SeqCst), 1, "one call, no splitting");
        assert_eq!(count_translation_pairs(&answer), 2);
    }

    /// Truncation (reasoning models run out of budget) is recovered the same
    /// way, and the two halves are concatenated in order.
    #[tokio::test]
    async fn truncation_is_recovered_by_splitting() {
        let svc = service();
        let block = format!("{}{}", paragraph("第一段"), paragraph("第二段"));

        let answer = svc
            .translate_with_recovery(&block, 0, move |part: String| {
                let length = part.chars().count();
                async move {
                    if length > MIN_SPLITTABLE_CHARS {
                        Err(AppError::OperationFailed(
                            "LLM response was truncated (finish_reason=length) for a block with max_tokens=10"
                                .into(),
                        ))
                    } else {
                        Ok(pair(&part))
                    }
                }
            })
            .await
            .expect("recovery should succeed");

        assert_eq!(count_translation_pairs(&answer), 2);
        let first = answer.find("第一段").unwrap_or(usize::MAX);
        let second = answer.find("第二段").unwrap_or(usize::MAX);
        assert!(first < second, "halves are concatenated in source order");
    }

    /// Once the depth budget is spent, a coarse-but-valid answer beats failing
    /// the whole article.
    #[tokio::test]
    async fn exhausted_depth_keeps_a_valid_answer() {
        let svc = service();
        let block = format!("{}{}", paragraph("第一段"), paragraph("第二段"));
        let answer = svc
            .translate_with_recovery(&block, MAX_SPLIT_DEPTH, move |part: String| {
                async move { Ok(pair(&part)) }
            })
            .await
            .expect("no split attempts left, but the answer is usable");
        assert_eq!(count_translation_pairs(&answer), 1);
    }

    /// Nothing usable in the answer: fail loudly rather than cache junk.
    #[tokio::test]
    async fn a_non_bilingual_answer_is_rejected() {
        let svc = service();
        let error = svc
            .translate_with_recovery("short paragraph", 0, move |_: String| async move {
                Ok("I cannot help with that.".to_string())
            })
            .await
            .expect_err("must not be accepted");
        assert!(error.to_string().contains("not a bilingual pair"), "{error}");
    }
}

#[cfg(test)]
mod topic_suggestion_tests {
    use super::parse_topic_suggestions_json;

    fn requested() -> Vec<String> {
        vec!["rust".to_string(), "opinion".to_string(), "docker".to_string()]
    }

    #[test]
    fn topic_suggestions_parser_drops_anything_it_cannot_place() {
        let allowed = [5_i64, 12];
        let response = r#"[
            {"name": "rust", "category_id": 5, "state": "assigned", "reason": "language"},
            {"name": "opinion", "category_id": null, "state": "context_only", "reason": "format"},
            {"name": "docker", "category_id": 99, "state": "assigned", "reason": "not in catalog"},
            {"name": "invented", "category_id": 5, "state": "assigned", "reason": "never asked"},
            {"name": "DOCKER", "category_id": 12, "state": "assigned", "reason": "case folded"},
            {"name": "opinion", "category_id": 5, "state": "context_only", "reason": "contradicts itself"},
            {"name": "rust", "category_id": null, "state": "assigned", "reason": "no topic"}
        ]"#;
        let parsed = parse_topic_suggestions_json(response, &requested(), &allowed);
        // The four bad rows are gone: an id outside the catalog, a name nobody
        // asked about, a state that contradicts the id, and an "assigned" with
        // no topic. What remains is one proposal per requested word, spelled
        // the way the database spells it.
        let summary: Vec<(&str, Option<i64>, &str)> = parsed
            .iter()
            .map(|item| (item.name.as_str(), item.category_id, item.state.as_str()))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("rust", Some(5), "assigned"),
                ("opinion", None, "context_only"),
                ("docker", Some(12), "assigned"),
            ]
        );
    }

    #[test]
    fn topic_suggestions_parser_keeps_review_verdicts_and_rejects_junk() {
        let allowed = [5_i64];
        let fence = "```json\n[{\"name\": \"rust\", \"category_id\": null, \"state\": \"review\", \"reason\": \"unsure\"}]\n```";
        let parsed = parse_topic_suggestions_json(fence, &requested(), &allowed);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].state, "review");

        // A truncated or non-array answer yields nothing rather than a guess.
        assert!(parse_topic_suggestions_json("{\"name\": \"rust\"}", &requested(), &allowed).is_empty());
        assert!(parse_topic_suggestions_json("", &requested(), &allowed).is_empty());
    }
}

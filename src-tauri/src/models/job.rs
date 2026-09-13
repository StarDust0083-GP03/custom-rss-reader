use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Kinds of background work that follow a committed article.
///
/// Enrichment is never part of the ingest path: the article row and its jobs
/// are committed in one transaction, and a worker drains them afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// Classify one article's title into tags/category (batched per claim).
    Classify,
    /// Fetch the article's website page and cache the Markdown conversion.
    WebsiteMarkdown,
    /// (Re)index one article in the semantic index.
    ChromaUpsert,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::Classify => "classify",
            JobKind::WebsiteMarkdown => "website_markdown",
            JobKind::ChromaUpsert => "chroma_upsert",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "classify" => Some(JobKind::Classify),
            "website_markdown" => Some(JobKind::WebsiteMarkdown),
            "chroma_upsert" => Some(JobKind::ChromaUpsert),
            _ => None,
        }
    }

    /// Higher priority runs first. Website caching outranks indexing so a
    /// reader who opens an article soon after refresh sees real text before
    /// the semantic index finishes catching up.
    pub fn priority(self) -> i64 {
        match self {
            JobKind::WebsiteMarkdown => 20,
            JobKind::ChromaUpsert => 10,
            JobKind::Classify => 5,
        }
    }

    pub fn max_attempts(self) -> i64 {
        match self {
            // Network work gets more chances than an LLM call, which costs
            // tokens on every retry.
            JobKind::WebsiteMarkdown | JobKind::ChromaUpsert => 5,
            JobKind::Classify => 3,
        }
    }
}

/// A queued unit of post-ingest work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: i64,
    pub kind: String,
    pub item_id: Option<i64>,
    /// Free-form JSON payload (e.g. the article URL for website fetches).
    pub payload: Option<String>,
    pub state: String,
    pub priority: i64,
    pub attempts: i64,
    pub max_attempts: i64,
    pub next_attempt_at: DateTime<Utc>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl Job {
    pub fn kind_enum(&self) -> Option<JobKind> {
        JobKind::parse(&self.kind)
    }
}

/// A job to enqueue alongside its article.
#[derive(Debug, Clone)]
pub struct NewJob {
    pub kind: JobKind,
    pub item_id: Option<i64>,
    pub payload: Option<String>,
}

impl NewJob {
    /// `item_id: None` means "the article this job is committed with". The
    /// repository fills it in from the row it just inserted.
    ///
    /// A `Some(0)` placeholder is refused by `NewJob::with_item_id` and by the
    /// repository, because SQLite AUTOINCREMENT ids start at 1: a job
    /// referencing id 0 can never load an article, so it would be completed as
    /// "article not found" and the work would silently never happen.
    pub fn classify(item_id: Option<i64>) -> Self {
        Self {
            kind: JobKind::Classify,
            item_id: Self::with_item_id(item_id),
            payload: None,
        }
    }

    pub fn website_markdown(item_id: Option<i64>, url: String) -> Self {
        Self {
            kind: JobKind::WebsiteMarkdown,
            item_id: Self::with_item_id(item_id),
            payload: Some(url),
        }
    }

    pub fn chroma_upsert(item_id: Option<i64>) -> Self {
        Self {
            kind: JobKind::ChromaUpsert,
            item_id: Self::with_item_id(item_id),
            payload: None,
        }
    }

    /// Treat a non-positive id as "not set yet" instead of as a real row id.
    fn with_item_id(item_id: Option<i64>) -> Option<i64> {
        item_id.filter(|id| *id > 0)
    }
}

/// Queue depth and recent failures, surfaced to the UI so background work is
/// visible instead of silent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JobStats {
    pub queued: i64,
    pub running: i64,
    pub failed: i64,
    pub succeeded: i64,
    /// First few failure messages, newest first (bounded).
    pub recent_errors: Vec<String>,
    /// Queued jobs per kind, so a caller can explain what is waiting.
    #[serde(default)]
    pub queued_by_kind: std::collections::BTreeMap<String, i64>,
    /// Human-readable reasons why queued work is not progressing yet
    /// (for example "AI is not configured"). Filled in by the command layer,
    /// which can see the runtime configuration the queue itself cannot.
    #[serde(default)]
    pub blocked_reasons: Vec<String>,
}

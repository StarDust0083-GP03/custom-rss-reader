//! Background job worker.
//!
//! The worker turns the durable queue in [`crate::repositories::job_repo`]
//! into actual enrichment:
//!
//! * `classify` — one LLM call per claimed batch of article titles.
//! * `website_markdown` — fetch the article page, convert to Markdown, cache.
//! * `chroma_upsert` — (re)index the article for semantic search.
//!
//! It never runs as part of a refresh: the fetch returns as soon as the
//! articles and their jobs are committed, so a slow provider or an unreachable
//! website cannot delay the reader's fresh articles, and a failure becomes a
//! retryable row with a visible error instead of an `eprintln!` nobody sees.

use std::sync::Arc;
use std::time::Duration;

use crate::ai::activity::{with_ai_task, AiActivityStore, AiTaskSpec};
use crate::ai::service::{AiService, SharedAiService};
use crate::chroma::ChromaHolder;
use crate::content_processor::html_to_markdown_pipeline;
use crate::error::{AppError, Result};
use crate::feed::FeedFetcher;
use crate::models::job::{Job, JobKind, NewJob};
use crate::repositories::job_repo::JobRepository;
use crate::repositories::FeedItemRepository;
use crate::services::tag_matcher::TagMatcher;

/// Concurrent worker loops. Enrichment is I/O bound and the LLM gate caps
/// provider concurrency separately, so a small number of loops keeps the
/// queue moving without flooding a single website with requests.
const WORKER_CONCURRENCY: usize = 2;

/// How many jobs one claim may take. Matches the classification batch size so
/// one model call covers a full claim.
const CLAIM_BATCH: i64 = crate::ai::CLASSIFY_BATCH_SIZE as i64;

/// Idle poll interval.
const IDLE_SLEEP: Duration = Duration::from_millis(750);

/// Per-host pacing for article fetches (one article per interval per host).
const WEBSITE_FETCH_INTERVAL: Duration = Duration::from_millis(400);

/// Terminal rows kept in the queue table.
const FINISHED_RETENTION: i64 = 2000;

/// How often the aggregated failure summary may be printed.
const FLUSH_INTERVAL: Duration = Duration::from_secs(30);

/// Builds an AI service when the shared slot is empty.
///
/// Injectable so tests never touch the developer's real
/// `~/.rss-reader/ai_config.json` (and never call a live API), and so the
/// "configured while running" path can be tested deterministically.
pub type AiLoader = Arc<dyn Fn() -> Option<Arc<dyn AiService>> + Send + Sync>;

/// Aggregated failure reporting.
///
/// One line per failed job is unusable during a bulk refresh: a single dead
/// host can produce hundreds of identical 404s and hide everything else. The
/// worker counts failures by (kind, reason, permanent/retryable) and prints a
/// summary when the queue goes quiet.
#[derive(Default)]
struct FailureCounters {
    counts: std::sync::Mutex<std::collections::BTreeMap<String, (usize, usize)>>,
    last_flush: std::sync::Mutex<Option<std::time::Instant>>,
}

impl FailureCounters {
    fn record(&self, kind: &str, message: &str, permanent: bool) {
        let key = format!("{kind}\u{1}{}\u{1}{}", shorten_reason(message), permanent as u8);
        if let Ok(mut counts) = self.counts.lock() {
            let entry = counts.entry(key).or_insert((0, 0));
            if permanent {
                entry.1 += 1;
            } else {
                entry.0 += 1;
            }
        }
    }

    /// Print at most once every `FLUSH_INTERVAL`, or immediately when forced
    /// (the queue went idle, so the summary is complete).
    fn flush_if_due(&self, force: bool) {
        {
            let Ok(last) = self.last_flush.lock() else {
                return;
            };
            if !force {
                if let Some(last) = *last {
                    if last.elapsed() < FLUSH_INTERVAL {
                        return;
                    }
                }
            }
        }
        let Ok(mut counts) = self.counts.lock() else {
            return;
        };
        if counts.is_empty() {
            return;
        }
        if let Ok(mut last) = self.last_flush.lock() {
            *last = Some(std::time::Instant::now());
        }
        let taken = std::mem::take(&mut *counts);

        // Group by kind so the summary reads like a report, not a list.
        let mut by_kind: std::collections::BTreeMap<&str, (usize, usize, Vec<String>)> =
            std::collections::BTreeMap::new();
        for (key, (retryable, permanent)) in &taken {
            let mut parts = key.split('\u{1}');
            let kind = parts.next().unwrap_or("job");
            let reason = parts.next().unwrap_or("");
            let entry = by_kind.entry(kind).or_insert((0, 0, Vec::new()));
            entry.0 += retryable;
            entry.1 += permanent;
            if *permanent > 0 {
                entry.2.push(format!("{reason} ×{permanent}"));
            }
        }
        for (kind, (retryable, permanent, permanent_reasons)) in by_kind {
            let mut line = format!("[jobs] {kind}: {permanent} parked permanently");
            if retryable > 0 {
                line.push_str(&format!(", {retryable} retrying"));
            }
            if !permanent_reasons.is_empty() {
                line.push_str(" (");
                line.push_str(&permanent_reasons.join("; "));
                line.push(')');
            }
            eprintln!("{line}");
        }
    }
}

/// Common failure message with the varying part (URL, job id) removed, so
/// identical problems share one counter.
pub(crate) fn shorten_reason(message: &str) -> String {
    let head = message.split(" (").next().unwrap_or(message);
    head.chars().take(80).collect()
}

/// Failures that retrying cannot fix.
///
/// * An article URL that answers 4xx (except 401/403/408/429, which a rotated
///   User-Agent or a pause can change) is gone.
/// * "No main content" means the page has no server-rendered text — a
///   JavaScript-only app or a bot-challenge interstitial. The fetcher does not
///   execute scripts, so every attempt returns the same empty shell.
pub(crate) fn is_permanent_failure(kind: &str, error: &AppError) -> bool {
    if kind != JobKind::WebsiteMarkdown.as_str() {
        // Classification and indexing failures are transient by nature (a
        // provider outage, a Chroma restart).
        return false;
    }
    let message = error.to_string();
    if message.contains("No main content found") {
        return true;
    }
    if let Some(status) = http_status_in(&message) {
        return (400..500).contains(&status) && ![401, 403, 408, 429].contains(&status);
    }
    false
}

/// Extract an HTTP status code from a fetch error message.
///
/// Two shapes exist: `HTTP status: 404 Not Found` (website fetch) and
/// `HTTP 404 Not Found` (feed fetch / status mapping).
pub(crate) fn http_status_in(message: &str) -> Option<u16> {
    let after = message
        .split_once("HTTP status: ")
        .map(|(_, rest)| rest)
        .or_else(|| message.split_once("HTTP ").map(|(_, rest)| rest))?;
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

pub struct JobWorker {
    jobs: Arc<dyn JobRepository>,
    repo: Arc<dyn FeedItemRepository>,
    ai_service: SharedAiService,
    ai_activity: AiActivityStore,
    fetcher: Option<Arc<FeedFetcher>>,
    chroma: ChromaHolder,
    tag_matcher: Arc<TagMatcher>,
    ai_loader: AiLoader,
    failures: Arc<FailureCounters>,
}

impl JobWorker {
    pub fn new(
        jobs: Arc<dyn JobRepository>,
        repo: Arc<dyn FeedItemRepository>,
        ai_service: SharedAiService,
        ai_activity: AiActivityStore,
        fetcher: Option<Arc<FeedFetcher>>,
        chroma: ChromaHolder,
        tag_matcher: Arc<TagMatcher>,
    ) -> Self {
        Self {
            jobs,
            repo,
            ai_service,
            ai_activity,
            fetcher,
            chroma,
            tag_matcher,
            ai_loader: Arc::new(crate::ai::load_configured_service),
            failures: Arc::new(FailureCounters::default()),
        }
    }

    /// Replace the on-demand AI loader.
    ///
    /// Only tests need this: production always loads from the config file, and
    /// keeping the seam test-only avoids carrying unused public surface.
    #[cfg(test)]
    pub fn with_ai_loader(mut self, loader: AiLoader) -> Self {
        self.ai_loader = loader;
        self
    }

    /// Spawn the worker loops. Runs for the lifetime of the process.
    pub fn spawn(self: Arc<Self>) {
        for _ in 0..WORKER_CONCURRENCY {
            let worker = Arc::clone(&self);
            tauri::async_runtime::spawn(async move {
                worker.run().await;
            });
        }
    }

    async fn run(&self) {
        // Recover work leased by a crashed run before taking new jobs.
        if let Err(e) = self.jobs.requeue_expired().await {
            eprintln!("[jobs] requeue failed: {}", e);
        }

        let mut idle_rounds = 0u32;
        loop {
            match self.tick().await {
                Ok(true) => idle_rounds = 0,
                Ok(false) => {
                    idle_rounds = idle_rounds.saturating_add(1);
                    // The queue is quiet: the summary is complete, so print it.
                    self.failures.flush_if_due(true);
                    tokio::time::sleep(IDLE_SLEEP).await;
                    if idle_rounds.is_multiple_of(120) {
                        let _ = self.jobs.requeue_expired().await;
                        let _ = self.jobs.prune_finished(FINISHED_RETENTION).await;
                    }
                }
                Err(e) => {
                    eprintln!("[jobs] worker error: {}", e);
                    tokio::time::sleep(IDLE_SLEEP).await;
                }
            }
        }
    }

    /// Job kinds that can actually make progress right now.
    ///
    /// Claiming work whose service is unavailable burns the attempt budget and
    /// buries real problems under noise: with no AI configuration every
    /// classification is guaranteed to fail, so those jobs simply stay queued
    /// until one is configured. Website caching has no such dependency.
    async fn claimable_kinds(&self) -> Vec<&'static str> {
        let mut kinds = vec![JobKind::WebsiteMarkdown.as_str()];
        if self.ai().await.is_some() {
            kinds.push(JobKind::Classify.as_str());
        }
        if self.chroma.is_configured() {
            kinds.push(JobKind::ChromaUpsert.as_str());
        }
        kinds
    }

    /// Claim and process one batch. Returns `true` when work was found.
    ///
    /// `pub(crate)` so integration tests can drive one deterministic round
    /// instead of racing the background loop.
    pub(crate) async fn tick(&self) -> Result<bool> {
        let kinds = self.claimable_kinds().await;
        let claimed = self.jobs.claim_batch(CLAIM_BATCH, &kinds).await?;
        if claimed.is_empty() {
            return Ok(false);
        }

        // Classify jobs are cheapest in bulk: one model call for the whole
        // group instead of one per article.
        let (classify, others): (Vec<Job>, Vec<Job>) = claimed
            .into_iter()
            .partition(|job| job.kind_enum() == Some(JobKind::Classify));

        if !classify.is_empty() {
            self.run_classify_batch(&classify).await;
        }
        for job in others {
            match job.kind_enum() {
                Some(JobKind::WebsiteMarkdown) => self.run_website_markdown(&job).await,
                Some(JobKind::ChromaUpsert) => self.run_chroma_upsert(&job).await,
                _ => {
                    let _ = self
                        .jobs
                        .fail(job.id, job.attempts, "unknown job kind")
                        .await;
                }
            }
        }
        self.failures.flush_if_due(false);
        Ok(true)
    }

    /// Finish a job, or record its failure.
    ///
    /// Failures are logged in aggregate: a library-wide refresh can produce
    /// hundreds of identical 404s, and one terminal line per attempt buries
    /// every other message.
    async fn settle(&self, job: &Job, result: Result<()>) {
        match result {
            Ok(()) => {
                if let Err(e) = self.jobs.complete(job.id, job.attempts).await {
                    eprintln!("[jobs] complete failed for job {}: {}", job.id, e);
                }
            }
            Err(error) => {
                let message = error.to_string();
                if matches!(error, AppError::NotFound(_)) {
                    // The article is gone (deleted subscription, cascade).
                    // Retrying cannot help.
                    let _ = self.jobs.complete(job.id, job.attempts).await;
                    return;
                }

                let permanent = is_permanent_failure(&job.kind, &error);
                self.failures.record(&job.kind, &message, permanent);
                let outcome = if permanent {
                    self.jobs.fail_permanently(job.id, job.attempts, &message).await
                } else {
                    self.jobs.fail(job.id, job.attempts, &message).await
                };
                if let Err(e) = outcome {
                    eprintln!("[jobs] fail bookkeeping failed for job {}: {}", job.id, e);
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // classify
    // ------------------------------------------------------------------

    /// The configured AI service, loading it from disk the first time it is
    /// needed and caching it in the shared slot.
    ///
    /// Without the disk fallback the worker only ever saw the service built at
    /// startup, so configuring AI while the app was running left every
    /// classification job waiting until the next restart.
    async fn ai(&self) -> Option<Arc<dyn AiService>> {
        if let Some(service) = self.ai_service.read().await.clone() {
            return Some(service);
        }
        let service = (self.ai_loader)()?;
        *self.ai_service.write().await = Some(service.clone());
        println!("[jobs] AI configuration loaded; classification can start");
        Some(service)
    }

    async fn run_classify_batch(&self, jobs: &[Job]) {
        let Some(ai) = self.ai().await else {
            // No AI configuration: completing without work is wrong (the user
            // may configure it later), but retrying forever burns the queue.
            // Park them with a clear message so they are visible and
            // explicitly retryable.
            for job in jobs {
                let _ = self
                    .jobs
                    .fail(job.id, job.attempts, "AI is not configured")
                    .await;
            }
            return;
        };

        // Load items that still exist, preserving job order.
        let mut items = Vec::new();
        let mut live_jobs = Vec::new();
        for job in jobs {
            let Some(item_id) = job.item_id else {
                let _ = self
                    .jobs
                    .fail(job.id, job.attempts, "job has no item")
                    .await;
                continue;
            };
            match self.repo.find_by_id(item_id).await {
                Ok(item) => {
                    items.push(item);
                    live_jobs.push(job.clone());
                }
                Err(AppError::NotFound(_)) => {
                    let _ = self.jobs.complete(job.id, job.attempts).await;
                }
                Err(e) => {
                    let _ = self.jobs.fail(job.id, job.attempts, &e.to_string()).await;
                }
            }
        }
        if items.is_empty() {
            return;
        }

        let entries: Vec<crate::ai::BatchClassifyEntry> = items
            .iter()
            .enumerate()
            .map(|(index, item)| crate::ai::BatchClassifyEntry {
                index,
                title: item.title.clone(),
            })
            .collect();

        let result = async {
            let existing_tags = self.repo.find_vocabulary_names().await?;
            ai.classify_batch(&entries, &existing_tags).await
        }
        .await;

        let responses = match result {
            Ok(responses) => responses,
            Err(error) => {
                // One job per item is wrong for a batch, but the retry policy
                // is per job; recording the same error on each keeps the
                // backoff honest and bounded.
                for job in &live_jobs {
                    let _ = self
                        .jobs
                        .fail(job.id, job.attempts, &error.to_string())
                        .await;
                }
                return;
            }
        };

        let task = self
            .ai_activity
            .begin(AiTaskSpec::background_classification(items.len()))
            .await;
        with_ai_task(task.clone(), async {
            for ((job, item), response) in live_jobs.iter().zip(items.iter()).zip(responses) {
                if response.tags.is_empty() {
                    let _ = self.jobs.complete(job.id, job.attempts).await;
                    continue;
                }
                // `resolve` is what learns a new alias; the ORIGINAL names are
                // what gets persisted. Saving the resolved list would store the
                // matcher's output as if the classifier had produced it, and an
                // adoption change could then no longer be undone.
                if let Err(e) = self
                    .tag_matcher
                    .resolve(self.repo.as_ref(), &response.tags)
                    .await
                {
                    eprintln!("Tag matching failed for item {}: {}", item.id, e);
                }
                let raw_json = serde_json::to_string(&response.tags)
                    .unwrap_or_else(|_| "[]".to_string());
                match self.repo.save_tags(item.id, &raw_json).await {
                    Ok(_) => {
                        let _ = self.jobs.complete(job.id, job.attempts).await;
                    }
                    Err(e) => {
                        let _ = self.jobs.fail(job.id, job.attempts, &e.to_string()).await;
                    }
                }
            }
        })
        .await;
        task.finish().await;
    }

    // ------------------------------------------------------------------
    // website_markdown
    // ------------------------------------------------------------------

    async fn run_website_markdown(&self, job: &Job) {
        let Some(fetcher) = self.fetcher.clone() else {
            let _ = self
                .jobs
                .fail(job.id, job.attempts, "fetcher not configured")
                .await;
            return;
        };
        let Some(item_id) = job.item_id else {
            let _ = self
                .jobs
                .fail(job.id, job.attempts, "job has no item")
                .await;
            return;
        };
        let Some(url) = job.payload.clone() else {
            let _ = self
                .jobs
                .fail(job.id, job.attempts, "job has no article URL")
                .await;
            return;
        };

        let result = async {
            let item = self.repo.find_by_id(item_id).await?;
            // A cached website document is still valid; skip the network call.
            if item.is_website_content
                && item.content_md.as_deref().is_some_and(|md| !md.is_empty())
            {
                return Ok(());
            }
            let html = fetcher.fetch_website_content(&url).await?;
            let markdown = tokio::task::spawn_blocking(move || html_to_markdown_pipeline(&html))
                .await
                .map_err(|e| AppError::Internal(format!("markdown task failed: {}", e)))??;
            self.repo
                .update_content_md(item_id, &markdown, true)
                .await?;
            Ok(())
        }
        .await;

        // Per-host pacing: one article at a time per site. Applied after the
        // attempt so a fast cache hit is never delayed.
        tokio::time::sleep(WEBSITE_FETCH_INTERVAL).await;

        let succeeded = result.is_ok();
        self.settle(job, result).await;
        if succeeded {
            // The richer website text must be re-embedded, and that is a
            // separate durable job so a Chroma outage cannot fail the fetch.
            if let Err(e) = self.jobs.enqueue(&[NewJob::chroma_upsert(Some(item_id))]).await {
                eprintln!(
                    "[jobs] could not queue re-index for item {}: {}",
                    item_id, e
                );
            }
        }
    }

    // ------------------------------------------------------------------
    // chroma_upsert
    // ------------------------------------------------------------------

    async fn run_chroma_upsert(&self, job: &Job) {
        let Some(item_id) = job.item_id else {
            let _ = self
                .jobs
                .fail(job.id, job.attempts, "job has no item")
                .await;
            return;
        };
        let result = async {
            if !self.chroma.is_configured() {
                // Semantic search is off. Nothing to index, and leaving the job
                // queued forever would be indistinguishable from a backlog.
                return Ok(());
            }
            let Some(chroma) = self.chroma.get().await else {
                // Enabled but unreachable. That is a retryable failure, not a
                // success: silently completing it would leave the article
                // unindexed until the next full sync walk.
                return Err(AppError::Network(
                    "ChromaDB is configured but unreachable".into(),
                ));
            };
            let item = self.repo.find_by_id(item_id).await?;
            chroma.index_item(&item).await
        }
        .await;
        self.settle(job, result).await;
    }
}

//! Website-Markdown backfill for history-imported articles.
//!
//! ## The problem
//!
//! Website-mode subscriptions (`use_website = 1`) cache the full article
//! page as Markdown (`content_md`) so the reader — and the ChromaDB
//! semantic index — sees the whole text instead of the RSS teaser. But that
//! cache is only written at fetch time (or when an article is opened
//! manually). Articles imported with the feed's *history* — e.g. everything
//! a subscription returned the first time it was added, before website mode
//! was enabled, or items whose fetch-time pre-cache failed — never got a
//! website Markdown, so their embeddings were built from the RSS snippet
//! and semantic search can't find them by body text.
//!
//! ## The mechanism
//!
//! `backfill_website_markdown` refreshes Markdown for the most recent
//! affected articles, then queues them for re-indexing via the sync
//! engine's `pending_upserts` queue and runs one incremental sync so the
//! new text is embedded immediately.
//!
//! ## Politeness contract: QPS
//!
//! This pass hits other people's websites, so it is strictly rate-limited —
//! the goal is to catch up over time, never to crawl aggressively:
//!
//! - **QPS**: at most [`BACKFILL_MAX_QPS`] website requests per second,
//!   enforced with a minimum interval between request starts. Each fetch is
//!   a single attempt with no retry loop (unlike feed fetching, which
//!   retries with backoff).
//! - Supporting rails: at most [`BACKFILL_BATCH_LIMIT`] articles per run
//!   (newest first; repeated triggers drain the backlog gradually), only
//!   one backfill runs at a time, and a host that fails
//!   [`BACKFILL_HOST_FAILURE_LIMIT`] times in a row is skipped for the rest
//!   of the run so a broken site isn't hammered.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Serialize;

use crate::chroma::ChromaHolder;
use crate::content_processor::html_to_markdown_pipeline;
use crate::feed::FeedFetcher;
use crate::repositories::FeedItemRepository;

use super::sync::{self, SyncState};

/// **QPS** — hard cap on website requests per second (requests per second
/// across the whole backfill pass). 0.5 → one request every 2 seconds;
/// deliberately conservative because these are one-off catch-up fetches
/// to third-party article pages, not a feed the site expects us to poll.
pub const BACKFILL_MAX_QPS: f64 = 0.5;

/// Minimum interval between two request starts, derived from the QPS cap.
fn backfill_min_interval() -> std::time::Duration {
    std::time::Duration::from_millis((1000.0 / BACKFILL_MAX_QPS) as u64)
}

/// Supporting rail: at most this many articles per backfill run. The
/// backlog drains over repeated runs instead of one long crawl.
pub const BACKFILL_BATCH_LIMIT: i64 = 20;

/// Supporting rail: consecutive failures tolerated per host before the
/// rest of that host's articles are skipped for this run.
pub const BACKFILL_HOST_FAILURE_LIMIT: u32 = 3;

/// Process-global single-flight guard: concurrent triggers (e.g. the search
/// page loading twice) collapse into one running pass.
static RUNNING: AtomicBool = AtomicBool::new(false);

struct RunningGuard;

impl Drop for RunningGuard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

/// Result of one backfill run, surfaced to the UI.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BackfillReport {
    /// True when this call found a run already in flight and did nothing.
    pub already_running: bool,
    /// Articles whose website Markdown was fetched and cached.
    pub fetched: usize,
    /// Articles whose fetch/conversion failed (they stay candidates and
    /// are retried by a later run, still under the pacing limits).
    pub failed: usize,
    /// Articles queued for ChromaDB re-indexing.
    pub queued_reindex: usize,
    /// Hosts skipped after repeated failures (see the S contract above).
    pub hosts_skipped: usize,
    /// True when the batch was full — more backfill candidates remain, so
    /// the caller (search-page load) may trigger another pass later to
    /// keep working through the backlog under the same pacing limits.
    pub more_pending: bool,
    pub duration_ms: u128,
}

/// Refresh website Markdown for articles missing it and re-index them.
///
/// Never fails the whole run on a per-article error: one bad page is
/// logged and the pass moves on (subject to the per-host failure cutoff).
/// A failure of the final Chroma sync is also non-fatal — the queued
/// upserts simply drain on the next sync.
pub async fn backfill_website_markdown(
    repo: &Arc<dyn FeedItemRepository>,
    fetcher: &Arc<FeedFetcher>,
    chroma: &ChromaHolder,
) -> crate::error::Result<BackfillReport> {
    // Single-flight: a second trigger while one pass runs is a no-op.
    if RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Ok(BackfillReport {
            already_running: true,
            ..Default::default()
        });
    }
    // Cancellation and early returns must release the process-global guard.
    let _running = RunningGuard;
    run_backfill(repo, fetcher, chroma).await
}

async fn run_backfill(
    repo: &Arc<dyn FeedItemRepository>,
    fetcher: &Arc<FeedFetcher>,
    chroma: &ChromaHolder,
) -> crate::error::Result<BackfillReport> {
    let started = std::time::Instant::now();
    let mut report = BackfillReport::default();

    let candidates = repo
        .find_website_backfill_candidates(BACKFILL_BATCH_LIMIT)
        .await?;
    if candidates.is_empty() {
        report.duration_ms = started.elapsed().as_millis();
        return Ok(report);
    }
    // A full batch means the backlog isn't drained yet — tell the caller so
    // it can schedule another pass instead of assuming everything is done.
    report.more_pending = candidates.len() as i64 >= BACKFILL_BATCH_LIMIT;

    // Per-host consecutive failure counter (supporting rail — see above).
    let mut host_failures: HashMap<String, u32> = HashMap::new();
    let mut skipped_hosts: Vec<String> = Vec::new();
    // QPS limiter: the next website request may not START before this
    // instant. Set at each request start (not end), so the interval is
    // enforced between request starts — exactly what a queries-per-second
    // cap means. A slow fetch that outlasts the interval costs no extra
    // sleep.
    let min_interval = backfill_min_interval();
    let mut next_allowed = tokio::time::Instant::now();

    for (id, link) in candidates {
        let host = reqwest::Url::parse(&link)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default();

        // Skip the rest of a repeatedly failing host — don't keep poking a
        // site that is down or blocking us.
        if host_failures.get(&host).copied().unwrap_or(0) >= BACKFILL_HOST_FAILURE_LIMIT {
            if !skipped_hosts.contains(&host) {
                skipped_hosts.push(host.clone());
                report.hosts_skipped += 1;
                eprintln!(
                    "[markdown-backfill] skipping remaining items for host {} ({} consecutive failures)",
                    host, BACKFILL_HOST_FAILURE_LIMIT
                );
            }
            continue;
        }

        // QPS cap: wait out the remainder of the interval since the last
        // request start, if any.
        let now = tokio::time::Instant::now();
        if now < next_allowed {
            tokio::time::sleep(next_allowed - now).await;
        }

        // Record the next start time before awaiting the request. A slow
        // request that already exceeds the interval needs no extra sleep.
        next_allowed = tokio::time::Instant::now() + min_interval;

        match fetch_one(repo, fetcher, id, &link).await {
            Ok(()) => {
                host_failures.remove(&host);
                report.fetched += 1;
                // Queue a re-embed: the item was first indexed from its RSS
                // snippet; the fresh Markdown is the real body text.
                SyncState::queue_upsert(id).await;
                report.queued_reindex += 1;
            }
            Err(e) => {
                *host_failures.entry(host).or_insert(0) += 1;
                report.failed += 1;
                eprintln!("[markdown-backfill] item {} failed: {}", id, e);
            }
        }
    }

    // Re-index what we just refreshed so the articles are searchable now,
    // not just at the next sync. Failures leave the pending queue intact.
    if report.queued_reindex > 0 {
        if let Some(service) = chroma.get().await {
            match sync::incremental_sync(repo, &service).await {
                Ok(sync_report) => println!(
                    "[markdown-backfill] re-indexed {} items in {}ms",
                    sync_report.indexed, sync_report.duration_ms
                ),
                Err(e) => eprintln!("[markdown-backfill] re-index sync failed: {}", e),
            }
        } else {
            println!(
                "[markdown-backfill] ChromaDB unavailable — {} items stay queued for the next sync",
                report.queued_reindex
            );
        }
    }

    report.duration_ms = started.elapsed().as_millis();
    Ok(report)
}

/// Fetch one article page, convert it to Markdown, and cache it as the
/// item's website content. A single HTTP attempt — no retries.
async fn fetch_one(
    repo: &Arc<dyn FeedItemRepository>,
    fetcher: &Arc<FeedFetcher>,
    id: i64,
    link: &str,
) -> crate::error::Result<()> {
    let html = fetcher.fetch_website_content(link).await?;

    // HTML -> Markdown is CPU-bound; keep it off the async workers.
    let html = tokio::task::spawn_blocking(move || html_to_markdown_pipeline(&html))
        .await
        .map_err(|e| crate::error::AppError::Internal(format!("markdown task failed: {}", e)))??;

    if html.trim().is_empty() {
        return Err(crate::error::AppError::OperationFailed(
            "website yielded no extractable content".into(),
        ));
    }

    repo.update_content_md(id, &html, true).await?;
    Ok(())
}

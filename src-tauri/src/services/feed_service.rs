use std::sync::Arc;

use tokio::sync::Semaphore;

use serde::Serialize;

use crate::ai::activity::AiActivityStore;
use crate::chroma::ChromaHolder;
use crate::error::{AppError, Result};
use crate::feed::fetcher::FeedValidators;
use crate::feed::parser::parse_feed;
use crate::feed::FeedFetcher;
use crate::models::job::NewJob;
use crate::models::{FeedItem, Subscription};

use crate::ai::service::SharedAiService;
#[cfg(test)]
use crate::content_processor::html_to_markdown_pipeline;
#[cfg(test)]
use crate::models::NewFeedItem;
use crate::repositories::{FeedItemRepository, SubscriptionRepository};
use crate::services::tag_matcher::TagMatcher;

/// Summary of a batch fetch operation.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FetchSummary {
    pub total_subscriptions: usize,
    pub success_count: usize,
    pub total_items: usize,
    pub new_items: usize,
    pub errors: Vec<String>,
}

/// Business logic layer for feed items.
///
/// Orchestrates feed fetching, parsing, deduplication, and
/// the HTML-to-Markdown caching pipeline.
///
/// Optional dependencies (fetcher and sub_repo) are `None` by default.
/// The shared AI slot starts empty until a valid configuration is loaded or
/// saved; use the builder methods to wire in full fetch/classify capability.
pub struct FeedService {
    repo: Arc<dyn FeedItemRepository>,
    sub_repo: Option<Arc<dyn SubscriptionRepository>>,
    fetcher: Option<Arc<FeedFetcher>>,
    ai_service: SharedAiService,
    ai_activity: AiActivityStore,
    chroma_service: ChromaHolder,
    tag_matcher: Arc<TagMatcher>,
}

impl FeedService {
    /// Create a new FeedService with only the item repository.
    /// Fulfills basic CRUD needs. Call builder methods to add optional capabilities.
    pub fn new(repo: Arc<dyn FeedItemRepository>) -> Self {
        Self {
            repo,
            sub_repo: None,
            fetcher: None,
            ai_service: Arc::new(tokio::sync::RwLock::new(None)),
            ai_activity: AiActivityStore::new(),
            chroma_service: ChromaHolder::default(),
            tag_matcher: Arc::new(TagMatcher::local()),
        }
    }

    /// Attach a subscription repository (needed for fetch_all / refresh).
    pub fn with_subscription_repo(mut self, repo: Arc<dyn SubscriptionRepository>) -> Self {
        self.sub_repo = Some(repo);
        self
    }

    /// Attach an HTTP fetcher (needed for fetch_and_save methods).
    pub fn with_fetcher(mut self, fetcher: Arc<FeedFetcher>) -> Self {
        self.fetcher = Some(fetcher);
        self
    }

    /// Attach the shared AI service slot (enables automatic classification).
    /// The slot can be replaced when the user saves AI settings, so a restart
    /// is not required before the next feed refresh uses the new config.
    pub fn with_ai_service(mut self, ai_service: SharedAiService) -> Self {
        self.ai_service = ai_service;
        self
    }

    /// Attach the shared AI activity store used by the status bar.
    pub fn with_ai_activity(mut self, activity: AiActivityStore) -> Self {
        self.ai_activity = activity;
        self
    }

    /// Attach the ChromaDB holder (enables semantic search indexing).
    pub fn with_chroma_service(mut self, chroma: ChromaHolder) -> Self {
        self.chroma_service = chroma;
        self
    }

    /// Attach the shared tag matcher so automatic classification snaps
    /// generated names onto the catalog with the same settings as the UI.
    pub fn with_tag_matcher(mut self, tag_matcher: Arc<TagMatcher>) -> Self {
        self.tag_matcher = tag_matcher;
        self
    }

    // ------------------------------------------------------------------
    // Test helpers (only available in test builds)
    // ------------------------------------------------------------------

    #[cfg(test)]
    pub async fn create_item(&self, input: NewFeedItem) -> Result<FeedItem> {
        self.repo.create(input).await
    }

    #[cfg(test)]
    pub async fn get_item(&self, id: i64) -> Result<FeedItem> {
        self.repo.find_by_id(id).await
    }

    // ------------------------------------------------------------------
    // Feed Fetching
    // ------------------------------------------------------------------

    fn require_fetcher(&self) -> Result<&Arc<FeedFetcher>> {
        self.fetcher
            .as_ref()
            .ok_or_else(|| AppError::Internal("FeedFetcher not configured".into()))
    }

    /// Fetch a single feed, parse it, deduplicate, and save new items.
    pub async fn fetch_and_save_feed(&self, subscription: &Subscription) -> Result<Vec<FeedItem>> {
        let fetcher = self.require_fetcher()?;
        let sub_repo = self.sub_repo.clone();
        let chroma_configured = self.chroma_service.is_configured();
        fetch_parse_and_save(
            &self.repo,
            fetcher,
            sub_repo.as_ref(),
            subscription,
            chroma_configured,
        )
        .await
    }

    /// Fetch all subscriptions concurrently (semaphore-limited).
    pub async fn fetch_and_save_all_feeds(&self) -> FetchSummary {
        let sub_repo = match self.sub_repo.as_ref() {
            Some(r) => r,
            None => {
                return FetchSummary {
                    errors: vec!["SubscriptionRepository not configured".into()],
                    ..Default::default()
                };
            }
        };

        let subs = match sub_repo.find_all().await {
            Ok(s) => s,
            Err(e) => {
                return FetchSummary {
                    errors: vec![format!("Failed to load subscriptions: {}", e)],
                    ..Default::default()
                };
            }
        };

        let total = subs.len();
        if total == 0 {
            return FetchSummary::default();
        }

        let results = self.spawn_fetch_tasks(subs).await;

        let mut summary = FetchSummary {
            total_subscriptions: total,
            ..Default::default()
        };

        for result in results {
            match result {
                Ok(items) => {
                    summary.success_count += 1;
                    summary.total_items += items.len();
                    summary.new_items += items.len();
                }
                Err(e) => summary.errors.push(e),
            }
        }

        summary
    }

    /// Refresh specific subscriptions concurrently (same semaphore-limited
    /// pipeline as fetch_all). Results are returned in input order.
    pub async fn refresh_subscriptions(
        &self,
        ids: &[i64],
    ) -> Result<Vec<(i64, std::result::Result<Vec<FeedItem>, String>)>> {
        let sub_repo = self
            .sub_repo
            .as_ref()
            .ok_or_else(|| AppError::Internal("SubscriptionRepository not configured".into()))?;

        // Resolve subscriptions first (cheap local lookups, keeps input order)
        let mut subs: Vec<(i64, Subscription)> = Vec::with_capacity(ids.len());
        let mut out: Vec<(i64, std::result::Result<Vec<FeedItem>, String>)> = Vec::new();
        for &id in ids {
            match sub_repo.find_by_id(id).await {
                Ok(s) => subs.push((id, s)),
                Err(e) => out.push((id, Err(format!("Subscription not found: {}", e)))),
            }
        }

        let sub_ids: Vec<i64> = subs.iter().map(|(id, _)| *id).collect();
        let results = self
            .spawn_fetch_tasks(subs.into_iter().map(|(_, s)| s).collect())
            .await;

        // spawn_fetch_tasks returns results in the same order as its input
        out.extend(sub_ids.into_iter().zip(results));
        // Restore the original input order
        out.sort_by_key(|(id, _)| ids.iter().position(|i| i == id).unwrap_or(usize::MAX));
        Ok(out)
    }

    /// Spawn one semaphore-limited fetch task per subscription and collect
    /// the results in input order (join_all preserves order).
    async fn spawn_fetch_tasks(
        &self,
        subs: Vec<Subscription>,
    ) -> Vec<std::result::Result<Vec<FeedItem>, String>> {
        let semaphore = Arc::new(Semaphore::new(20));
        let mut handles = Vec::with_capacity(subs.len());

        for sub in subs {
            let sem = Arc::clone(&semaphore);
            let repo = self.repo.clone();
            let fetcher = self.fetcher.clone();
            let sub_repo = self.sub_repo.clone();
            let chroma_configured = self.chroma_service.is_configured();

            handles.push(tokio::spawn(async move {
                let result = async {
                    let _permit = sem.acquire().await;
                    let fetcher = fetcher
                        .as_ref()
                        .ok_or_else(|| "FeedFetcher not configured".to_string())?;
                    fetch_parse_and_save(&repo, fetcher, sub_repo.as_ref(), &sub, chroma_configured)
                        .await
                        .map_err(|e| e.to_string())
                }
                .await;
                result
            }));
        }

        futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| match r {
                Ok(res) => res,
                Err(e) => Err(format!("Task panicked: {}", e)),
            })
            .collect()
    }

    // ------------------------------------------------------------------
    // Website Content Caching
    // ------------------------------------------------------------------

    /// Cache website content as Markdown for a feed item.
    #[cfg(test)]
    pub async fn cache_website_content(&self, item_id: i64, raw_html: &str) -> Result<FeedItem> {
        let md = html_to_markdown_pipeline(raw_html)?;
        self.repo.update_content_md(item_id, &md, true).await
    }
}

// ----------------------------------------------------------------------
// Shared fetch pipeline (single implementation used by both the service
// method and the spawned per-subscription tasks)
// ----------------------------------------------------------------------

use crate::feed::parser::ensure_content_md;

/// Lazily fill `content_md` for an item that has raw HTML `content` but no
/// cached markdown. The conversion runs on a blocking worker because
/// `html2md::parse_html` is CPU-bound. Once cached, every display path
/// (host DOM text + iframe webview) routes through `marked` → `setSafeHtml`
/// and never sees raw RSS HTML.
///
/// No-op when `content_md` is already populated or `content` is missing.
/// Returns the (possibly updated) item so callers can re-fetch the row.
pub async fn ensure_content_md_for_item(
    repo: &Arc<dyn FeedItemRepository>,
    item_id: i64,
) -> Result<crate::models::FeedItem> {
    let item = repo.find_by_id(item_id).await?;
    if item.content_md.is_some() {
        return Ok(item);
    }
    let Some(html) = item.content.clone() else {
        return Ok(item);
    };
    let md = tokio::task::spawn_blocking(move || ensure_content_md(&html))
        .await
        .map_err(|e| crate::error::AppError::Internal(format!("markdown task failed: {}", e)))?;
    if md.is_empty() {
        return Ok(item);
    }
    repo.update_content_md(item_id, &md, false).await
}

/// Re-derive `content_md` from the item's raw RSS `content`, overwriting
/// whatever is cached (e.g. website markdown) and clearing
/// `is_website_content`. Used when a subscription leaves webview mode so the
/// Markdown view reverts to the RSS text instead of the cached website
/// content. When the item has no RSS content it just clears the website flag.
pub async fn revert_to_rss_markdown(
    repo: &Arc<dyn FeedItemRepository>,
    item_id: i64,
) -> Result<crate::models::FeedItem> {
    let item = repo.find_by_id(item_id).await?;
    let md = match item.content.clone() {
        Some(html) => tokio::task::spawn_blocking(move || ensure_content_md(&html))
            .await
            .map_err(|e| {
                crate::error::AppError::Internal(format!("markdown task failed: {}", e))
            })?,
        None => String::new(),
    };
    repo.reset_content_md(item_id, &md).await
}

/// Enrichment jobs that must follow a newly committed article.
///
/// The jobs deliberately carry no item id: they are written in the same
/// transaction as the article, so the repository fills in the id of the row it
/// just inserted. Passing a placeholder id here (0) made every job point at a
/// nonexistent article, which the worker then completed as "not found" —
/// classification, website caching, and indexing silently never ran.
pub(crate) fn jobs_for_new_article(
    subscription: &Subscription,
    link: Option<String>,
    chroma_configured: bool,
) -> Vec<NewJob> {
    // Indexing is skipped entirely when semantic search is switched off. The
    // watermark sync indexes the backlog if it is enabled later, so nothing is
    // lost by not queueing one job per article for a feature nobody uses.
    let mut jobs = Vec::new();
    if chroma_configured {
        jobs.push(NewJob::chroma_upsert(None));
    }
    if subscription.auto_classify {
        jobs.push(NewJob::classify(None));
    }
    if subscription.use_website {
        if let Some(url) = link {
            jobs.push(NewJob::website_markdown(None, url));
        }
    }
    jobs
}

/// Fetch one feed, parse, dedup against existing rows, and commit new items
/// together with the enrichment jobs that follow them.
///
/// This function deliberately stops after the commit. Classification, website
/// caching, and semantic indexing are slow and can fail independently of the
/// article; running them here made a refresh wait on all of them (and lose
/// the work on failure). The durable queue now owns them and the reader sees
/// new articles immediately.
async fn fetch_parse_and_save(
    repo: &Arc<dyn FeedItemRepository>,
    fetcher: &Arc<FeedFetcher>,
    sub_repo: Option<&Arc<dyn SubscriptionRepository>>,
    subscription: &Subscription,
    chroma_configured: bool,
) -> Result<Vec<FeedItem>> {
    let feed_url = subscription
        .rsshub_url
        .as_deref()
        .unwrap_or(&subscription.url);

    // Conditional GET: an unchanged feed answers 304, so quiet subscriptions
    // cost one small request instead of a full download and parse.
    let fetched = fetcher
        .fetch_feed_conditional(
            feed_url,
            FeedValidators {
                etag: subscription.http_etag.as_deref(),
                last_modified: subscription.http_last_modified.as_deref(),
            },
        )
        .await?;

    if fetched.final_url != feed_url {
        // Redirects are otherwise invisible; knowing the feed moved is the
        // difference between "the site changed" and "we kept asking the old
        // address".
        println!(
            "[feed] subscription {} redirected: {} -> {}",
            subscription.id, feed_url, fetched.final_url
        );
    }

    if let Some(repo) = sub_repo {
        if let Err(e) = repo
            .update_http_validators(
                subscription.id,
                fetched.etag.as_deref(),
                fetched.last_modified.as_deref(),
            )
            .await
        {
            // Validators are an optimization; failing to store them must not
            // fail the refresh.
            eprintln!(
                "Failed to store HTTP validators for subscription {}: {}",
                subscription.id, e
            );
        }
    }

    let Some(content) = fetched.body else {
        // 304 Not Modified — nothing to parse and nothing changed.
        return Ok(Vec::new());
    };
    let parsed = parse_feed(&content, subscription.id)?;

    // One lightweight query for in-memory O(1) dedup
    // (was: one full-content SELECT * per parsed item — the N+1 hot spot).
    let (existing_guids, existing_links) = repo.find_dedup_keys(subscription.id).await?;

    let mut saved = Vec::new();
    for item in parsed {
        let is_dup = item
            .guid
            .as_ref()
            .is_some_and(|g| existing_guids.contains(g))
            || item
                .link
                .as_ref()
                .is_some_and(|l| existing_links.contains(l));
        if is_dup {
            continue;
        }

        // Only jobs for work that is actually wanted: no AI config means no
        // classification job, no link means no website job. Chroma is
        // optional too, but the worker treats "disabled" as a no-op, so the
        // job is queued and drained when the index is enabled.
        let jobs = jobs_for_new_article(subscription, item.link.clone(), chroma_configured);

        match repo.create_with_jobs(item, jobs).await {
            Ok(created) => saved.push(created),
            // A concurrent refresh beat us to this row; benign, skip it.
            Err(AppError::Duplicate(_)) => continue,
            Err(e) => return Err(e),
        }
    }

    Ok(saved)
}

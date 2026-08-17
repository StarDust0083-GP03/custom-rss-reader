use std::sync::Arc;

use tokio::sync::Semaphore;

use serde::Serialize;

use crate::chroma::ChromaHolder;
use crate::error::{AppError, Result};
use crate::feed::parser::parse_feed;
use crate::feed::FeedFetcher;
use crate::models::{FeedItem, Subscription};

use crate::content_processor::html_to_markdown_pipeline;
#[cfg(test)]
use crate::models::NewFeedItem;
use crate::repositories::{FeedItemRepository, SubscriptionRepository};

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
/// Optional dependencies (fetcher, sub_repo, ai_service) are `None` by default.
/// Use the builder methods to wire them in for full fetch/classify capability.
pub struct FeedService {
    repo: Arc<dyn FeedItemRepository>,
    sub_repo: Option<Arc<dyn SubscriptionRepository>>,
    fetcher: Option<Arc<FeedFetcher>>,
    ai_service: Option<Arc<dyn crate::ai::service::AiService>>,
    chroma_service: ChromaHolder,
}

impl FeedService {
    /// Create a new FeedService with only the item repository.
    /// Fulfills basic CRUD needs. Call builder methods to add optional capabilities.
    pub fn new(repo: Arc<dyn FeedItemRepository>) -> Self {
        Self {
            repo,
            sub_repo: None,
            fetcher: None,
            ai_service: None,
            chroma_service: ChromaHolder::default(),
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

    /// Attach the ChromaDB holder (enables semantic search indexing).
    pub fn with_chroma_service(mut self, chroma: ChromaHolder) -> Self {
        self.chroma_service = chroma;
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

    #[cfg(test)]
    pub async fn get_items_by_subscription(&self, subscription_id: i64) -> Result<Vec<FeedItem>> {
        self.repo.find_by_subscription(subscription_id).await
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
        fetch_parse_and_save(
            &self.repo,
            fetcher,
            self.ai_service.as_ref(),
            &self.chroma_service,
            subscription,
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
            let ai_service = self.ai_service.clone();
            let chroma_service = self.chroma_service.clone();

            handles.push(tokio::spawn(async move {
                let result = async {
                    let _permit = sem.acquire().await;
                    let fetcher = fetcher
                        .as_ref()
                        .ok_or_else(|| "FeedFetcher not configured".to_string())?;
                    fetch_parse_and_save(
                        &repo,
                        fetcher,
                        ai_service.as_ref(),
                        &chroma_service,
                        &sub,
                    )
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

/// Fetch one feed, parse, dedup against existing rows, insert new items and
/// run per-item side effects (classification, Chroma indexing, website
/// content pre-caching).
async fn fetch_parse_and_save(
    repo: &Arc<dyn FeedItemRepository>,
    fetcher: &Arc<FeedFetcher>,
    ai_service: Option<&Arc<dyn crate::ai::service::AiService>>,
    chroma_service: &ChromaHolder,
    subscription: &Subscription,
) -> Result<Vec<FeedItem>> {
    let feed_url = subscription
        .rsshub_url
        .as_deref()
        .unwrap_or(&subscription.url);

    let content = fetcher.fetch_feed(feed_url).await?;
    let parsed = parse_feed(&content, subscription.id)?;

    // One lightweight query for in-memory O(1) dedup
    // (was: one full-content SELECT * per parsed item — the N+1 hot spot).
    let (existing_guids, existing_links) = repo.find_dedup_keys(subscription.id).await?;

        let mut saved = Vec::new();
    // Items pending auto-classification. Classification now runs in BATCHES
    // after the insert loop: one LLM call per ~20 articles instead of one
    // call per article — a 20x reduction in request count (and thus in
    // rate-limit pressure) during a bulk refresh.
    let mut pending_classify: Vec<FeedItem> = Vec::new();

    for item in parsed {
        let is_dup = item
            .guid
            .as_ref()
            .map_or(false, |g| existing_guids.contains(g))
            || item
                .link
                .as_ref()
                .map_or(false, |l| existing_links.contains(l));
        if is_dup {
            continue;
        }

        let feed_item = match repo.create(item).await {
            Ok(it) => it,
            // A concurrent refresh beat us to this row; benign, skip it.
            Err(AppError::Duplicate(_)) => continue,
            Err(e) => return Err(e),
        };

        // Index into ChromaDB if available (lazy-connects). A failure here
        // must NOT lose the item from the semantic index forever — queue it
        // for the next incremental sync (watermark + pending-upsert retry).
        if let Some(chroma) = chroma_service.get().await {
            if let Err(e) = chroma.index_item(&feed_item).await {
                eprintln!("ChromaDB indexing failed for item {}: {}", feed_item.id, e);
                crate::chroma::sync::SyncState::queue_upsert(feed_item.id);
            }
        }

        // Pre-cache website content for use_website subscriptions
        if subscription.use_website {
            precache_website_content(repo, fetcher, &feed_item).await;
        }

        if subscription.auto_classify {
            pending_classify.push(feed_item.clone());
        }
        saved.push(feed_item);
    }

    // Batch classification: titles only, one LLM call per
    // CLASSIFY_BATCH_SIZE articles.
    if let Some(ai) = ai_service {
        for chunk in pending_classify.chunks(crate::ai::CLASSIFY_BATCH_SIZE) {
            if let Err(e) = classify_batch_and_save(repo, ai.as_ref(), chunk).await {
                eprintln!("Batch classification failed ({} items): {}", chunk.len(), e);
            }
        }
    }

    Ok(saved)
}

/// Classify a batch of items in a single LLM call and persist the results.
///
/// Payload per article is the title only — neither description nor
/// `content` is ever sent.
async fn classify_batch_and_save(
    repo: &Arc<dyn FeedItemRepository>,
    ai: &dyn crate::ai::service::AiService,
    items: &[FeedItem],
) -> Result<()> {
    let entries: Vec<crate::ai::BatchClassifyEntry> = items
        .iter()
        .enumerate()
        .map(|(i, item)| crate::ai::BatchClassifyEntry {
            index: i,
            title: item.title.clone(),
        })
        .collect();

    let responses = ai.classify_batch(&entries).await?;

    for (item, response) in items.iter().zip(responses) {
        // Skip items the model left unclassified (empty tags AND no category)
        // so we don't wipe anything with a no-op write.
        if response.tags.is_empty() && response.category.is_none() {
            continue;
        }
        let tags_json =
            serde_json::to_string(&response.tags).unwrap_or_else(|_| "[]".to_string());
        let category = response.category.unwrap_or_default();
        if let Err(e) = repo.save_tags(item.id, &tags_json, &category).await {
            eprintln!("Failed to save tags for item {}: {}", item.id, e);
        }
    }
    Ok(())
}

/// Fetch the article's website page and cache its Markdown conversion.
/// Errors are logged, never propagated (this is a best-effort side effect).
async fn precache_website_content(
    repo: &Arc<dyn FeedItemRepository>,
    fetcher: &Arc<FeedFetcher>,
    item: &FeedItem,
) {
    let Some(ref link) = item.link else { return };

    let html = match fetcher.fetch_website_content(link).await {
        Ok(html) => html,
        Err(e) => {
            eprintln!("Failed to fetch website for item {}: {}", item.id, e);
            return;
        }
    };

    // HTML -> Markdown is CPU-bound; keep it off the async worker threads.
    let converted = tokio::task::spawn_blocking(move || html_to_markdown_pipeline(&html)).await;
    match converted {
        Ok(Ok(md)) => {
            if let Err(e) = repo.update_content_md(item.id, &md, true).await {
                eprintln!("Failed to cache website content for item {}: {}", item.id, e);
            }
        }
        Ok(Err(e)) => {
            eprintln!("Failed to convert website content for item {}: {}", item.id, e);
        }
        Err(e) => {
            eprintln!("Website conversion task failed for item {}: {}", item.id, e);
        }
    }
}

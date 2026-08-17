pub mod feed_item_repo;
pub mod subscription_repo;

use std::collections::HashSet;

use async_trait::async_trait;

use crate::error::Result;
use crate::models::{NewFeedItem, FeedItem, FeedItemSummary, NewSubscription, Subscription, UpdateSubscription};

/// Lightweight row for embedding-index pipelines (ChromaDB).
///
/// `description`/`content` are SQL-truncated (`substr(..., 1, 2000)`), which
/// is sufficient because the indexed document is itself truncated to 2000
/// units after joining — bytes beyond that can never influence the embedding.
/// This keeps a 500-row page bounded regardless of article size, unlike
/// [`FeedItem`] which carries the full text columns.
#[derive(Debug, Clone)]
pub struct IndexRow {
    pub id: i64,
    pub title: String,
    pub link: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
    pub category: Option<String>,
    pub description: Option<String>,
    pub content: Option<String>,
}

/// Repository trait for feed item data access.
///
/// Using a trait allows:
/// - Swapping implementations (production SQLite vs test mocks)
/// - Testing services without a real database
/// - Clear separation of data access from business logic
///
/// List-oriented queries return [`FeedItemSummary`] (no large text columns);
/// the full [`FeedItem`] is only loaded by id or for pipeline processing.
#[async_trait]
pub trait FeedItemRepository: Send + Sync {
    /// Create a new feed item.
    ///
    /// Insertion uses `ON CONFLICT (subscription_id, guid) DO NOTHING`; a
    /// conflicting row yields `AppError::Duplicate` (treat as "already
    /// exists", not as a hard failure).
    async fn create(&self, input: NewFeedItem) -> Result<FeedItem>;

    /// Find a feed item by its ID.
    /// Returns `AppError::NotFound` if it doesn't exist.
    async fn find_by_id(&self, id: i64) -> Result<FeedItem>;

    /// Fetch the dedup keys (guids and links) of all items of a subscription
    /// in a single lightweight query, for in-memory dedup during fetch.
    async fn find_dedup_keys(
        &self,
        subscription_id: i64,
    ) -> Result<(HashSet<String>, HashSet<String>)>;

    /// Fetch just the ids of all items of a subscription. Used to clean up
    /// external indexes (ChromaDB) before the rows are cascade-deleted.
    async fn find_ids_by_subscription(&self, subscription_id: i64) -> Result<Vec<i64>>;

    /// List full feed items for a subscription, ordered by published_at DESC.
    /// Heavy (includes content) — pipeline use only.
    async fn find_by_subscription(&self, subscription_id: i64) -> Result<Vec<FeedItem>>;

    /// Page through ALL full feed items (includes content). Pipeline use only
    /// (e.g. Chroma reindex); UI lists must use the summary queries.
    async fn find_all_full(&self, limit: i64, offset: i64) -> Result<Vec<FeedItem>>;

    /// Keyset page of lightweight [`IndexRow`]s with `id > after_id`, in
    /// ascending id order. Stable under concurrent inserts/deletes (unlike
    /// OFFSET paging) and memory-bounded (text columns are truncated).
    async fn find_index_page(&self, after_id: i64, limit: i64) -> Result<Vec<IndexRow>>;

    /// Fetch lightweight [`IndexRow`]s for the given ids. Missing ids are
    /// silently omitted. Used to drain the sync pending-upsert queue.
    async fn find_index_rows_by_ids(&self, ids: &[i64]) -> Result<Vec<IndexRow>>;

    /// The maximum feed-item id currently in the database (0 when empty).
    /// Used to validate the Chroma sync watermark after a DB reset.
    async fn max_item_id(&self) -> Result<i64>;

    /// Update the Markdown-cached content for a feed item.
    ///
    /// `from_website` distinguishes the two paths the cache can be filled
    /// from: `true` for website HTML (which also flips `is_website_content`
    /// to `1`), `false` for lazily converting RSS `content` on first read.
    /// Returns `AppError::NotFound` if the item doesn't exist.
    async fn update_content_md(
        &self,
        id: i64,
        content_md: &str,
        from_website: bool,
    ) -> Result<FeedItem>;

    /// Persist (or overwrite) the translation of a feed item.
    async fn update_translation(
        &self,
        item_id: i64,
        translated_title: Option<&str>,
        translated_content: &str,
    ) -> Result<FeedItem>;

    /// List feed item summaries with optional subscription filter and pagination.
    async fn find_all(
        &self,
        subscription_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FeedItemSummary>>;

    /// Search feed items by query on title, description, and content.
    /// LIKE wildcards in the query are escaped.
    async fn search(&self, query: &str, limit: i64) -> Result<Vec<FeedItemSummary>>;

    /// Fetch summaries for the given ids in a single query.
    ///
    /// The result order is unspecified (DB order); callers that need a
    /// specific order (e.g. Chroma similarity ranking) reorder afterwards.
    /// Ids that don't exist are silently omitted.
    async fn find_summaries_by_ids(&self, ids: &[i64]) -> Result<Vec<FeedItemSummary>>;

    /// Find feed items having exactly the given tag (matched via json_each).
    async fn find_by_tag(
        &self,
        tag: &str,
        subscription_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FeedItemSummary>>;

    /// Collect unique tags across feed items (computed in the database).
    async fn find_all_tags(&self, subscription_id: Option<i64>) -> Result<Vec<String>>;

    /// Mark a feed item as read or unread.
    async fn mark_read(&self, id: i64, is_read: bool) -> Result<FeedItem>;

    /// Mark all unread items as read, optionally scoped to a subscription.
    async fn mark_all_read(&self, subscription_id: Option<i64>) -> Result<()>;

    /// Mark every item as unread, optionally scoped to a subscription.
    async fn mark_all_unread(&self, subscription_id: Option<i64>) -> Result<()>;

    /// Set the favorite flag on every item, optionally scoped to a
    /// subscription (bulk "favorite all" / "unfavorite all").
    async fn mark_all_favorite(
        &self,
        subscription_id: Option<i64>,
        is_favorite: bool,
    ) -> Result<()>;

    /// Toggle the favorite flag atomically. Returns the new state.
    async fn toggle_favorite(&self, id: i64) -> Result<bool>;

    /// Toggle the read-later flag atomically. Returns the new state.
    async fn toggle_read_later(&self, id: i64) -> Result<bool>;

    /// Toggle the ignored flag atomically. Returns the new state.
    async fn toggle_ignored(&self, id: i64) -> Result<bool>;

    /// Get favorited feed item summaries.
    async fn get_favorites(&self, limit: i64, offset: i64) -> Result<Vec<FeedItemSummary>>;

    /// Get read-later feed item summaries.
    async fn get_read_later(&self, limit: i64, offset: i64) -> Result<Vec<FeedItemSummary>>;

    /// Get unread feed item summaries, optionally filtered by subscription.
    async fn get_unread(
        &self,
        subscription_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FeedItemSummary>>;

    /// Get today's feed item summaries (local timezone day boundaries),
    /// optionally filtered by subscription and/or unread only.
    async fn get_today_items(
        &self,
        subscription_id: Option<i64>,
        unread_only: bool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FeedItemSummary>>;

    /// Save tags and category for a feed item.
    async fn save_tags(&self, item_id: i64, tags: &str, category: &str) -> Result<FeedItem>;
}

/// Repository trait for subscription data access.
#[async_trait]
pub trait SubscriptionRepository: Send + Sync {
    /// Create a new subscription after validating input.
    async fn create(&self, input: NewSubscription) -> Result<Subscription>;

    /// Find a subscription by its ID.
    /// Returns `AppError::NotFound` if it doesn't exist.
    async fn find_by_id(&self, id: i64) -> Result<Subscription>;

    /// List all subscriptions ordered by title.
    async fn find_all(&self) -> Result<Vec<Subscription>>;

    /// Update an existing subscription.
    /// - `None` (field absent) leaves the column unchanged
    /// - `Some(None)` (explicit null) clears the column to NULL
    /// - `Some(Some(v))` sets a new value
    /// Returns `AppError::NotFound` if the subscription doesn't exist.
    async fn update(&self, id: i64, input: UpdateSubscription) -> Result<Subscription>;

    /// Delete a subscription by ID.
    /// Returns `AppError::NotFound` if the subscription doesn't exist.
    async fn delete(&self, id: i64) -> Result<()>;

    /// Check whether a subscription with the given URL already exists.
    async fn exists_by_url(&self, url: &str) -> Result<bool>;

    /// Toggle a boolean field on a subscription.
    /// Returns the updated subscription.
    async fn toggle_use_website(&self, id: i64) -> Result<Subscription>;

    /// Toggle auto_classify on a subscription.
    /// Returns the updated subscription.
    async fn toggle_auto_classify(&self, id: i64) -> Result<Subscription>;
}

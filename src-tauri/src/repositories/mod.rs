pub mod feed_item_repo;
pub mod job_repo;
pub mod subscription_repo;

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::models::{
    FeedItem, FeedItemSummary, NewFeedItem, NewJob, NewSubscription, Subscription,
    UpdateSubscription,
};

/// Lightweight row for embedding-index pipelines (ChromaDB).
///
/// `description`/`content` are SQL-truncated (`substr(..., 1, 2000)`), which
/// is sufficient because the indexed document is itself truncated to 2000
/// units after joining — bytes beyond that can never influence the embedding.
/// This keeps a 500-row page bounded regardless of article size, unlike
/// [`FeedItem`] which carries the full text columns.
///
/// `content` prefers `content_md` over the raw RSS `content`: for
/// website-mode subscriptions the RSS text is often just a teaser while the
/// cached Markdown holds the full article (and for plain RSS items the lazy
/// Markdown conversion is textually equivalent), so the coalesced column is
/// never worse and frequently much richer.
#[derive(Debug, Clone)]
pub struct IndexRow {
    pub id: i64,
    pub title: String,
    pub link: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
    pub description: Option<String>,
    pub content: Option<String>,
}

/// One catalog tag and its existing usage/synonyms.
#[derive(Debug, Clone, Serialize)]
pub struct TagCatalogEntry {
    pub name: String,
    pub usage_count: i64,
    /// Names folded into this one, so the vocabulary reads as one entry per
    /// subject instead of one per spelling.
    pub aliases: Vec<String>,
}

/// One entry of the topic navigation catalog. `id` is the stable identity:
/// renaming a topic keeps its slot and its colour.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TopicCategory {
    pub id: i64,
    pub label: String,
    pub definition: String,
    pub sort_order: i64,
}

/// Where one tag belongs, or why it navigates nowhere.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TopicAssignment {
    pub tag_name: String,
    /// Set only when `state == "assigned"`.
    pub category_id: Option<i64>,
    pub state: String,
    pub source: String,
}

/// Source data behind the community map, so the view can say how much of the
/// library it is actually describing.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TagOverviewCoverage {
    pub total_items: i64,
    pub tagged_items: i64,
    /// Articles whose stored tag column is not readable JSON. They are left
    /// out of the map and reported instead of silently counting as empty.
    pub unreadable_items: i64,
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
    #[allow(dead_code)] // exercised by tests and kept for direct inserts
    async fn create(&self, input: NewFeedItem) -> Result<FeedItem>;

    /// Insert an item and its follow-up enrichment jobs in ONE transaction.
    ///
    /// Ingest must commit the article before any classification, website
    /// fetch, or embedding runs: those are slow, failure-prone, and
    /// replaceable, while the article is not. Enqueueing in the same
    /// transaction also means a crash can never leave an article with no
    /// record that enrichment is still owed.
    async fn create_with_jobs(&self, input: NewFeedItem, jobs: Vec<NewJob>) -> Result<FeedItem>;

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

    /// Stable identity generated inside this SQLite database. Replacing the
    /// database produces a new id so external indexes cannot reuse an old
    /// watermark against unrelated rows.
    async fn database_id(&self) -> Result<String>;

    /// Find items that should have website Markdown cached but don't:
    /// their subscription has `use_website` enabled and they carry a link,
    /// yet `content_md` is missing/empty or did not come from the website
    /// (`is_website_content = 0`). These are typically articles imported
    /// from the feed's history before website mode was enabled, or whose
    /// fetch-time website pre-cache failed. Returned newest-first so a
    /// batched backfill refreshes the most relevant articles first.
    async fn find_website_backfill_candidates(&self, limit: i64) -> Result<Vec<(i64, String)>>;

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

    /// Overwrite the Markdown cache back to the RSS content source: sets
    /// `content_md` AND clears `is_website_content`. Used when a
    /// subscription leaves webview mode so a cached website markdown is
    /// replaced by its RSS text. Returns `AppError::NotFound` if missing.
    async fn reset_content_md(&self, id: i64, content_md: &str) -> Result<FeedItem>;

    /// Persist (or overwrite) the translation of a feed item.
    /// Store a translation together with the identity of what produced it.
    ///
    /// `source_hash`/`model`/`prompt_version` are what make a cached
    /// translation trustworthy: a result is only reused when all three match
    /// the article and configuration asking for it. Without them, editing an
    /// article (or switching models) silently served the old text as if it
    /// described the new source.
    async fn update_translation(
        &self,
        item_id: i64,
        translated_title: Option<&str>,
        translated_content: &str,
        source_hash: &str,
        model: &str,
        prompt_version: i64,
    ) -> Result<FeedItem>;

    /// Record which source text a stored translation came from, without
    /// rewriting the translation or its timestamp.
    ///
    /// Used for rows that carry a translation but no provenance: written by a
    /// build that predates validity tracking, or by a downgraded build after
    /// the upgrade. Adopting the *current* stored source is the same one-time
    /// decision migration v10 makes; after it, edits are detected normally.
    async fn adopt_translation_provenance(&self, item_id: i64, source_hash: &str) -> Result<()>;

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

    /// Collect unique tags used by feed items (computed in the database).
    async fn find_all_tags(&self, subscription_id: Option<i64>) -> Result<Vec<String>>;

    /// List every active canonical tag, including unused manually-created tags.
    async fn find_tag_catalog(&self) -> Result<Vec<TagCatalogEntry>>;

    /// Every name the vocabulary knows. This is what the classifier is offered
    /// to reuse, so a word the library already has does not come back spelled
    /// six ways.
    async fn find_vocabulary_names(&self) -> Result<Vec<String>>;

    /// The topic navigation catalog.
    async fn find_topic_categories(&self) -> Result<Vec<TopicCategory>>;

    /// Store model proposals for later review, keyed for cheap reuse.
    async fn save_topic_suggestions(&self, entries: &[(String, String, String)]) -> Result<()>;

    /// Proposals already cached for these names, with the key they were made
    /// under, so a caller can tell a reusable proposal from a stale one.
    async fn find_topic_suggestions(
        &self,
        names: &[String],
    ) -> Result<HashMap<String, (String, String)>>;

    /// Every decided word and where it goes.
    async fn find_topic_assignments(&self) -> Result<Vec<TopicAssignment>>;

    /// Write the submitted catalog and assignments in ONE transaction. The
    /// assignment table is replaced wholesale, which is what the workspace
    /// edits; a caller that has not seen the current state is rejected before
    /// reaching here by the expected-hash check.
    async fn replace_topic_state(
        &self,
        categories: &[TopicCategory],
        assignments: &[TopicAssignment],
    ) -> Result<()>;

    /// Article counts per raw tag (pre-alias names), scoped by subscription.
    async fn find_raw_tag_usage(&self, subscription_id: Option<i64>) -> Result<HashMap<String, i64>>;

    /// Co-occurrence over raw tags: `(a, b, shared articles)` with `a < b`.
    async fn find_raw_tag_cooccurrence(
        &self,
        subscription_id: Option<i64>,
    ) -> Result<Vec<(String, String, i64)>>;

    /// Raw `(article_id, tag)` rows for one scope. The caller can build one
    /// article-set index and answer several community count queries without
    /// rescanning `feed_items` for every community.
    async fn find_raw_tag_items(&self, subscription_id: Option<i64>) -> Result<Vec<(i64, String)>>;

    /// How much of the scope carries readable tags at all.
    async fn tag_overview_coverage(
        &self,
        subscription_id: Option<i64>,
    ) -> Result<TagOverviewCoverage>;

    /// Create an unused canonical tag.
    async fn create_tag(&self, name: &str) -> Result<()>;

    /// List names that a user has removed and blocked from future writes.
    async fn find_blocked_tags(&self) -> Result<Vec<String>>;

    /// Rename a canonical tag and preserve the old name as an alias.
    async fn rename_tag(&self, old_name: &str, new_name: &str) -> Result<()>;

    /// Map several canonical tags to the selected canonical head.
    async fn merge_tags(&self, canonical_name: &str, members: &[String]) -> Result<()>;

    /// Remove a tag from all articles and block its name and aliases.
    async fn delete_tag(&self, name: &str) -> Result<()>;

    /// Restore a blocked name as an unused canonical tag.
    async fn restore_tag(&self, name: &str) -> Result<()>;

    /// Record that `alias` should resolve to the existing canonical tag
    /// `canonical_name` on future writes. Used when a generated name is
    /// matched onto the catalog. No-op if the alias is already recorded.
    async fn add_tag_alias(&self, alias: &str, canonical_name: &str) -> Result<()>;

    /// Drop a recorded mapping so `alias` becomes an independent name again.
    /// Catalog tags still missing an LLM definition, alphabetical.
    async fn find_tags_missing_explanation(&self, limit: i64) -> Result<Vec<String>>;

    /// `(catalog tags, explained tags, indexed tags)` for the dictionary UI.
    async fn tag_dictionary_status(&self) -> Result<(i64, i64, i64)>;

    /// Persist definitions written by the LLM.
    ///
    /// Rewriting a definition also clears its stored vector, because a vector
    /// computed from the previous text no longer describes the tag.
    async fn save_tag_explanations(
        &self,
        entries: &[(String, String)],
        prompt_version: i64,
    ) -> Result<()>;

    /// Every defined tag as `(name, explanation)`, alphabetical.
    async fn find_tag_explanations(&self) -> Result<Vec<(String, String)>>;

    /// Store dictionary vectors for one encoder identity.
    async fn save_tag_embeddings(
        &self,
        key: &str,
        entries: &[(String, Vec<f32>)],
    ) -> Result<()>;

    /// Dictionary vectors produced by `key`, keyed by tag name.
    ///
    /// A different key yields an empty map: vectors from another model or
    /// another input format must never be mixed into one comparison.
    async fn find_tag_embeddings(&self, key: &str) -> Result<HashMap<String, Vec<f32>>>;

    /// Tag pairs seen on the same article, with the number of shared articles.
    ///
    /// The raw material for community detection: unlike name similarity, this
    /// says which tags the reader's own library actually treats as related.
    /// Mark a feed item as read or unread.
    async fn mark_read(&self, id: i64, is_read: bool) -> Result<FeedItem>;

    /// Mark all unread items as read, optionally scoped to a subscription.
    async fn mark_all_read(&self, subscription_id: Option<i64>) -> Result<()>;

    /// Toggle the favorite flag atomically. Returns the new state.
    async fn toggle_favorite(&self, id: i64) -> Result<bool>;

    /// Toggle the read-later flag atomically. Returns the new state.
    async fn toggle_read_later(&self, id: i64) -> Result<bool>;

    /// Toggle the ignored flag atomically. Returns the new state.
    async fn toggle_ignored(&self, id: i64) -> Result<bool>;

    /// Get favorited feed item summaries, optionally scoped to a subscription.
    async fn get_favorites(
        &self,
        subscription_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FeedItemSummary>>;

    /// Get read-later feed item summaries, optionally scoped to a subscription.
    async fn get_read_later(
        &self,
        subscription_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FeedItemSummary>>;

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
    async fn save_tags(&self, item_id: i64, tags: &str) -> Result<FeedItem>;}

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

    /// Persist the HTTP validators seen on the last successful fetch.
    ///
    /// Passing `None` clears them (the server stopped sending them, and a
    /// stale `If-None-Match` would make every fetch answer 304).
    async fn update_http_validators(
        &self,
        id: i64,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<()>;
}

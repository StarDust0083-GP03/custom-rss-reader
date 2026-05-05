pub mod feed_item_repo;
pub mod subscription_repo;

use async_trait::async_trait;

use crate::error::Result;
use crate::models::{NewFeedItem, FeedItem, NewSubscription, Subscription, UpdateSubscription};

/// Repository trait for subscription data access.
/// Repository trait for feed item data access.
#[async_trait]
pub trait FeedItemRepository: Send + Sync {
    /// Create a new feed item.
    async fn create(&self, input: NewFeedItem) -> Result<FeedItem>;

    /// Find a feed item by its ID.
    /// Returns `AppError::NotFound` if it doesn't exist.
    async fn find_by_id(&self, id: i64) -> Result<FeedItem>;

    /// List all feed items for a subscription, ordered by published_at DESC.
    async fn find_by_subscription(&self, subscription_id: i64) -> Result<Vec<FeedItem>>;

    /// Update the Markdown-cached content for a feed item.
    /// Returns `AppError::NotFound` if the item doesn't exist.
    async fn update_content_md(&self, id: i64, content_md: &str) -> Result<FeedItem>;

    /// List feed items with optional subscription filter and pagination.
    async fn find_all(
        &self,
        subscription_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FeedItem>>;

    /// Search feed items by query on title, description, and content.
    async fn search(&self, query: &str, limit: i64) -> Result<Vec<FeedItem>>;

    /// Find feed items by tag (searches within the tags JSON string).
    async fn find_by_tag(
        &self,
        tag: &str,
        subscription_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FeedItem>>;

    /// Collect unique tags across feed items.
    async fn find_all_tags(&self, subscription_id: Option<i64>) -> Result<Vec<String>>;

    /// Mark a feed item as read or unread.
    async fn mark_read(&self, id: i64, is_read: bool) -> Result<FeedItem>;

    /// Mark all items for a subscription as read.
    /// Pass `None` to mark all items across all subscriptions as read.
    async fn mark_all_read(&self, subscription_id: Option<i64>) -> Result<()>;

    /// Toggle the favorite flag. Returns the new state.
    async fn toggle_favorite(&self, id: i64) -> Result<bool>;

    /// Toggle the read-later flag. Returns the new state.
    async fn toggle_read_later(&self, id: i64) -> Result<bool>;

    /// Toggle the ignored flag. Returns the new state.
    async fn toggle_ignored(&self, id: i64) -> Result<bool>;

    /// Get all favorited feed items.
    async fn get_favorites(&self, limit: i64, offset: i64) -> Result<Vec<FeedItem>>;

    /// Get all read-later feed items.
    async fn get_read_later(&self, limit: i64, offset: i64) -> Result<Vec<FeedItem>>;

    /// Get unread feed items, optionally filtered by subscription.
    async fn get_unread(
        &self,
        subscription_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FeedItem>>;

    /// Get today's feed items, optionally filtered by subscription and/or unread only.
    async fn get_today_items(
        &self,
        subscription_id: Option<i64>,
        unread_only: bool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FeedItem>>;

    /// Save tags and category for a feed item.
    async fn save_tags(&self, item_id: i64, tags: &str, category: &str) -> Result<FeedItem>;
}
/// Using a trait allows:
/// - Swapping implementations (production SQLite vs test mocks)
/// - Testing services without a real database
/// - Clear separation of data access from business logic
#[async_trait]
pub trait SubscriptionRepository: Send + Sync {
    /// Create a new subscription after validating input.
    async fn create(&self, input: NewSubscription) -> Result<Subscription>;

    /// Find a subscription by its ID.
    /// Returns `AppError::NotFound` if it doesn't exist.
    async fn find_by_id(&self, id: i64) -> Result<Subscription>;

    /// List all subscriptions ordered by title.
    async fn find_all(&self) -> Result<Vec<Subscription>>;

    /// Update an existing subscription. `None` fields in the input
    /// are left unchanged (COALESCE semantics).
    /// Returns `AppError::NotFound` if the subscription doesn't exist.
    async fn update(&self, id: i64, input: UpdateSubscription) -> Result<Subscription>;

    /// Delete a subscription by ID.
    /// Returns `AppError::NotFound` if it doesn't exist.
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

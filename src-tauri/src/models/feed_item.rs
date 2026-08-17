use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Domain model for a feed item (article).
///
/// `content_md` stores the Markdown-cached version of the HTML content,
/// produced by the `content_processor` pipeline (extract → convert).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedItem {
    pub id: i64,
    pub subscription_id: i64,
    pub guid: Option<String>,
    pub title: String,
    pub link: Option<String>,
    pub content: Option<String>,
    /// Markdown-cached version of the content (produced by html_to_markdown_pipeline).
    pub content_md: Option<String>,
    pub description: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub fetched_at: DateTime<Utc>,
    pub is_website_content: bool,
    pub is_read: bool,
    pub is_favorite: bool,
    pub is_read_later: bool,
    pub is_ignored: bool,
    pub tags: Option<String>,
    pub category: Option<String>,
    pub translated_title: Option<String>,
    pub translated_content: Option<String>,
    pub translated_at: Option<DateTime<Utc>>,
}

/// Lightweight projection of [`FeedItem`] for list views.
///
/// Omits the large payload columns (`content`, `content_md`,
/// `translated_content`, `guid`) so list queries stay cheap to transfer over
/// IPC. The full item is loaded on demand via `get_item(id)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedItemSummary {
    pub id: i64,
    pub subscription_id: i64,
    pub title: String,
    pub link: Option<String>,
    pub description: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub fetched_at: DateTime<Utc>,
    pub is_website_content: bool,
    pub is_read: bool,
    pub is_favorite: bool,
    pub is_read_later: bool,
    pub is_ignored: bool,
    pub tags: Option<String>,
    pub category: Option<String>,
    pub translated_title: Option<String>,
    /// Whether a translated content exists (without transferring it).
    pub has_translation: bool,
}

/// Input for creating a new feed item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewFeedItem {    pub subscription_id: i64,
    pub guid: Option<String>,
    pub title: String,
    pub link: Option<String>,
    pub content: Option<String>,
    pub content_md: Option<String>,
    pub description: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub is_website_content: bool,
    pub is_read: bool,
    pub is_favorite: bool,
    pub is_read_later: bool,
    pub is_ignored: bool,
    pub tags: Option<String>,
    pub category: Option<String>,
    pub translated_title: Option<String>,
    pub translated_content: Option<String>,
    pub translated_at: Option<DateTime<Utc>>,
}

impl Default for NewFeedItem {
    fn default() -> Self {
        Self {
            subscription_id: 0,
            guid: None,
            title: String::new(),
            link: None,
            content: None,
            content_md: None,
            description: None,
            author: None,
            published_at: None,
            is_website_content: false,
            is_read: false,
            is_favorite: false,
            is_read_later: false,
            is_ignored: false,
            tags: None,
            category: None,
            translated_title: None,
            translated_content: None,
            translated_at: None,
        }
    }
}

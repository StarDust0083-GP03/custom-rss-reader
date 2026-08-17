use tauri::State;

use crate::error::Result;
use crate::models::{FeedItem, FeedItemSummary};

use super::AppState;

/// Clamp `limit` to a sane upper bound. SQLite's `LIMIT -1` means "unlimited"
/// and the frontend must not be able to accidentally pull the whole table.
fn clamp_limit(limit: Option<i64>, default: i64, max: i64) -> i64 {
    limit.unwrap_or(default).clamp(1, max)
}

fn clamp_offset(offset: Option<i64>) -> i64 {
    offset.unwrap_or(0).max(0)
}

// ---- Item queries ----

#[tauri::command]
pub async fn get_items(
    state: State<'_, AppState>,
    subscription_id: Option<i64>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<FeedItemSummary>> {
    state
        .feed_repo
        .find_all(subscription_id, clamp_limit(limit, 50, 200), clamp_offset(offset))
        .await
}

#[tauri::command]
pub async fn search_items(
    state: State<'_, AppState>,
    query: String,
    limit: Option<i64>,
) -> Result<Vec<FeedItemSummary>> {
    state
        .feed_repo
        .search(&query, clamp_limit(limit, 50, 200))
        .await
}

#[tauri::command]
pub async fn get_item(state: State<'_, AppState>, id: i64) -> Result<FeedItem> {
    // Lazily fill `content_md` from raw `content` on first read so the
    // frontend can route through `marked` → `setSafeHtml` for both display
    // paths (host DOM text + iframe webview) without ever seeing raw HTML.
    crate::services::feed_service::ensure_content_md_for_item(&state.feed_repo, id).await
}

#[tauri::command]
pub async fn get_items_by_subscription(
    state: State<'_, AppState>,
    subscription_id: i64,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<FeedItemSummary>> {
    state
        .feed_repo
        .find_all(
            Some(subscription_id),
            clamp_limit(limit, 50, 200),
            clamp_offset(offset),
        )
        .await
}

#[tauri::command]
pub async fn get_items_by_tag(
    state: State<'_, AppState>,
    tag: String,
    subscription_id: Option<i64>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<FeedItemSummary>> {
    state
        .feed_repo
        .find_by_tag(
            &tag,
            subscription_id,
            clamp_limit(limit, 50, 200),
            clamp_offset(offset),
        )
        .await
}

#[tauri::command]
pub async fn get_all_tags(
    state: State<'_, AppState>,
    subscription_id: Option<i64>,
) -> Result<Vec<String>> {
    state.feed_repo.find_all_tags(subscription_id).await
}

// ---- Item actions ----

#[tauri::command]
pub async fn mark_item_read(
    state: State<'_, AppState>,
    item_id: i64,
    is_read: bool,
) -> Result<FeedItem> {
    state.feed_repo.mark_read(item_id, is_read).await
}

#[tauri::command]
pub async fn mark_all_read(
    state: State<'_, AppState>,
    subscription_id: Option<i64>,
) -> Result<()> {
    state.feed_repo.mark_all_read(subscription_id).await
}

#[tauri::command]
pub async fn toggle_favorite(
    state: State<'_, AppState>,
    item_id: i64,
) -> Result<bool> {
    state.feed_repo.toggle_favorite(item_id).await
}

#[tauri::command]
pub async fn toggle_read_later(
    state: State<'_, AppState>,
    item_id: i64,
) -> Result<bool> {
    state.feed_repo.toggle_read_later(item_id).await
}

#[tauri::command]
pub async fn toggle_ignored(
    state: State<'_, AppState>,
    item_id: i64,
) -> Result<bool> {
    state.feed_repo.toggle_ignored(item_id).await
}

#[tauri::command]
pub async fn get_favorites(
    state: State<'_, AppState>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<FeedItemSummary>> {
    state
        .feed_repo
        .get_favorites(clamp_limit(limit, 50, 200), clamp_offset(offset))
        .await
}

#[tauri::command]
pub async fn get_read_later(
    state: State<'_, AppState>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<FeedItemSummary>> {
    state
        .feed_repo
        .get_read_later(clamp_limit(limit, 50, 200), clamp_offset(offset))
        .await
}

#[tauri::command]
pub async fn get_unread(
    state: State<'_, AppState>,
    subscription_id: Option<i64>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<FeedItemSummary>> {
    state
        .feed_repo
        .get_unread(
            subscription_id,
            clamp_limit(limit, 50, 200),
            clamp_offset(offset),
        )
        .await
}

#[tauri::command]
pub async fn get_today_items(
    state: State<'_, AppState>,
    subscription_id: Option<i64>,
    unread_only: Option<bool>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<FeedItemSummary>> {
    state
        .feed_repo
        .get_today_items(
            subscription_id,
            unread_only.unwrap_or(false),
            clamp_limit(limit, 50, 200),
            clamp_offset(offset),
        )
        .await
}

#[tauri::command]
pub async fn save_item_tags(
    state: State<'_, AppState>,
    item_id: i64,
    tags: String,
    category: String,
) -> Result<FeedItem> {
    state.feed_repo.save_tags(item_id, &tags, &category).await
}
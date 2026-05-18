use tauri::State;

use crate::error::Result;
use crate::models::FeedItem;

use super::AppState;

// ---- Item queries ----

#[tauri::command]
pub async fn get_items(
    state: State<'_, AppState>,
    subscription_id: Option<i64>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<FeedItem>> {
    state
        .feed_repo
        .find_all(subscription_id, limit.unwrap_or(50), offset.unwrap_or(0))
        .await
}

#[tauri::command]
pub async fn search_items(
    state: State<'_, AppState>,
    query: String,
    limit: Option<i64>,
) -> Result<Vec<FeedItem>> {
    state.feed_repo.search(&query, limit.unwrap_or(50)).await
}

#[tauri::command]
pub async fn get_item(state: State<'_, AppState>, id: i64) -> Result<FeedItem> {
    state.feed_repo.find_by_id(id).await
}

#[tauri::command]
pub async fn get_items_by_subscription(
    state: State<'_, AppState>,
    subscription_id: i64,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<FeedItem>> {
    state
        .feed_repo
        .find_all(Some(subscription_id), limit.unwrap_or(50), offset.unwrap_or(0))
        .await
}

#[tauri::command]
pub async fn get_items_by_tag(
    state: State<'_, AppState>,
    tag: String,
    subscription_id: Option<i64>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<FeedItem>> {
    state
        .feed_repo
        .find_by_tag(&tag, subscription_id, limit.unwrap_or(50), offset.unwrap_or(0))
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
) -> Result<Vec<FeedItem>> {
    state
        .feed_repo
        .get_favorites(limit.unwrap_or(50), offset.unwrap_or(0))
        .await
}

#[tauri::command]
pub async fn get_read_later(
    state: State<'_, AppState>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<FeedItem>> {
    state
        .feed_repo
        .get_read_later(limit.unwrap_or(50), offset.unwrap_or(0))
        .await
}

#[tauri::command]
pub async fn get_unread(
    state: State<'_, AppState>,
    subscription_id: Option<i64>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<FeedItem>> {
    state
        .feed_repo
        .get_unread(subscription_id, limit.unwrap_or(50), offset.unwrap_or(0))
        .await
}

#[tauri::command]
pub async fn get_today_items(
    state: State<'_, AppState>,
    subscription_id: Option<i64>,
    unread_only: Option<bool>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<FeedItem>> {
    state
        .feed_repo
        .get_today_items(subscription_id, unread_only.unwrap_or(false), limit.unwrap_or(50), offset.unwrap_or(0))
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

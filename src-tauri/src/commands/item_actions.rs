use tauri::State;
use sqlx::SqlitePool;

#[tauri::command]
pub async fn mark_item_read(
    pool: State<'_, SqlitePool>,
    item_id: i64,
    is_read: bool,
) -> Result<(), String> {
    sqlx::query(
        "UPDATE feed_items SET is_read = $1 WHERE id = $2"
    )
    .bind(is_read)
    .bind(item_id)
    .execute(pool.inner())
    .await
    .map_err(|e| format!("Failed to mark item as read: {}", e))?;

    Ok(())
}

#[tauri::command]
pub async fn mark_all_read(
    pool: State<'_, SqlitePool>,
    subscription_id: Option<i64>,
) -> Result<(), String> {
    let query = if subscription_id.is_some() {
        "UPDATE feed_items SET is_read = 1 WHERE subscription_id = $1"
    } else {
        "UPDATE feed_items SET is_read = 1 WHERE is_read = 0"
    };

    let mut q = sqlx::query(query);

    if let Some(sub_id) = subscription_id {
        q = q.bind(sub_id);
    }

    q.execute(pool.inner())
        .await
        .map_err(|e| format!("Failed to mark all as read: {}", e))?;

    Ok(())
}

#[tauri::command]
pub async fn toggle_favorite(
    pool: State<'_, SqlitePool>,
    item_id: i64,
) -> Result<bool, String> {
    let item: (bool,) = sqlx::query_as(
        "SELECT is_favorite FROM feed_items WHERE id = $1"
    )
    .bind(item_id)
    .fetch_one(pool.inner())
    .await
    .map_err(|e| format!("Failed to get item: {}", e))?;

    let new_value = !item.0;

    sqlx::query(
        "UPDATE feed_items SET is_favorite = $1 WHERE id = $2"
    )
    .bind(new_value)
    .bind(item_id)
    .execute(pool.inner())
    .await
    .map_err(|e| format!("Failed to toggle favorite: {}", e))?;

    Ok(new_value)
}

#[tauri::command]
pub async fn toggle_read_later(
    pool: State<'_, SqlitePool>,
    item_id: i64,
) -> Result<bool, String> {
    let item: (bool,) = sqlx::query_as(
        "SELECT is_read_later FROM feed_items WHERE id = $1"
    )
    .bind(item_id)
    .fetch_one(pool.inner())
    .await
    .map_err(|e| format!("Failed to get item: {}", e))?;

    let new_value = !item.0;

    sqlx::query(
        "UPDATE feed_items SET is_read_later = $1 WHERE id = $2"
    )
    .bind(new_value)
    .bind(item_id)
    .execute(pool.inner())
    .await
    .map_err(|e| format!("Failed to toggle read later: {}", e))?;

    Ok(new_value)
}

#[tauri::command]
pub async fn get_favorites(
    pool: State<'_, SqlitePool>,
    limit: Option<i32>,
    offset: Option<i32>,
) -> Result<Vec<crate::database::schema::FeedItem>, String> {
    let limit = limit.unwrap_or(50);
    let offset = offset.unwrap_or(0);

    let items = sqlx::query_as::<_, crate::database::schema::FeedItem>(
        "SELECT * FROM feed_items WHERE is_favorite = 1 ORDER BY published_at DESC LIMIT $1 OFFSET $2"
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(pool.inner())
    .await
    .map_err(|e| format!("Failed to get favorites: {}", e))?;

    Ok(items)
}

#[tauri::command]
pub async fn get_read_later(
    pool: State<'_, SqlitePool>,
    limit: Option<i32>,
    offset: Option<i32>,
) -> Result<Vec<crate::database::schema::FeedItem>, String> {
    let limit = limit.unwrap_or(50);
    let offset = offset.unwrap_or(0);

    let items = sqlx::query_as::<_, crate::database::schema::FeedItem>(
        "SELECT * FROM feed_items WHERE is_read_later = 1 ORDER BY published_at DESC LIMIT $1 OFFSET $2"
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(pool.inner())
    .await
    .map_err(|e| format!("Failed to get read later: {}", e))?;

    Ok(items)
}

#[tauri::command]
pub async fn get_unread(
    pool: State<'_, SqlitePool>,
    subscription_id: Option<i64>,
    limit: Option<i32>,
    offset: Option<i32>,
) -> Result<Vec<crate::database::schema::FeedItem>, String> {
    let limit = limit.unwrap_or(50);
    let offset = offset.unwrap_or(0);

    let items = if let Some(sub_id) = subscription_id {
        sqlx::query_as::<_, crate::database::schema::FeedItem>(
            "SELECT * FROM feed_items WHERE subscription_id = $1 AND is_read = 0 ORDER BY published_at DESC LIMIT $2 OFFSET $3"
        )
        .bind(sub_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool.inner())
        .await
    } else {
        sqlx::query_as::<_, crate::database::schema::FeedItem>(
            "SELECT * FROM feed_items WHERE is_read = 0 ORDER BY published_at DESC LIMIT $1 OFFSET $2"
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool.inner())
        .await
    };

    items.map_err(|e| format!("Failed to get unread: {}", e))
}

#[tauri::command]
pub async fn get_today_items(
    pool: State<'_, SqlitePool>,
    limit: Option<i32>,
    offset: Option<i32>,
) -> Result<Vec<crate::database::schema::FeedItem>, String> {
    let limit = limit.unwrap_or(50);
    let offset = offset.unwrap_or(0);

    let items = sqlx::query_as::<_, crate::database::schema::FeedItem>(
        "SELECT * FROM feed_items WHERE DATE(published_at) = DATE('now') ORDER BY published_at DESC LIMIT $1 OFFSET $2"
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(pool.inner())
    .await
    .map_err(|e| format!("Failed to get today items: {}", e))?;

    Ok(items)
}

#[tauri::command]
pub async fn save_item_tags(
    pool: State<'_, SqlitePool>,
    item_id: i64,
    tags: Vec<String>,
    category: Option<String>,
) -> Result<(), String> {
    let tags_json = serde_json::to_string(&tags)
        .map_err(|e| format!("Failed to serialize tags: {}", e))?;

    sqlx::query(
        "UPDATE feed_items SET tags = $1, category = $2 WHERE id = $3"
    )
    .bind(&tags_json)
    .bind(&category)
    .bind(item_id)
    .execute(pool.inner())
    .await
    .map_err(|e| format!("Failed to save item tags: {}", e))?;

    Ok(())
}

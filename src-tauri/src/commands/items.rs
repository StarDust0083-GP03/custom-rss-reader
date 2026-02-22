use crate::database::schema::FeedItem;

#[tauri::command]
pub async fn get_items(
    pool: tauri::State<'_, sqlx::SqlitePool>,
    subscription_id: Option<i64>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<FeedItem>, String> {
    let limit = limit.unwrap_or(50);
    let offset = offset.unwrap_or(0);

    let items = if let Some(sub_id) = subscription_id {
        sqlx::query_as::<_, FeedItem>(
            r#"
            SELECT * FROM feed_items
            WHERE subscription_id = $1
            ORDER BY published_at DESC
            LIMIT $2 OFFSET $3
            "#
        )
        .bind(sub_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool.inner())
        .await
    } else {
        sqlx::query_as::<_, FeedItem>(
            r#"
            SELECT * FROM feed_items
            ORDER BY published_at DESC
            LIMIT $1 OFFSET $2
            "#
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool.inner())
        .await
    };

    Ok(items.map_err(|e| format!("Failed to get items: {}", e))?)
}

#[tauri::command]
pub async fn search_items(
    pool: tauri::State<'_, sqlx::SqlitePool>,
    query: String,
    limit: Option<i64>,
) -> Result<Vec<FeedItem>, String> {
    let limit = limit.unwrap_or(50);
    let search_pattern = format!("%{}%", query);

    let items = sqlx::query_as::<_, FeedItem>(
        r#"
        SELECT * FROM feed_items
        WHERE title LIKE $1 OR description LIKE $1 OR content LIKE $1
        ORDER BY published_at DESC
        LIMIT $2
        "#
    )
    .bind(search_pattern)
    .bind(limit)
    .fetch_all(pool.inner())
    .await
    .map_err(|e| format!("Failed to search items: {}", e))?;

    Ok(items)
}

#[tauri::command]
pub async fn get_item(
    pool: tauri::State<'_, sqlx::SqlitePool>,
    id: i64,
) -> Result<FeedItem, String> {
    let item = sqlx::query_as::<_, FeedItem>("SELECT * FROM feed_items WHERE id = $1")
        .bind(id)
        .fetch_one(pool.inner())
        .await
        .map_err(|e| format!("Failed to get item: {}", e))?;

    Ok(item)
}

#[tauri::command]
pub async fn get_items_by_subscription(
    pool: tauri::State<'_, sqlx::SqlitePool>,
    subscription_id: i64,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<FeedItem>, String> {
    get_items(pool, Some(subscription_id), limit, offset).await
}

#[tauri::command]
pub async fn get_items_by_tag(
    pool: tauri::State<'_, sqlx::SqlitePool>,
    tag: String,
    subscription_id: Option<i64>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<FeedItem>, String> {
    let limit = limit.unwrap_or(50);
    let offset = offset.unwrap_or(0);
    let search_pattern = format!("%\"{}\"%", tag);

    let items = if let Some(sub_id) = subscription_id {
        sqlx::query_as::<_, FeedItem>(
            r#"
            SELECT * FROM feed_items
            WHERE subscription_id = $1 AND tags LIKE $2
            ORDER BY published_at DESC
            LIMIT $3 OFFSET $4
            "#
        )
        .bind(sub_id)
        .bind(&search_pattern)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool.inner())
        .await
    } else {
        sqlx::query_as::<_, FeedItem>(
            r#"
            SELECT * FROM feed_items
            WHERE tags LIKE $1
            ORDER BY published_at DESC
            LIMIT $2 OFFSET $3
            "#
        )
        .bind(&search_pattern)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool.inner())
        .await
    };

    Ok(items.map_err(|e| format!("Failed to get items by tag: {}", e))?)
}

#[tauri::command]
pub async fn get_all_tags(
    pool: tauri::State<'_, sqlx::SqlitePool>,
    subscription_id: Option<i64>,
) -> Result<Vec<String>, String> {
    let rows: Vec<(Option<String>,)> = if let Some(sub_id) = subscription_id {
        sqlx::query_as(
            "SELECT DISTINCT tags FROM feed_items WHERE subscription_id = $1 AND tags IS NOT NULL AND tags != ''"
        )
        .bind(sub_id)
        .fetch_all(pool.inner())
        .await
        .map_err(|e| format!("Failed to get tags: {}", e))?
    } else {
        sqlx::query_as(
            "SELECT DISTINCT tags FROM feed_items WHERE tags IS NOT NULL AND tags != ''"
        )
        .fetch_all(pool.inner())
        .await
        .map_err(|e| format!("Failed to get tags: {}", e))?
    };

    let mut all_tags = std::collections::HashSet::new();
    for (tags_json,) in rows {
        if let Some(json_str) = tags_json {
            if let Ok(tags_vec) = serde_json::from_str::<Vec<String>>(&json_str) {
                all_tags.extend(tags_vec);
            }
        }
    }

    let mut tags: Vec<_> = all_tags.into_iter().collect();
    tags.sort();
    Ok(tags)
}

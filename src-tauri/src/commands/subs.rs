use crate::database::schema::{Subscription, NewSubscription};
use sqlx::SqlitePool;

#[tauri::command]
pub async fn add_subscription(
    pool: tauri::State<'_, SqlitePool>,
    url: String,
    title: Option<String>,
    website_url: Option<String>,
    use_website: Option<bool>,
    rsshub_url: Option<String>,
) -> Result<Subscription, String> {
    let new_sub = NewSubscription {
        url,
        title,
        website_url,
        rsshub_url,
        use_website: use_website.unwrap_or(false),
        auto_classify: true, // Default to true for new subscriptions
        opml_attributes: None,
    };

    let result = sqlx::query_as::<_, Subscription>(
        r#"
        INSERT INTO subscriptions (url, title, website_url, rsshub_url, use_website, auto_classify, opml_attributes)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING *
        "#
    )
    .bind(&new_sub.url)
    .bind(&new_sub.title)
    .bind(&new_sub.website_url)
    .bind(&new_sub.rsshub_url)
    .bind(new_sub.use_website)
    .bind(new_sub.auto_classify)
    .bind(&new_sub.opml_attributes)
    .fetch_one(pool.inner())
    .await
    .map_err(|e| format!("Failed to add subscription: {}", e))?;

    Ok(result)
}

#[tauri::command]
pub async fn remove_subscription(
    pool: tauri::State<'_, SqlitePool>,
    id: i64,
) -> Result<(), String> {
    sqlx::query("DELETE FROM subscriptions WHERE id = $1")
        .bind(id)
        .execute(pool.inner())
        .await
        .map_err(|e| format!("Failed to remove subscription: {}", e))?;

    Ok(())
}

#[tauri::command]
pub async fn list_subscriptions(
    pool: tauri::State<'_, SqlitePool>,
) -> Result<Vec<Subscription>, String> {
    let subscriptions = sqlx::query_as::<_, Subscription>("SELECT * FROM subscriptions ORDER BY title")
        .fetch_all(pool.inner())
        .await
        .map_err(|e| format!("Failed to list subscriptions: {}", e))?;

    Ok(subscriptions)
}

#[tauri::command]
pub async fn get_subscription(
    pool: tauri::State<'_, SqlitePool>,
    id: i64,
) -> Result<Subscription, String> {
    let subscription = sqlx::query_as::<_, Subscription>("SELECT * FROM subscriptions WHERE id = $1")
        .bind(id)
        .fetch_one(pool.inner())
        .await
        .map_err(|e| format!("Failed to get subscription: {}", e))?;

    Ok(subscription)
}

#[tauri::command]
pub async fn update_subscription(
    pool: tauri::State<'_, SqlitePool>,
    id: i64,
    title: Option<String>,
    website_url: Option<String>,
    use_website: Option<bool>,
    rsshub_url: Option<String>,
) -> Result<Subscription, String> {
    let subscription = sqlx::query_as::<_, Subscription>(
        r#"
        UPDATE subscriptions
        SET
            title = COALESCE($2, title),
            website_url = COALESCE($3, website_url),
            use_website = COALESCE($4, use_website),
            rsshub_url = COALESCE($5, rsshub_url),
            updated_at = CURRENT_TIMESTAMP
        WHERE id = $1
        RETURNING *
        "#
    )
    .bind(id)
    .bind(title)
    .bind(website_url)
    .bind(use_website)
    .bind(rsshub_url)
    .fetch_one(pool.inner())
    .await
    .map_err(|e| format!("Failed to update subscription: {}", e))?;

    Ok(subscription)
}

#[tauri::command]
pub async fn toggle_use_website(
    pool: tauri::State<'_, SqlitePool>,
    id: i64,
) -> Result<Subscription, String> {
    let subscription = sqlx::query_as::<_, Subscription>(
        r#"
        UPDATE subscriptions
        SET use_website = NOT use_website,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = $1
        RETURNING *
        "#
    )
    .bind(id)
    .fetch_one(pool.inner())
    .await
    .map_err(|e| format!("Failed to toggle use_website: {}", e))?;

    Ok(subscription)
}

#[tauri::command]
pub async fn toggle_auto_classify(
    pool: tauri::State<'_, SqlitePool>,
    id: i64,
) -> Result<Subscription, String> {
    let subscription = sqlx::query_as::<_, Subscription>(
        r#"
        UPDATE subscriptions
        SET auto_classify = NOT auto_classify,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = $1
        RETURNING *
        "#
    )
    .bind(id)
    .fetch_one(pool.inner())
    .await
    .map_err(|e| format!("Failed to toggle auto_classify: {}", e))?;

    Ok(subscription)
}

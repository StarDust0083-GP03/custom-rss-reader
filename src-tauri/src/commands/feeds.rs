use crate::database::schema::{FeedItem, Subscription};
use crate::debug::DebugLogger;
use crate::feed::{FeedFetcher, parse_with_logger};
use crate::ai::{AiService, ClassificationRequest, AiConfig};
use tauri::{Emitter, Manager};
use sqlx::SqlitePool;
use std::collections::HashSet;

/// Insert a feed item into the database and return the new item ID
async fn insert_feed_item(pool: &SqlitePool, item: &FeedItem) -> Result<i64, String> {
    let result = sqlx::query(
        r#"
        INSERT INTO feed_items (
            subscription_id, guid, title, link, content,
            description, author, published_at, is_website_content,
            is_read, is_favorite, is_read_later, tags, category,
            translated_title, translated_content, translated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 0, 0, 0, $10, $11, $12, $13, $14)
        "#,
    )
    .bind(item.subscription_id)
    .bind(&item.guid)
    .bind(&item.title)
    .bind(&item.link)
    .bind(&item.content)
    .bind(&item.description)
    .bind(&item.author)
    .bind(item.published_at)
    .bind(item.is_website_content)
    .bind(&item.tags)
    .bind(&item.category)
    .bind(&item.translated_title)
    .bind(&item.translated_content)
    .bind(item.translated_at)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to insert item: {}", e))?;

    Ok(result.last_insert_rowid())
}

/// Check if a feed item already exists in the database
async fn feed_item_exists(pool: &SqlitePool, subscription_id: i64, guid: &str) -> Result<bool, String> {
    let exists: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM feed_items WHERE subscription_id = $1 AND guid = $2",
    )
    .bind(subscription_id)
    .bind(guid)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to check item existence: {}", e))?;
    Ok(exists.is_some())
}

/// Get all existing tags from the database for a specific subscription
async fn get_existing_tags(pool: &SqlitePool, subscription_id: i64) -> Result<Vec<String>, String> {
    let rows: Vec<(Option<String>,)> = sqlx::query_as(
        "SELECT DISTINCT tags FROM feed_items WHERE subscription_id = $1 AND tags IS NOT NULL AND tags != ''"
    )
    .bind(subscription_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to get existing tags: {}", e))?;

    let mut all_tags = HashSet::new();
    for (tags_json,) in rows {
        if let Some(json_str) = tags_json {
            if let Ok(tags_vec) = serde_json::from_str::<Vec<String>>(&json_str) {
                all_tags.extend(tags_vec);
            }
        }
    }

    Ok(all_tags.into_iter().collect())
}

/// Load AI config from app state or file
fn load_ai_config(app_handle: &tauri::AppHandle) -> Option<AiConfig> {
    // Try to get from app state
    if let Some(state) = app_handle.try_state::<AiConfig>() {
        return Some(state.inner().clone());
    }

    // Try to load from persistent storage
    let resource_path = app_handle.path().app_config_dir().ok()?;
    std::fs::create_dir_all(&resource_path).ok()?;

    let config_file = resource_path.join("ai_config.json");
    if config_file.exists() {
        let content = std::fs::read_to_string(&config_file).ok()?;
        let config: AiConfig = serde_json::from_str(&content).ok()?;
        // Store in app state
        app_handle.manage(config.clone());
        Some(config)
    } else {
        None
    }
}

/// Classify a single item with AI (internal helper)
async fn classify_item_internal(
    pool: &SqlitePool,
    app_handle: &tauri::AppHandle,
    item_id: i64,
    subscription_title: String,
    existing_tags: Vec<String>,
) -> Result<(), String> {
    // Get AI config
    let config = load_ai_config(app_handle)
        .ok_or_else(|| "No AI configuration found".to_string())?;

    // Get item from database
    let item: FeedItem = sqlx::query_as::<_, _>("SELECT * FROM feed_items WHERE id = $1")
        .bind(item_id)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("Failed to get item: {}", e))?;

    // Prepare classification request
    let content_snippet = item.content.as_ref()
        .map(|c| c.chars().take(500).collect::<String>());

    let ai_service = AiService::new(config)?;

    let request = ClassificationRequest {
        title: item.title.clone(),
        description: item.description.clone(),
        content_snippet,
        rss_title: Some(subscription_title),
        existing_tags: if existing_tags.is_empty() { None } else { Some(existing_tags) },
    };

    let result = ai_service.classify(request).await?;

    // Save to database
    let tags_json = serde_json::to_string(&result.tags)
        .map_err(|e| format!("Failed to serialize tags: {}", e))?;
    let category_str = result.category.as_ref().map(|s| s.as_str()).unwrap_or("");

    sqlx::query("UPDATE feed_items SET tags = $1, category = $2 WHERE id = $3")
        .bind(&tags_json)
        .bind(category_str)
        .bind(item_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to update item: {}", e))?;

    Ok(())
}

/// Result of saving feed items
struct SaveItemsResult {
    items_count: usize,
    new_items_count: usize,
    new_item_ids: Vec<i64>,
}

/// Save feed items to database, returning new item IDs for classification
async fn save_feed_items(
    pool: &SqlitePool,
    items: &[FeedItem],
) -> Result<SaveItemsResult, String> {
    let items_count = items.len();
    let mut new_items_count = 0;
    let mut new_item_ids = Vec::new();

    for item in items {
        if let Some(guid) = &item.guid {
            match feed_item_exists(pool, item.subscription_id, guid).await {
                Ok(false) => {
                    match insert_feed_item(pool, item).await {
                        Ok(item_id) => {
                            new_items_count += 1;
                            new_item_ids.push(item_id);
                        }
                        Err(e) => {
                            eprintln!("DB error inserting item: {}", e);
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("DB error checking item existence: {}", e);
                }
            }
        }
    }

    Ok(SaveItemsResult {
        items_count,
        new_items_count,
        new_item_ids,
    })
}

/// Trigger async classification for new items
fn trigger_classification(
    pool: SqlitePool,
    app_handle: tauri::AppHandle,
    subscription: &Subscription,
    new_item_ids: Vec<i64>,
) {
    if new_item_ids.is_empty() || !subscription.auto_classify {
        return;
    }

    let sub_title = subscription.title.clone().unwrap_or_else(|| subscription.url.clone());
    let sub_id = subscription.id;

    tokio::spawn(async move {
        // Get existing tags once for all items
        let existing_tags = match get_existing_tags(&pool, sub_id).await {
            Ok(tags) => tags,
            Err(_) => Vec::new(),
        };

        for item_id in new_item_ids {
            let _ = classify_item_internal(
                &pool,
                &app_handle,
                item_id,
                sub_title.clone(),
                existing_tags.clone(),
            ).await;
        }
    });
}

#[tauri::command]
pub async fn fetch_feed(
    app_handle: tauri::AppHandle,
    subscription_id: i64,
    subscription: Subscription,
) -> Result<Vec<FeedItem>, String> {
    let debug_logger = DebugLogger::new(&app_handle);

    debug_logger.log_info(
        "fetch_feed_command",
        &format!("Starting fetch for subscription ID: {}", subscription_id),
    );

    let fetcher = FeedFetcher::new().with_debug(debug_logger.clone());

    // Fetch RSS feed content
    let feed_content = fetcher.fetch_feed(&subscription.url).await.map_err(|e| {
        debug_logger.log_error(
            "fetch_feed_command",
            &format!("Failed to fetch feed: {}", e),
        );
        format!("Failed to fetch feed: {}", e)
    })?;

    let mut items = parse_with_logger(&feed_content, subscription_id, &debug_logger)
        .map_err(|e| {
            debug_logger.log_error(
                "fetch_feed_command",
                &format!("Failed to parse feed: {}", e),
            );
            format!("Failed to parse feed: {}", e)
        })?;

    // If use_website is true, fetch website content for each item
    if subscription.use_website {
        let items_count = items.len();
        debug_logger.log_info(
            "fetch_feed_command",
            &format!("Fetching website content for {} items", items_count),
        );
        for (index, item) in items.iter_mut().enumerate() {
            if let Some(link) = &item.link {
                debug_logger.log_info(
                    "fetch_feed_command",
                    &format!("Fetching website {}/{}: {}", index + 1, items_count, link),
                );
                match fetcher.fetch_website_content(link).await {
                    Ok(content) => {
                        item.content = Some(content);
                        item.is_website_content = true;
                    }
                    Err(e) => {
                        debug_logger.log_error(
                            "fetch_feed_command",
                            &format!("Failed to fetch website content for {}: {}", link, e),
                        );
                        // Fall back to RSS content
                        item.is_website_content = false;
                    }
                }
            }
        }
    }

    debug_logger.log_info(
        "fetch_feed_command",
        &format!("Successfully processed {} items", items.len()),
    );

    Ok(items
        .into_iter()
        .map(|item| FeedItem {
            id: 0, // Will be set by database
            subscription_id: item.subscription_id,
            guid: item.guid,
            title: item.title,
            link: item.link,
            content: item.content,
            description: item.description,
            author: item.author,
            published_at: item.published_at,
            fetched_at: chrono::Utc::now(),
            is_website_content: item.is_website_content,
            is_read: false,
            is_favorite: false,
            is_read_later: false,
            tags: None,
            category: None,
            translated_title: None,
            translated_content: None,
            translated_at: None,
        })
        .collect())
}

#[tauri::command]
pub async fn fetch_all_feeds(
    app_handle: tauri::AppHandle,
    pool: tauri::State<'_, sqlx::SqlitePool>,
) -> Result<String, String> {
    // Get all subscriptions
    let subscriptions = sqlx::query_as::<_, Subscription>("SELECT * FROM subscriptions")
        .fetch_all(pool.inner())
        .await
        .map_err(|e| format!("Failed to fetch subscriptions: {}", e))?;

    let total = subscriptions.len();

    // 使用并发限制：最多同时处理 20 个订阅
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(20));
    let mut tasks = Vec::new();

    // 用于收集结果的通道
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    // 原子计数器用于跟踪实际完成进度
    let completed_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    for subscription in subscriptions.iter() {
        let subscription = subscription.clone();
        let app_handle = app_handle.clone();
        let pool_inner = pool.inner().clone();
        let semaphore = semaphore.clone();
        let tx = tx.clone();
        let completed_count = completed_count.clone();

        let task = tokio::spawn(async move {
            // 获取信号量许可（限制并发数）
            let _permit = semaphore.acquire().await.unwrap();

            let title = subscription
                .title
                .as_ref()
                .unwrap_or(&subscription.url)
                .clone();
            let result = match fetch_feed(app_handle.clone(), subscription.id, subscription.clone()).await {
                Ok(items) => {
                    let save_result = save_feed_items(&pool_inner, &items).await;

                    match save_result {
                        Ok(saved) => {
                            // Trigger async classification for new items
                            trigger_classification(
                                pool_inner.clone(),
                                app_handle.clone(),
                                &subscription,
                                saved.new_item_ids,
                            );

                            // Send success event
                            let _ = app_handle.emit_to(
                                "main",
                                "fetch-success",
                                serde_json::json!({
                                    "title": title.clone(),
                                    "count": saved.items_count
                                }),
                            );

                            // Update progress
                            let completed = completed_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                            let _ = app_handle.emit_to(
                                "main",
                                "fetch-progress",
                                serde_json::json!({
                                    "current": completed,
                                    "total": total,
                                    "title": title,
                                    "status": "completed"
                                }),
                            );

                            if saved.new_items_count > 0 {
                                Ok((title, saved.new_items_count))
                            } else if saved.items_count > 0 {
                                Ok((title, 0))
                            } else {
                                Err((title, "No items found in feed".to_string()))
                            }
                        }
                        Err(e) => {
                            Err((title, e))
                        }
                    }
                }
                Err(e) => {
                    // 发送失败事件
                    let _ = app_handle.emit_to(
                        "main",
                        "fetch-error",
                        serde_json::json!({
                            "title": title.clone(),
                            "error": e.clone()
                        }),
                    );

                    // 仍然要更新完成计数
                    let completed = completed_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    let _ = app_handle.emit_to(
                        "main",
                        "fetch-progress",
                        serde_json::json!({
                            "current": completed,
                            "total": total,
                            "title": title,
                            "status": "failed"
                        }),
                    );

                    Err((title, e))
                }
            };

            // 发送结果到通道
            let _ = tx.send(result);
        });

        tasks.push(task);
    }

    // 收集所有结果
    drop(tx); // 关闭发送端

    let mut total_items = 0;
    let mut errors = Vec::new();

    // 等待所有任务完成并收集结果
    while let Some(result) = rx.recv().await {
        match result {
            Ok((_title, count)) => {
                total_items += count;
            }
            Err((title, error)) => {
                errors.push(format!("Subscription {}: {}", title, error));
            }
        }
    }

    // 确保所有任务都完成
    for task in tasks {
        let _ = task.await;
    }

    Ok(format!(
        "Fetched {} new items from {} subscriptions. Errors: {}",
        total_items,
        total,
        errors.join("; ")
    ))
}

#[tauri::command]
pub async fn refresh_subscriptions(
    app_handle: tauri::AppHandle,
    pool: tauri::State<'_, sqlx::SqlitePool>,
    subscription_ids: Vec<i64>,
) -> Result<String, String> {
    let mut total_items = 0;
    let mut errors = Vec::new();

    for id in subscription_ids {
        let subscription =
            sqlx::query_as::<_, Subscription>("SELECT * FROM subscriptions WHERE id = $1")
                .bind(id)
                .fetch_one(pool.inner())
                .await
                .map_err(|e| format!("Failed to fetch subscription: {}", e))?;

        match fetch_feed(app_handle.clone(), id, subscription.clone()).await {
            Ok(items) => {
                match save_feed_items(pool.inner(), &items).await {
                    Ok(saved) => {
                        total_items += saved.new_items_count;
                        // Trigger async classification for new items
                        trigger_classification(
                            pool.inner().clone(),
                            app_handle.clone(),
                            &subscription,
                            saved.new_item_ids,
                        );
                    }
                    Err(e) => {
                        errors.push(format!("Subscription ID {}: {}", id, e));
                    }
                }
            }
            Err(e) => {
                errors.push(format!("Subscription ID {}: {}", id, e));
            }
        }
    }

    Ok(format!(
        "Refreshed {} new items. Errors: {}",
        total_items,
        errors.join("; ")
    ))
}

#[tauri::command]
pub async fn fetch_website_content(url: String) -> Result<String, String> {
    let fetcher = FeedFetcher::new();
    fetcher.fetch_website_content(&url).await
        .map_err(|e| format!("Failed to fetch website: {}", e))
}

#[tauri::command]
pub async fn translate_website_content(
    app_handle: tauri::AppHandle,
    url: String,
) -> Result<String, String> {
    // First fetch the website content
    let fetcher = FeedFetcher::new();
    let content = fetcher.fetch_website_content(&url).await
        .map_err(|e| format!("Failed to fetch website: {}", e))?;
    
    // Extract main content while preserving HTML structure
    let html_content = extract_main_content_for_translation(&content);
    
    // Translate the content using AI (preserves HTML structure)
    let config = load_ai_config(&app_handle)
        .ok_or_else(|| "No AI configuration found. Please configure API key first.".to_string())?;
    
    let ai_service = AiService::new(config)?;
    
    let bilingual = ai_service.translate_bilingual_segmented(
        &html_content,
        "auto",
        "zh-CN",
    ).await?;
    
    Ok(bilingual)
}

fn extract_main_content_for_translation(html: &str) -> String {
    use scraper::{Html, Selector};
    
    let document = Html::parse_document(html);
    
    // Try common content selectors in order of preference
    let selectors = [
        "article",
        "[role='main']",
        "main",
        ".post-content",
        ".entry-content",
        ".article-content",
        ".content",
        "#content",
        "body",
    ];
    
    for selector_str in selectors {
        if let Ok(selector) = Selector::parse(selector_str) {
            if let Some(element) = document.select(&selector).next() {
                let html = element.html();
                // Only return if we have substantial content
                if html.len() > 200 {
                    return html;
                }
            }
        }
    }
    
    // Fallback: return the body or original content
    if let Ok(selector) = Selector::parse("body") {
        if let Some(element) = document.select(&selector).next() {
            return element.html();
        }
    }
    
    html.to_string()
}

fn extract_text_from_html(html: &str) -> String {
    // Simple HTML tag removal
    let mut result = String::new();
    let mut in_tag = false;
    
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                result.push(' ');
            }
            _ if !in_tag => result.push(c),
            _ => {}
        }
    }
    
    // Clean up whitespace
    let mut cleaned = String::new();
    let mut last_was_space = false;
    for c in result.chars() {
        if c.is_whitespace() {
            if !last_was_space {
                cleaned.push(' ');
                last_was_space = true;
            }
        } else {
            cleaned.push(c);
            last_was_space = false;
        }
    }
    
    cleaned.trim().to_string()
}

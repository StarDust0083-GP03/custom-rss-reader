use crate::error::AppError;
use crate::models::{NewFeedItem, NewSubscription};

use super::helpers::TestEnv;

/// Helper: create a subscription and return its ID.
async fn seed_sub(env: &TestEnv) -> i64 {
    env.service
        .add_subscription(NewSubscription {
            url: "https://example.com/rss".into(),
            title: Some("Test Sub".into()),
            ..Default::default()
        })
        .await
        .expect("Failed to seed subscription")
        .id
}

/// Helper: create a feed item with the given subscription_id.
async fn create_item(env: &TestEnv, sub_id: i64, title: &str) -> i64 {
    env.feed_service
        .create_item(NewFeedItem {
            subscription_id: sub_id,
            title: title.into(),
            content: Some("<p>Test content</p>".into()),
            ..Default::default()
        })
        .await
        .expect("Failed to create feed item")
        .id
}

// ---------------------------------------------------------------------------
// CREATE
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_create_feed_item() {
    let env = TestEnv::new().await;
    let sub_id = seed_sub(&env).await;

    let item = env
        .feed_service
        .create_item(NewFeedItem {
            subscription_id: sub_id,
            title: "Test Article".into(),
            link: Some("https://example.com/article".into()),
            content: Some("<p>Hello world</p>".into()),
            ..Default::default()
        })
        .await
        .expect("Should create feed item");

    assert!(item.id > 0);
    assert_eq!(item.title, "Test Article");
    assert_eq!(item.link, Some("https://example.com/article".into()));
    assert_eq!(item.content, Some("<p>Hello world</p>".into()));
    assert!(item.content_md.is_none()); // no MD cached yet
    assert!(!item.is_read);
    assert!(!item.is_favorite);
}

#[tokio::test]
async fn test_create_feed_item_with_content_md() {
    let env = TestEnv::new().await;
    let sub_id = seed_sub(&env).await;

    let item = env
        .feed_service
        .create_item(NewFeedItem {
            subscription_id: sub_id,
            title: "MD Cached Article".into(),
            content: Some("<h1>Title</h1><p>Content</p>".into()),
            content_md: Some("# Title\n\nContent".into()),
            ..Default::default()
        })
        .await
        .expect("Should create feed item with MD cache");

    assert_eq!(item.content_md, Some("# Title\n\nContent".into()));
}

// ---------------------------------------------------------------------------
// READ
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_find_feed_item_by_id() {
    let env = TestEnv::new().await;
    let sub_id = seed_sub(&env).await;

    let created_id = create_item(&env, sub_id, "Find Me").await;

    let found = env.feed_service.get_item(created_id).await.unwrap();
    assert_eq!(found.id, created_id);
    assert_eq!(found.title, "Find Me");
}

#[tokio::test]
async fn test_find_feed_item_not_found() {
    let env = TestEnv::new().await;

    let result = env.feed_service.get_item(99999).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), AppError::NotFound(_)));
}

#[tokio::test]
async fn test_find_feed_items_by_subscription() {
    let env = TestEnv::new().await;
    let sub_id = seed_sub(&env).await;

    // No items yet
    let items = env.feed_service.get_items_by_subscription(sub_id).await.unwrap();
    assert!(items.is_empty());

    // Add two items
    create_item(&env, sub_id, "Item 1").await;
    create_item(&env, sub_id, "Item 2").await;

    let items = env.feed_service.get_items_by_subscription(sub_id).await.unwrap();
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|i| i.subscription_id == sub_id));
}

// ---------------------------------------------------------------------------
// UPDATE content_md
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_update_content_md() {
    let env = TestEnv::new().await;
    let sub_id = seed_sub(&env).await;
    let item_id = create_item(&env, sub_id, "To Cache").await;

    let updated = env
        .feed_repo
        .update_content_md(item_id, "# Cached Markdown\n\nOriginal HTML")
        .await
        .unwrap();

    assert_eq!(
        updated.content_md,
        Some("# Cached Markdown\n\nOriginal HTML".into())
    );
}

// ---------------------------------------------------------------------------
// DELETE
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_delete_feed_item() {
    let env = TestEnv::new().await;
    let sub_id = seed_sub(&env).await;
    let item_id = create_item(&env, sub_id, "To Delete").await;

    sqlx::query("DELETE FROM feed_items WHERE id = $1")
        .bind(item_id)
        .execute(&env.pool)
        .await
        .unwrap();

    // Verify the item is gone
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM feed_items WHERE id = $1")
        .bind(item_id)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0);
}

#[tokio::test]
async fn test_delete_feed_item_not_found() {
    let env = TestEnv::new().await;

    let result = sqlx::query("DELETE FROM feed_items WHERE id = $1")
        .bind(99999i64)
        .execute(&env.pool)
        .await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().rows_affected(), 0);
}

// ---------------------------------------------------------------------------
// FULL PIPELINE: cache_website_content
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_cache_website_content() {
    let env = TestEnv::new().await;
    let sub_id = seed_sub(&env).await;
    let item_id = create_item(&env, sub_id, "Pipeline Test").await;

    // Simulate fetching website HTML and caching as Markdown
    let website_html = format!(
        r#"
    <html><body>
        <nav>Nav</nav>
        <article>
            <h1>Article Title</h1>
            <p>This is the <strong>main</strong> content. {}</p>
            <p>More text here. {}</p>
        </article>
        <footer>Footer</footer>
    </body></html>
    "#,
        "A longer paragraph to reach the 200 character minimum threshold for extraction. ".repeat(5),
        "Additional content to ensure the article body is sufficiently long. ".repeat(3)
    );

    let cached = env
        .feed_service
        .cache_website_content(item_id, &website_html)
        .await
        .expect("Should cache website content as Markdown");

    let md = cached.content_md.expect("content_md should be populated");
    assert!(md.contains("Article Title"), "Markdown should contain the heading");
    assert!(md.contains("main"), "Markdown should contain paragraph text");
    assert!(!md.contains("Nav"), "Navigation should be stripped");
    assert!(!md.contains("Footer"), "Footer should be stripped");
}

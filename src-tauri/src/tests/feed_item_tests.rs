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
        .update_content_md(item_id, "# Cached Markdown\n\nOriginal HTML", true)
        .await
        .unwrap();

    assert_eq!(
        updated.content_md,
        Some("# Cached Markdown\n\nOriginal HTML".into())
    );
    assert!(updated.is_website_content, "website cache flips the flag");
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

// ---------------------------------------------------------------------------
// Lazy RSS → markdown on first read
// ---------------------------------------------------------------------------

/// Regression: after `get_item` lazily populates `content_md`, the frontend
/// never sees raw HTML on the text-mode display path. RSS `content` is
/// converted to markdown (with pipeline-or-fallback) and the
/// `is_website_content` flag stays false (it's RSS, not website).
#[tokio::test]
async fn test_ensure_content_md_for_item_populates_and_preserves_flag() {
    let env = TestEnv::new().await;
    let sub_id = seed_sub(&env).await;

    // Item with rich HTML content but no cached markdown.
    let item_id = env
        .feed_service
        .create_item(NewFeedItem {
            subscription_id: sub_id,
            title: "RSS Item".into(),
            content: Some(
                "<article><h1>Headline</h1><p>This is the body. ".repeat(20) + "</p></article>",
            ),
            ..Default::default()
        })
        .await
        .expect("create item")
        .id;

    // Sanity: starts with no markdown, no website flag.
    let pre = env.feed_repo.find_by_id(item_id).await.unwrap();
    assert!(pre.content_md.is_none());
    assert!(!pre.is_website_content);

    let updated = crate::services::feed_service::ensure_content_md_for_item(
        &env.feed_repo,
        item_id,
    )
    .await
    .expect("lazy conversion");

    let md = updated
        .content_md
        .as_deref()
        .expect("content_md should be populated");
    assert!(md.contains("Headline"), "Markdown should contain the heading");
    assert!(
        !md.contains("<article>"),
        "Raw HTML must not survive the conversion"
    );
    assert!(
        !updated.is_website_content,
        "Lazy RSS conversion must not flip is_website_content"
    );

    // Second call is a no-op (already cached).
    let again = crate::services::feed_service::ensure_content_md_for_item(
        &env.feed_repo,
        item_id,
    )
    .await
    .expect("second call");
    assert_eq!(again.content_md.as_deref(), Some(md));
}

/// Short RSS content (smaller than the main-content 200-char threshold)
/// falls back to plain `html2md::parse_html` instead of erroring.
#[tokio::test]
async fn test_ensure_content_md_short_rss_falls_back_to_plain() {
    let env = TestEnv::new().await;
    let sub_id = seed_sub(&env).await;

    let item_id = env
        .feed_service
        .create_item(NewFeedItem {
            subscription_id: sub_id,
            title: "Short".into(),
            // Below the 200-char threshold that extract_main_content requires.
            content: Some("<p>Hi <a href=\"https://example.com\">there</a>.</p>".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;

    let updated = crate::services::feed_service::ensure_content_md_for_item(
        &env.feed_repo,
        item_id,
    )
    .await
    .expect("lazy conversion should not error on short input");

    let md = updated.content_md.expect("markdown should be populated");
    assert!(md.contains("[there]"));
    assert!(md.contains("(https://example.com)"));
}

// ---------------------------------------------------------------------------
// Bulk id lookups (ChromaDB similar-articles support)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_find_summaries_by_ids() {
    let env = TestEnv::new().await;
    let sub_id = seed_sub(&env).await;
    let a = create_item(&env, sub_id, "Alpha").await;
    let b = create_item(&env, sub_id, "Beta").await;
    let _c = create_item(&env, sub_id, "Gamma").await;

    let mut summaries = env
        .feed_repo
        .find_summaries_by_ids(&[b, a])
        .await
        .expect("Should fetch summaries by ids");
    summaries.sort_by_key(|s| s.id);
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].title, "Alpha");
    assert_eq!(summaries[1].title, "Beta");

    // Unknown ids are silently omitted; empty input short-circuits.
    let missing = env
        .feed_repo
        .find_summaries_by_ids(&[999_999])
        .await
        .expect("Missing ids should not error");
    assert!(missing.is_empty());
    let empty = env
        .feed_repo
        .find_summaries_by_ids(&[])
        .await
        .expect("Empty input should not error");
    assert!(empty.is_empty());
}

#[tokio::test]
async fn test_find_ids_by_subscription() {
    let env = TestEnv::new().await;
    let sub_id = seed_sub(&env).await;
    let a = create_item(&env, sub_id, "One").await;
    let b = create_item(&env, sub_id, "Two").await;

    let mut ids = env
        .feed_repo
        .find_ids_by_subscription(sub_id)
        .await
        .expect("Should fetch ids");
    ids.sort();
    assert_eq!(ids, vec![a.min(b), a.max(b)]);

    let none = env
        .feed_repo
        .find_ids_by_subscription(999_999)
        .await
        .expect("Unknown subscription should not error");
    assert!(none.is_empty());
}

// ---------------------------------------------------------------------------
// Chroma sync support: IndexRow keyset pagination + max_item_id
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_find_index_page_orders_ascending_after_id() {
    let env = TestEnv::new().await;
    let sub_id = seed_sub(&env).await;
    let id1 = create_item(&env, sub_id, "one").await;
    let id2 = create_item(&env, sub_id, "two").await;
    let id3 = create_item(&env, sub_id, "three").await;

    // Page 1 from the watermark 0
    let page = env
        .feed_repo
        .find_index_page(0, 2)
        .await
        .expect("find_index_page failed");
    assert_eq!(page.len(), 2);
    assert_eq!(page[0].id, id1);
    assert_eq!(page[1].id, id2);
    assert_eq!(page[0].title, "one");

    // Page 2 continues strictly after the last seen id — keyset semantics
    let page2 = env
        .feed_repo
        .find_index_page(page[1].id, 2)
        .await
        .expect("find_index_page page 2 failed");
    assert_eq!(page2.len(), 1);
    assert_eq!(page2[0].id, id3);

    // Beyond the end → empty page terminates the sync walk
    let page3 = env.feed_repo.find_index_page(id3, 2).await.unwrap();
    assert!(page3.is_empty());
}

#[tokio::test]
async fn test_find_index_page_truncates_text_columns() {
    let env = TestEnv::new().await;
    let sub_id = seed_sub(&env).await;
    // 5000 CJK chars in description = ~15KB; the projection must bound it.
    env.feed_service
        .create_item(NewFeedItem {
            subscription_id: sub_id,
            title: "big".into(),
            description: Some("字".repeat(5000)),
            ..Default::default()
        })
        .await
        .expect("create big item failed");

    let rows = env.feed_repo.find_index_page(0, 10).await.unwrap();
    assert_eq!(rows.len(), 1);
    let desc = rows[0].description.as_deref().expect("description present");
    assert!(desc.chars().count() <= 2001, "description must be truncated, got {} chars", desc.chars().count());
}

#[tokio::test]
async fn test_max_item_id_tracks_inserts_and_empty() {
    let env = TestEnv::new().await;
    // Empty table → 0 (not NULL / error) so the watermark validation works.
    assert_eq!(env.feed_repo.max_item_id().await.unwrap(), 0);

    let sub_id = seed_sub(&env).await;
    let id = create_item(&env, sub_id, "x").await;
    assert_eq!(env.feed_repo.max_item_id().await.unwrap(), id);
}

#[tokio::test]
async fn test_find_index_rows_by_ids() {
    let env = TestEnv::new().await;
    let sub_id = seed_sub(&env).await;
    let id1 = create_item(&env, sub_id, "one").await;
    let id2 = create_item(&env, sub_id, "two").await;

    // Empty input → empty output, no SQL error
    assert!(env.feed_repo.find_index_rows_by_ids(&[]).await.unwrap().is_empty());

    let rows = env
        .feed_repo
        .find_index_rows_by_ids(&[id1, id2, 999999])
        .await
        .unwrap();
    // Missing ids silently omitted; ascending order
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, id1);
    assert_eq!(rows[1].id, id2);
}

// ---------------------------------------------------------------------------
// Summary source columns (issue #3: 列表页缺失来源信息)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_summaries_carry_source_title() {
    let env = TestEnv::new().await;
    let sub = env
        .service
        .add_subscription(NewSubscription {
            url: "https://example.com/rss".into(),
            title: Some("Example Feed".into()),
            ..Default::default()
        })
        .await
        .expect("seed sub");
    create_item(&env, sub.id, "article").await;

    // Main list path
    let items = env.feed_repo.find_all(Some(sub.id), 10, 0).await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].source_title.as_deref(), Some("Example Feed"));
    assert_eq!(items[0].source_url.as_deref(), Some("https://example.com/rss"));

    // Search path (different query shape, same join)
    let hits = env.feed_repo.search("article", 10).await.unwrap();
    assert_eq!(hits[0].source_title.as_deref(), Some("Example Feed"));

    // Unread path
    let unread = env.feed_repo.get_unread(None, 10, 0).await.unwrap();
    assert_eq!(unread[0].source_title.as_deref(), Some("Example Feed"));

    // Titleless subscription → source_url is the fallback display value
    let sub2 = env
        .service
        .add_subscription(NewSubscription {
            url: "https://bare.com/rss".into(),
            title: None,
            ..Default::default()
        })
        .await
        .unwrap();
    create_item(&env, sub2.id, "bare article").await;
    let all = env.feed_repo.find_all(None, 10, 0).await.unwrap();
    let bare = all.iter().find(|i| i.subscription_id == sub2.id).unwrap();
    assert_eq!(bare.source_title, None);
    assert_eq!(bare.source_url.as_deref(), Some("https://bare.com/rss"));
}

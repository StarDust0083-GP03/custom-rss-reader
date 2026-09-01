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
async fn test_search_includes_cached_markdown() {
    let env = TestEnv::new().await;
    let sub_id = seed_sub(&env).await;

    env.feed_service
        .create_item(NewFeedItem {
            subscription_id: sub_id,
            title: "Search title".into(),
            content: Some("RSS teaser".into()),
            content_md: Some("Full body contains markdown-only phrase".into()),
            ..Default::default()
        })
        .await
        .unwrap();

    let results = env
        .feed_repo
        .search("markdown-only phrase", 50)
        .await
        .expect("search should include content_md");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].title, "Search title");
}

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
    let items = env.feed_repo.find_all(Some(sub_id), 50, 0).await.unwrap();
    assert!(items.is_empty());

    // Add two items
    create_item(&env, sub_id, "Item 1").await;
    create_item(&env, sub_id, "Item 2").await;

    let items = env.feed_repo.find_all(Some(sub_id), 50, 0).await.unwrap();
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|i| i.subscription_id == sub_id));
}

#[tokio::test]
async fn test_favorites_and_read_later_can_filter_by_subscription() {
    let env = TestEnv::new().await;
    let sub_a = seed_sub(&env).await;
    let sub_b = env
        .service
        .add_subscription(NewSubscription {
            url: "https://example.com/other.xml".into(),
            title: Some("Other Sub".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;
    let favorite_a = create_item(&env, sub_a, "Favorite A").await;
    let favorite_b = create_item(&env, sub_b, "Favorite B").await;
    let later_a = create_item(&env, sub_a, "Later A").await;
    let later_b = create_item(&env, sub_b, "Later B").await;

    env.feed_repo.toggle_favorite(favorite_a).await.unwrap();
    env.feed_repo.toggle_favorite(favorite_b).await.unwrap();
    env.feed_repo.toggle_read_later(later_a).await.unwrap();
    env.feed_repo.toggle_read_later(later_b).await.unwrap();

    let favorites = env
        .feed_repo
        .get_favorites(Some(sub_a), 50, 0)
        .await
        .unwrap();
    assert_eq!(
        favorites.iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![favorite_a]
    );
    let read_later = env
        .feed_repo
        .get_read_later(Some(sub_a), 50, 0)
        .await
        .unwrap();
    assert_eq!(
        read_later.iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![later_a]
    );
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
// TRANSLATION CACHE
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_empty_translation_clears_cached_content() {
    let env = TestEnv::new().await;
    let sub_id = seed_sub(&env).await;
    let item_id = create_item(&env, sub_id, "Translation cache").await;

    env.feed_repo
        .update_translation(item_id, None, "<div>cached</div>")
        .await
        .unwrap();
    let cleared = env
        .feed_repo
        .update_translation(item_id, None, "")
        .await
        .unwrap();

    assert!(cleared.translated_content.is_none());
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
        "A longer paragraph to reach the 200 character minimum threshold for extraction. "
            .repeat(5),
        "Additional content to ensure the article body is sufficiently long. ".repeat(3)
    );

    let cached = env
        .feed_service
        .cache_website_content(item_id, &website_html)
        .await
        .expect("Should cache website content as Markdown");

    let md = cached.content_md.expect("content_md should be populated");
    assert!(
        md.contains("Article Title"),
        "Markdown should contain the heading"
    );
    assert!(
        md.contains("main"),
        "Markdown should contain paragraph text"
    );
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

    let updated =
        crate::services::feed_service::ensure_content_md_for_item(&env.feed_repo, item_id)
            .await
            .expect("lazy conversion");

    let md = updated
        .content_md
        .as_deref()
        .expect("content_md should be populated");
    assert!(
        md.contains("Headline"),
        "Markdown should contain the heading"
    );
    assert!(
        !md.contains("<article>"),
        "Raw HTML must not survive the conversion"
    );
    assert!(
        !updated.is_website_content,
        "Lazy RSS conversion must not flip is_website_content"
    );

    // Second call is a no-op (already cached).
    let again = crate::services::feed_service::ensure_content_md_for_item(&env.feed_repo, item_id)
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

    let updated =
        crate::services::feed_service::ensure_content_md_for_item(&env.feed_repo, item_id)
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
    assert!(
        desc.chars().count() <= 2001,
        "description must be truncated, got {} chars",
        desc.chars().count()
    );
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
    assert!(env
        .feed_repo
        .find_index_rows_by_ids(&[])
        .await
        .unwrap()
        .is_empty());

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

/// The index projection must prefer `content_md` (full website text) over
/// the raw RSS `content` (often just a teaser) so history-backfilled
/// articles embed their real body.
#[tokio::test]
async fn test_index_rows_prefer_content_md() {
    let env = TestEnv::new().await;
    let sub_id = seed_sub(&env).await;

    // Website-mode item: full text lives in content_md, content is a teaser.
    let full_id = env
        .feed_service
        .create_item(NewFeedItem {
            subscription_id: sub_id,
            title: "full".into(),
            content: Some("<p>RSS teaser</p>".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;
    env.feed_repo
        .update_content_md(full_id, "# Full website article body", true)
        .await
        .unwrap();

    // Plain item with an EMPTY content_md (lazy conversion never ran):
    // must fall back to the RSS content.
    let lazy_id = env
        .feed_service
        .create_item(NewFeedItem {
            subscription_id: sub_id,
            title: "lazy".into(),
            content: Some("<p>RSS only text</p>".into()),
            content_md: Some(String::new()),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;

    let rows = env
        .feed_repo
        .find_index_rows_by_ids(&[full_id, lazy_id])
        .await
        .unwrap();
    assert_eq!(
        rows[0].content.as_deref(),
        Some("# Full website article body")
    );
    assert_eq!(rows[1].content.as_deref(), Some("<p>RSS only text</p>"));
}

// ---------------------------------------------------------------------------
// Website-Markdown backfill candidates
// ---------------------------------------------------------------------------

/// Helper: create a subscription with website mode enabled, return its ID.
async fn seed_website_sub(env: &TestEnv, url: &str) -> i64 {
    env.repo
        .create(NewSubscription {
            url: url.into(),
            title: Some("Website Sub".into()),
            use_website: true,
            ..Default::default()
        })
        .await
        .expect("Failed to seed website subscription")
        .id
}

#[tokio::test]
async fn test_find_website_backfill_candidates() {
    let env = TestEnv::new().await;
    let web_sub = seed_website_sub(&env, "https://web.example.com/rss").await;
    let rss_sub = seed_sub(&env).await; // use_website = false

    // Missing website Markdown → candidate
    let missing = env
        .feed_service
        .create_item(NewFeedItem {
            subscription_id: web_sub,
            title: "missing md".into(),
            link: Some("https://web.example.com/a".into()),
            content: Some("<p>teaser</p>".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;

    // Website Markdown already cached (is_website_content = 1) → NOT a candidate
    let cached = env
        .feed_service
        .create_item(NewFeedItem {
            subscription_id: web_sub,
            title: "cached md".into(),
            link: Some("https://web.example.com/b".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;
    env.feed_repo
        .update_content_md(cached, "# Cached", true)
        .await
        .unwrap();

    // Lazily converted RSS Markdown (is_website_content still 0) → candidate:
    // the full text lives on the website but was never fetched.
    let lazy = env
        .feed_service
        .create_item(NewFeedItem {
            subscription_id: web_sub,
            title: "lazy md".into(),
            link: Some("https://web.example.com/c".into()),
            content: Some("<p>rss text</p>".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;
    env.feed_repo
        .update_content_md(lazy, "rss text", false)
        .await
        .unwrap();

    // No link → can't fetch the website → NOT a candidate
    env.feed_service
        .create_item(NewFeedItem {
            subscription_id: web_sub,
            title: "no link".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    // Website mode off → NOT a candidate (RSS content is the full text)
    env.feed_service
        .create_item(NewFeedItem {
            subscription_id: rss_sub,
            title: "rss mode".into(),
            link: Some("https://example.com/d".into()),
            ..Default::default()
        })
        .await
        .unwrap();

    let candidates = env
        .feed_repo
        .find_website_backfill_candidates(10)
        .await
        .unwrap();
    // Newest-first: the lazy item was created after the missing one
    assert_eq!(candidates.len(), 2);
    assert_eq!(
        candidates[0],
        (lazy, "https://web.example.com/c".to_string())
    );
    assert_eq!(
        candidates[1],
        (missing, "https://web.example.com/a".to_string())
    );

    // The limit bounds the batch (politeness: Q)
    let one = env
        .feed_repo
        .find_website_backfill_candidates(1)
        .await
        .unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].0, lazy);
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
    assert_eq!(
        items[0].source_url.as_deref(),
        Some("https://example.com/rss")
    );

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

// ---------------------------------------------------------------------------
// TAG CATALOG
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_tag_catalog_canonicalizes_and_manages_mappings() {
    let env = TestEnv::new().await;
    let sub_id = seed_sub(&env).await;
    let first = create_item(&env, sub_id, "First tagged item").await;
    let second = create_item(&env, sub_id, "Second tagged item").await;

    env.feed_repo
        .save_tags(
            first,
            r#"["Machine Learning", "machine-learning", "AI", "Extra"]"#,
            "technology",
        )
        .await
        .unwrap();
    env.feed_repo.create_tag("Database").await.unwrap();

    let tags: Vec<String> =
        serde_json::from_str(&env.feed_repo.find_by_id(first).await.unwrap().tags.unwrap())
            .unwrap();
    assert_eq!(tags, vec!["machine_learning", "ai", "extra"]);
    assert!(env.feed_repo.create_tag("machine-learning").await.is_err());

    env.feed_repo
        .merge_tags("machine_learning", &["ai".into()])
        .await
        .unwrap();
    let after_merge: Vec<String> =
        serde_json::from_str(&env.feed_repo.find_by_id(first).await.unwrap().tags.unwrap())
            .unwrap();
    assert_eq!(after_merge, vec!["machine_learning", "extra"]);

    env.feed_repo
        .rename_tag("machine_learning", "artificial_intelligence")
        .await
        .unwrap();
    env.feed_repo
        .save_tags(second, r#"["AI"]"#, "technology")
        .await
        .unwrap();
    let second_tags: Vec<String> = serde_json::from_str(
        &env.feed_repo
            .find_by_id(second)
            .await
            .unwrap()
            .tags
            .unwrap(),
    )
    .unwrap();
    assert_eq!(second_tags, vec!["artificial_intelligence"]);

    let catalog = env.feed_repo.find_tag_catalog().await.unwrap();
    let head = catalog
        .iter()
        .find(|entry| entry.name == "artificial_intelligence")
        .expect("renamed head should exist");
    assert!(head.aliases.contains(&"ai".to_string()));
    assert!(head.aliases.contains(&"machine_learning".to_string()));

    env.feed_repo
        .delete_tag("artificial_intelligence")
        .await
        .unwrap();
    let blocked = env.feed_repo.find_blocked_tags().await.unwrap();
    assert!(blocked.contains(&"artificial_intelligence".to_string()));
    assert!(blocked.contains(&"ai".to_string()));
    assert!(blocked.contains(&"machine_learning".to_string()));
    assert!(env
        .feed_repo
        .save_tags(second, r#"["AI", "extra"]"#, "technology")
        .await
        .unwrap()
        .tags
        .as_deref()
        .is_some_and(|tags| tags == r#"["extra"]"#));

    env.feed_repo
        .restore_tag("artificial_intelligence")
        .await
        .unwrap();
    let restored = env.feed_repo.find_tag_catalog().await.unwrap();
    assert!(restored
        .iter()
        .any(|entry| entry.name == "artificial_intelligence"));
}

#[tokio::test]
async fn test_restore_tag_requires_a_blocked_non_alias_name() {
    let env = TestEnv::new().await;
    env.feed_repo.create_tag("machine_learning").await.unwrap();
    env.feed_repo.create_tag("ai").await.unwrap();
    env.feed_repo
        .merge_tags("machine_learning", &["ai".into()])
        .await
        .unwrap();

    assert!(env.feed_repo.restore_tag("never_blocked").await.is_err());

    // Even a corrupt/legacy blocked row cannot turn an existing alias into a
    // second canonical tag with the same name.
    sqlx::query("INSERT INTO blocked_tags (name) VALUES ('ai')")
        .execute(&env.pool)
        .await
        .unwrap();
    assert!(env.feed_repo.restore_tag("ai").await.is_err());
    let canonical_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM tag_catalog WHERE name = 'ai'")
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(canonical_count, 0);
}

#[tokio::test]
async fn test_find_all_tags_is_canonical_and_subscription_scoped() {
    let env = TestEnv::new().await;
    let sub_a = seed_sub(&env).await;
    let sub_b = env
        .service
        .add_subscription(NewSubscription {
            url: "https://other.example.com/rss".into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;
    let item_a = create_item(&env, sub_a, "A").await;
    let item_b = create_item(&env, sub_b, "B").await;

    env.feed_repo
        .save_tags(item_a, r#"["Machine Learning"]"#, "")
        .await
        .unwrap();
    env.feed_repo
        .save_tags(item_b, r#"["PostgreSQL"]"#, "")
        .await
        .unwrap();
    env.feed_repo.create_tag("Unused Subject").await.unwrap();

    assert_eq!(
        env.feed_repo.find_all_tags(None).await.unwrap(),
        vec!["machine_learning", "postgresql"]
    );
    assert_eq!(
        env.feed_repo.find_all_tags(Some(sub_a)).await.unwrap(),
        vec!["machine_learning"]
    );
}

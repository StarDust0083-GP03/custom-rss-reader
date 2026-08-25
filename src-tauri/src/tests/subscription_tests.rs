use crate::error::AppError;
use crate::models::{NewSubscription, UpdateSubscription};

use super::helpers::{new_sub, TestEnv};

// ---------------------------------------------------------------------------
// CREATE
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_create_subscription() {
    let env = TestEnv::new().await;

    let sub = env
        .service
        .add_subscription(NewSubscription {
            url: "https://example.com/rss".into(),
            title: Some("Example".into()),
            website_url: Some("https://example.com".into()),
            ..Default::default()
        })
        .await
        .expect("Should create subscription");

    assert!(sub.id > 0);
    assert_eq!(sub.url, "https://example.com/rss");
    assert_eq!(sub.title, Some("Example".into()));
    assert_eq!(sub.website_url, Some("https://example.com".into()));
    assert!(sub.auto_classify); // default
    assert!(!sub.use_website); // default
}

#[tokio::test]
async fn test_create_subscription_with_all_fields() {
    let env = TestEnv::new().await;

    let sub = env
        .service
        .add_subscription(NewSubscription {
            url: "https://blog.example.com/feed".into(),
            title: Some("Tech Blog".into()),
            website_url: Some("https://blog.example.com".into()),
            rsshub_url: Some("https://rsshub.example.com/tech".into()),
            use_website: true,
            auto_classify: false,
            opml_attributes: Some(r#"{"type":"rss","version":"2.0"}"#.into()),
        })
        .await
        .expect("Should create subscription with all fields");

    assert_eq!(sub.url, "https://blog.example.com/feed");
    assert_eq!(sub.title, Some("Tech Blog".into()));
    assert!(sub.use_website);
    assert!(!sub.auto_classify);
    assert_eq!(
        sub.opml_attributes,
        Some(r#"{"type":"rss","version":"2.0"}"#.into())
    );
}

#[tokio::test]
async fn test_create_subscription_duplicate_fails() {
    let env = TestEnv::new().await;

    env.service
        .add_subscription(new_sub("https://example.com/rss"))
        .await
        .expect("First creation should succeed");

    let result = env
        .service
        .add_subscription(new_sub("https://example.com/rss"))
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        AppError::Duplicate(msg) => {
            assert!(msg.contains("https://example.com/rss"), "Error: {msg}");
        }
        other => panic!("Expected Duplicate error, got: {other}"),
    }
}

#[tokio::test]
async fn test_create_subscription_empty_url_fails() {
    let env = TestEnv::new().await;

    // Empty string is the one URL the normalizer can't rescue — after trim
    // it's still empty (no scheme to inject) and validate rejects it.
    let result = env
        .service
        .add_subscription(NewSubscription {
            url: " ".into(),
            ..Default::default()
        })
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        AppError::Validation(msg) => {
            assert!(msg.contains("URL cannot be empty"));
        }
        other => panic!("Expected Validation error, got: {other}"),
    }
}

#[tokio::test]
async fn test_create_subscription_invalid_url_fails() {
    let env = TestEnv::new().await;

    let result = env
        .service
        .add_subscription(NewSubscription {
            url: "ftp://not-allowed.com".into(),
            ..Default::default()
        })
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        AppError::Validation(msg) => {
            assert!(msg.contains("http"));
        }
        other => panic!("Expected Validation error, got: {other}"),
    }
}

// ---------------------------------------------------------------------------
// READ
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_list_subscriptions() {
    let env = TestEnv::new().await;

    // No subscriptions yet
    let subs = env.service.list_subscriptions().await.unwrap();
    assert!(subs.is_empty());

    // Add two
    env.service
        .add_subscription(new_sub("https://a.com/rss"))
        .await
        .unwrap();
    env.service
        .add_subscription(new_sub("https://b.com/rss"))
        .await
        .unwrap();

    let subs = env.service.list_subscriptions().await.unwrap();
    assert_eq!(subs.len(), 2);
}

#[tokio::test]
async fn test_get_subscription_by_id() {
    let env = TestEnv::new().await;

    let created = env
        .service
        .add_subscription(NewSubscription {
            url: "https://example.com/rss".into(),
            title: Some("Example".into()),
            ..Default::default()
        })
        .await
        .unwrap();

    let fetched = env.service.get_subscription(created.id).await.unwrap();
    assert_eq!(fetched.id, created.id);
    assert_eq!(fetched.url, created.url);
    assert_eq!(fetched.title, created.title);
}

#[tokio::test]
async fn test_get_subscription_not_found() {
    let env = TestEnv::new().await;

    let result = env.service.get_subscription(99999).await;
    assert!(result.is_err());
    match result.unwrap_err() {
        AppError::NotFound(msg) => assert!(msg.contains("99999")),
        other => panic!("Expected NotFound error, got: {other}"),
    }
}

// ---------------------------------------------------------------------------
// UPDATE
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_update_subscription_title() {
    let env = TestEnv::new().await;

    let sub = env
        .service
        .add_subscription(new_sub("https://example.com/rss"))
        .await
        .unwrap();

    let updated = env
        .service
        .update_subscription(
            sub.id,
            UpdateSubscription {
                title: Some("Updated Title".into()),
                website_url: None,
                use_website: None,
                rsshub_url: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(updated.title, Some("Updated Title".into()));
    // unchanged fields preserved
    assert_eq!(updated.url, sub.url);
}

#[tokio::test]
async fn test_update_subscription_preserves_unchanged_fields() {
    let env = TestEnv::new().await;

    let sub = env
        .service
        .add_subscription(NewSubscription {
            url: "https://example.com/rss".into(),
            title: Some("Original Title".into()),
            website_url: Some("https://example.com".into()),
            use_website: true,
            ..Default::default()
        })
        .await
        .unwrap();

    let updated = env
        .service
        .update_subscription(
            sub.id,
            UpdateSubscription {
                title: Some("New Title".into()),
                website_url: None, // unchanged
                use_website: None,  // unchanged
                rsshub_url: None,   // unchanged
            },
        )
        .await
        .unwrap();

    assert_eq!(updated.title, Some("New Title".into()));
    assert_eq!(updated.website_url, Some("https://example.com".into()));
    assert!(updated.use_website);
}

#[tokio::test]
async fn test_update_subscription_not_found() {
    let env = TestEnv::new().await;

    let result = env
        .service
        .update_subscription(
            99999,
            UpdateSubscription {
                title: Some("Nope".into()),
                website_url: None,
                use_website: None,
                rsshub_url: None,
            },
        )
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        AppError::NotFound(msg) => assert!(msg.contains("99999")),
        other => panic!("Expected NotFound error, got: {other}"),
    }
}

#[tokio::test]
async fn test_add_subscription_rejects_malformed_http_url() {
    let env = TestEnv::new().await;

    for url in ["http://?", "https://#fragment", "http://"] {
        let result = env.service.add_subscription(new_sub(url)).await;
        assert!(
            matches!(result, Err(AppError::Validation(_))),
            "expected validation error for {url:?}"
        );
    }
}

#[tokio::test]
async fn test_update_subscription_rejects_malformed_optional_http_url() {
    let env = TestEnv::new().await;
    let sub = env
        .service
        .add_subscription(new_sub("https://example.com/rss"))
        .await
        .unwrap();

    let result = env
        .service
        .update_subscription(
            sub.id,
            UpdateSubscription {
                title: None,
                website_url: Some(Some("http://?".into())),
                use_website: None,
                rsshub_url: None,
            },
        )
        .await;

    assert!(matches!(result, Err(AppError::Validation(msg)) if msg.contains("website_url")));
}

#[tokio::test]
async fn test_update_subscription_invalid_website_url_fails() {
    let env = TestEnv::new().await;

    let sub = env
        .service
        .add_subscription(new_sub("https://example.com/rss"))
        .await
        .unwrap();

    let result = env
        .service
        .update_subscription(
            sub.id,
            UpdateSubscription {
                title: None,
                website_url: Some(Some("not-a-url".into())),
                use_website: None,
                rsshub_url: None,
            },
        )
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        AppError::Validation(msg) => assert!(msg.contains("website_url")),
        other => panic!("Expected Validation error, got: {other}"),
    }
}

// ---------------------------------------------------------------------------
// DELETE
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_delete_subscription() {
    let env = TestEnv::new().await;

    let sub = env
        .service
        .add_subscription(new_sub("https://example.com/rss"))
        .await
        .unwrap();

    // Delete succeeds
    env.service.remove_subscription(sub.id).await.unwrap();

    // Get should now fail with NotFound
    let result = env.service.get_subscription(sub.id).await;
    assert!(matches!(result.unwrap_err(), AppError::NotFound(_)));
}

#[tokio::test]
async fn test_delete_subscription_not_found() {
    let env = TestEnv::new().await;

    let result = env.service.remove_subscription(99999).await;
    assert!(result.is_err());
    match result.unwrap_err() {
        AppError::NotFound(msg) => assert!(msg.contains("99999")),
        other => panic!("Expected NotFound error, got: {other}"),
    }
}

// ---------------------------------------------------------------------------
// TOGGLE
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_toggle_use_website() {
    let env = TestEnv::new().await;

    let sub = env
        .service
        .add_subscription(new_sub("https://example.com/rss"))
        .await
        .unwrap();
    assert!(!sub.use_website);

    let toggled = env.service.toggle_use_website(sub.id).await.unwrap();
    assert!(toggled.use_website);

    let toggled_back = env.service.toggle_use_website(sub.id).await.unwrap();
    assert!(!toggled_back.use_website);
}

#[tokio::test]
async fn test_toggle_auto_classify() {
    let env = TestEnv::new().await;

    let sub = env
        .service
        .add_subscription(new_sub("https://example.com/rss"))
        .await
        .unwrap();
    assert!(sub.auto_classify); // default true

    let toggled = env.service.toggle_auto_classify(sub.id).await.unwrap();
    assert!(!toggled.auto_classify);

    let toggled_back = env.service.toggle_auto_classify(sub.id).await.unwrap();
    assert!(toggled_back.auto_classify);
}

#[tokio::test]
async fn test_toggle_not_found() {
    let env = TestEnv::new().await;

    let result = env.service.toggle_use_website(99999).await;
    assert!(matches!(result.unwrap_err(), AppError::NotFound(_)));
}

// ---------------------------------------------------------------------------
// REPOSITORY DIRECT: EXISTS_BY_URL
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_exists_by_url() {
    let env = TestEnv::new().await;

    assert!(!env.repo.exists_by_url("https://example.com/rss").await.unwrap());

    env.service
        .add_subscription(new_sub("https://example.com/rss"))
        .await
        .unwrap();

    assert!(env.repo.exists_by_url("https://example.com/rss").await.unwrap());
}

// ---------------------------------------------------------------------------
// URL NORMALIZATION ON ADD
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_add_subscription_trims_url() {
    let env = TestEnv::new().await;

    let sub = env
        .service
        .add_subscription(new_sub("  https://example.com/trimmed.xml  "))
        .await
        .expect("padded URL should be accepted");

    // The stored URL must be the normalized one, not the raw input —
    // otherwise the later fetch would fail and dedup would be bypassed.
    assert_eq!(sub.url, "https://example.com/trimmed.xml");
}

#[tokio::test]
async fn test_add_subscription_adds_missing_scheme() {
    let env = TestEnv::new().await;

    let sub = env
        .service
        .add_subscription(new_sub("example.com/rss.xml"))
        .await
        .expect("scheme-less URL should be accepted");

    assert_eq!(sub.url, "https://example.com/rss.xml");
}

#[tokio::test]
async fn test_add_subscription_dedup_after_normalization() {
    let env = TestEnv::new().await;

    env.service
        .add_subscription(new_sub("https://example.com/dup.xml"))
        .await
        .unwrap();

    // Same feed, padded + scheme-less spelling — must still be a duplicate.
    let result = env
        .service
        .add_subscription(new_sub("  example.com/dup.xml "))
        .await;
    match result {
        Err(AppError::Duplicate(_)) => {}
        other => panic!("Expected Duplicate after normalization, got {:?}", other),
    }
}

#[tokio::test]
async fn test_add_subscription_validates_optional_urls() {
    let env = TestEnv::new().await;

    let mut input = new_sub("https://example.com/feed.xml");
    input.website_url = Some("not-a-url".into());
    let result = env.service.add_subscription(input).await;
    match result {
        Err(AppError::Validation(msg)) => assert!(msg.contains("website_url")),
        other => panic!("Expected Validation error, got {:?}", other),
    }

    // Empty optional URL normalizes to None instead of failing.
    let mut input = new_sub("https://example.com/feed.xml");
    input.website_url = Some("   ".into());
    let sub = env
        .service
        .add_subscription(input)
        .await
        .expect("blank website_url should become None");
    assert_eq!(sub.website_url, None);
}

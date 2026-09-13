//! IPC contract tests.
//!
//! These run the REAL command handlers through Tauri's mock runtime — no
//! window, no webview process — and assert the wire contract the frontend
//! depends on. This is the layer the review found broken (the add-feed form
//! sent `website_url` while the command expects `websiteUrl`), and it is the
//! only test level that can catch that class of bug: the frontend mock proves
//! what the app *sends*, these prove what the backend *accepts*.
//!
//! Run with `cargo test --manifest-path src-tauri/Cargo.toml ipc_contract`.

use std::sync::Arc;

use serde_json::{json, Value};
use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::{Manager, WebviewWindow, WebviewWindowBuilder};

use crate::ai::activity::AiActivityStore;
use crate::ai::service::SharedAiService;
use crate::chroma::ChromaHolder;
use crate::commands::AppState;
use crate::feed::FeedFetcher;
use crate::repositories::feed_item_repo::SqliteFeedItemRepository;
use crate::repositories::job_repo::{JobRepository, SqliteJobRepository};
use crate::repositories::subscription_repo::SqliteSubscriptionRepository;
use crate::repositories::{FeedItemRepository, SubscriptionRepository};
use crate::services::{FeedService, SubscriptionService, TagMatcher};

/// A mock-runtime app plus its webview. Both must stay alive while invoking.
struct Harness {
    _app: tauri::App<tauri::test::MockRuntime>,
    webview: WebviewWindow<tauri::test::MockRuntime>,
}

/// Build the real application state on an in-memory database.
fn test_state() -> AppState {
    let pool = tauri::async_runtime::block_on(async {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .foreign_keys(true),
            )
            .await
            .expect("in-memory database");
        crate::database::migrations::run_migrations(&pool)
            .await
            .expect("migrations");
        pool
    });

    let sub_repo: Arc<dyn SubscriptionRepository> =
        Arc::new(SqliteSubscriptionRepository::new(pool.clone()));
    let feed_repo: Arc<dyn FeedItemRepository> =
        Arc::new(SqliteFeedItemRepository::new(pool.clone()));
    let jobs: Arc<dyn JobRepository> = Arc::new(SqliteJobRepository::new(pool.clone()));
    let fetcher = Arc::new(FeedFetcher::new().expect("http client"));
    let ai_service: SharedAiService = Arc::new(tokio::sync::RwLock::new(None));

    AppState {
        subscription_service: SubscriptionService::new(sub_repo),
        feed_service: FeedService::new(feed_repo.clone())
            .with_fetcher(fetcher.clone())
            .with_ai_service(ai_service.clone())
            .with_chroma_service(ChromaHolder::default()),
        feed_repo,
        jobs,
        fetcher,
        ai_service,
        ai_activity: AiActivityStore::new(),
        chroma_service: ChromaHolder::default(),
        tag_matcher: Arc::new(TagMatcher::local()),
    }
}

impl Harness {
    /// One app + one database, so calls in a test share state.
    fn new() -> Self {
        let app = mock_builder()
            .invoke_handler(tauri::generate_handler![
                crate::commands::add_subscription,
                crate::commands::list_subscriptions,
                crate::commands::get_items,
                crate::commands::get_item,
                crate::commands::toggle_favorite,
                crate::commands::toggle_read_later,
                crate::commands::mark_item_read,
                crate::commands::save_item_tags,
                crate::commands::get_job_stats,
                crate::commands::chroma_index_status,
            ])
            .build(mock_context(noop_assets()))
            .expect("build mock app");
        app.manage(test_state());
        let webview = WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("mock webview");
        Self {
            _app: app,
            webview,
        }
    }

    /// Create an article through the real ingest path (article + jobs in one
    /// transaction) and return its id.
    fn seed_article(&self, title: &str) -> i64 {
        let state = self._app.state::<AppState>();
        let created = tauri::async_runtime::block_on(async {
            state
                .feed_repo
                .create_with_jobs(
                    crate::models::NewFeedItem {
                        subscription_id: 1,
                        guid: Some(title.to_string()),
                        title: title.to_string(),
                        link: Some(format!("https://example.com/{title}")),
                        content: Some("<p>Body</p>".into()),
                        ..Default::default()
                    },
                    vec![crate::models::NewJob::chroma_upsert(None)],
                )
                .await
                .expect("seed article")
        });
        created.id
    }

    /// Invoke a command exactly as the webview would.
    fn invoke(&self, cmd: &str, body: Value) -> Value {
        let request = InvokeRequest {
            cmd: cmd.into(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body: body.into(),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        };
        get_ipc_response(&self.webview, request)
            .unwrap_or_else(|error| panic!("{cmd} failed: {error}"))
            .deserialize::<Value>()
            .expect("json response")
    }

    fn try_invoke(&self, cmd: &str, body: Value) -> Result<Value, Value> {
        let request = InvokeRequest {
            cmd: cmd.into(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body: body.into(),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        };
        get_ipc_response(&self.webview, request).map(|body| {
            body.deserialize::<Value>().expect("json response")
        })
    }
}

#[test]
fn add_subscription_accepts_camel_case_option_keys() {
    let app = Harness::new();

    // Exactly what the frontend sends through `subscriptions.add`.
    let created = app.invoke(
        "add_subscription",
        json!({
            "url": "https://example.com/feed",
            "websiteUrl": "https://example.com",
            "rsshubUrl": "https://rsshub.app/test",
            "useWebsite": true,
        }),
    );

    assert_eq!(created["url"], "https://example.com/feed");
    assert_eq!(
        created["website_url"], "https://example.com",
        "camelCase websiteUrl must reach the stored row"
    );
    assert_eq!(created["rsshub_url"], "https://rsshub.app/test");
    assert_eq!(created["use_website"], true);

    // Same app, same database: the row is really there.
    let listed = app.invoke("list_subscriptions", json!({}));
    assert_eq!(listed.as_array().map(|rows| rows.len()), Some(1));
}

/// Pins the failure mode the review reproduced: snake_case option keys are not
/// part of this command's contract. If this ever starts passing, the wire
/// format changed and the frontend adapter must be revisited.
#[test]
fn add_subscription_ignores_snake_case_option_keys() {
    let app = Harness::new();

    let created = app.invoke(
        "add_subscription",
        json!({
            "url": "https://snake.example.com/feed",
            "website_url": "https://snake.example.com",
            "use_website": true,
        }),
    );

    assert_eq!(created["url"], "https://snake.example.com/feed");
    assert!(
        created["website_url"].is_null(),
        "snake_case website_url must NOT be accepted: {created}"
    );
    assert_eq!(created["use_website"], false);
}

#[test]
fn list_paging_forwards_limit_and_offset() {
    let app = Harness::new();
    app.invoke(
        "add_subscription",
        json!({ "url": "https://example.com/feed" }),
    );
    for i in 0..5 {
        app.seed_article(&format!("article-{i}"));
    }

    // The frontend's "load more" path sends an offset and reads the page size.
    let first = app.invoke("get_items", json!({ "limit": 2, "offset": 0 }));
    let second = app.invoke("get_items", json!({ "limit": 2, "offset": 2 }));

    let ids = |value: &Value| -> Vec<i64> {
        value
            .as_array()
            .expect("page is an array")
            .iter()
            .map(|row| row["id"].as_i64().expect("id"))
            .collect()
    };
    assert_eq!(ids(&first).len(), 2);
    assert_eq!(ids(&second).len(), 2);
    assert!(
        ids(&first).iter().all(|id| !ids(&second).contains(id)),
        "pages must not overlap: {first} vs {second}"
    );

    // Projections stay light: the list must not ship full article bodies.
    assert!(first[0].get("content").is_none());
    assert!(first[0].get("content_md").is_none());
    assert!(first[0]["has_translation"].is_boolean());
}

#[test]
fn flag_commands_return_the_new_state_and_persist_it() {
    let app = Harness::new();
    app.invoke(
        "add_subscription",
        json!({ "url": "https://example.com/feed" }),
    );
    let id = app.seed_article("flagged");

    assert_eq!(app.invoke("toggle_favorite", json!({ "itemId": id })), json!(true));
    assert_eq!(app.invoke("toggle_read_later", json!({ "itemId": id })), json!(true));

    let stored = app.invoke("get_item", json!({ "id": id }));
    assert_eq!(stored["is_favorite"], true);
    assert_eq!(stored["is_read_later"], true);

    // Toggling back is idempotent in the other direction.
    assert_eq!(app.invoke("toggle_favorite", json!({ "itemId": id })), json!(false));
    let stored = app.invoke("get_item", json!({ "id": id }));
    assert_eq!(stored["is_favorite"], false);
}

#[test]
fn marking_read_accepts_the_frontends_argument_names() {
    let app = Harness::new();
    app.invoke(
        "add_subscription",
        json!({ "url": "https://example.com/feed" }),
    );
    let id = app.seed_article("read-me");

    // markAsRead(id, false) — the "translating un-reads the article" path.
    let updated = app.invoke("mark_item_read", json!({ "itemId": id, "isRead": false }));
    assert_eq!(updated["is_read"], false);
    assert_eq!(app.invoke("get_item", json!({ "id": id }))["is_read"], false);

    let updated = app.invoke("mark_item_read", json!({ "itemId": id, "isRead": true }));
    assert_eq!(updated["is_read"], true);
}

#[test]
fn save_tags_returns_the_normalized_backend_row() {
    let app = Harness::new();
    app.invoke(
        "add_subscription",
        json!({ "url": "https://example.com/feed" }),
    );
    let id = app.seed_article("tagged");

    // The backend owns normalization: the frontend renders what comes back.
    let saved = app.invoke(
        "save_item_tags",
        json!({ "itemId": id, "tags": ["Machine Learning", "machine learning", "AI"] }),
    );

    let tags: Vec<String> =
        serde_json::from_str(saved["tags"].as_str().expect("tags json")).expect("tags array");
    assert!(
        tags.len() <= 3 && tags.iter().all(|t| t == &t.to_lowercase()),
        "tags must come back normalized and capped: {tags:?}"
    );
}

#[test]
fn get_job_stats_serializes_the_queue_shape_the_frontend_reads() {
    let app = Harness::new();
    let stats = app.invoke("get_job_stats", json!({}));
    for field in ["queued", "running", "failed", "succeeded"] {
        assert!(
            stats.get(field).and_then(|v| v.as_i64()).is_some(),
            "JobStats.{field} must be a number, got {stats}"
        );
    }
    assert!(stats["recent_errors"].is_array());
}

/// The queue is visible through IPC: creating an article queues indexing, and
/// the stats command reports it. This is the read path the status bar uses.
/// The settings panel reads one snapshot: it must be present and well-typed
/// even on a fresh library with semantic search switched off.
///
/// Only shape and repository-derived values are asserted: the watermark,
/// pending queues and collection id live in `~/.rss-reader/chroma_sync.json`,
/// so asserting their values would make this test depend on whoever runs it.
#[test]
fn index_status_shape_is_stable() {
    let app = Harness::new();
    let status = app.invoke("chroma_index_status", json!({}));

    for field in [
        "indexed",
        "total",
        "queued_jobs",
        "pending_upserts",
        "pending_deletes",
        "done",
        "scan_total",
    ] {
        assert!(
            status.get(field).and_then(|v| v.as_i64()).is_some(),
            "IndexStatus.{field} must be a number, got {status}"
        );
    }
    assert!(status["enabled"].is_boolean(), "{status}");
    assert!(status["running"].is_boolean(), "{status}");
    assert!(status["phase"].is_string(), "{status}");
    assert!(status["collection_name"].is_string(), "{status}");
    assert!(
        status["collection_id"].is_null() || status["collection_id"].is_string(),
        "{status}"
    );
    // Comes from this harness's empty in-memory database.
    assert_eq!(status["total"], 0, "an empty library has no items to index");
}

#[test]
fn statistics_reflect_queued_enrichment() {
    let app = Harness::new();
    app.invoke(
        "add_subscription",
        json!({ "url": "https://example.com/feed" }),
    );
    app.seed_article("queued");

    let stats = app.invoke("get_job_stats", json!({}));
    assert_eq!(stats["queued"], 1);
    assert_eq!(stats["failed"], 0);
}

#[test]
fn unknown_command_is_reported_not_silently_ignored() {
    let app = Harness::new();
    assert!(app.try_invoke("does_not_exist", json!({})).is_err());
}

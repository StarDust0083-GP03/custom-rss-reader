//! Integration tests for the ingest → job → worker pipeline.
//!
//! These cover the part of the refactor that has no UI: the article and its
//! enrichment jobs are committed together, the worker claims them by lease,
//! and each handler either finishes its job or leaves a retryable failure.
//! They run against a real in-memory SQLite database with the production
//! migrations, so the SQL here is the same SQL the app runs.

use std::sync::Arc;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::ai::activity::AiActivityStore;
use crate::ai::service::AiService;
use crate::chroma::ChromaHolder;
use crate::models::job::{JobKind, NewJob};
use crate::models::NewFeedItem;
use crate::repositories::feed_item_repo::SqliteFeedItemRepository;
use crate::repositories::job_repo::{JobRepository, SqliteJobRepository};
use crate::repositories::{FeedItemRepository, IndexRow};
use crate::services::{JobWorker, TagMatcher};
use crate::tests::helpers::FakeAi;

async fn pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(":memory:")
                .foreign_keys(true),
        )
        .await
        .expect("in-memory database");
    crate::database::migrations::run_migrations(&pool)
        .await
        .expect("migrations");
    sqlx::query("INSERT INTO subscriptions (id, url) VALUES (1, 'https://example.com/feed')")
        .execute(&pool)
        .await
        .expect("seed subscription");
    pool
}

fn new_item(guid: &str) -> NewFeedItem {
    NewFeedItem {
        subscription_id: 1,
        guid: Some(guid.to_string()),
        title: format!("Article {guid}"),
        link: Some(format!("https://example.com/{guid}")),
        content: Some("<p>Body</p>".to_string()),
        ..Default::default()
    }
}

/// Worker with explicit dependencies, so tests never read the developer's real
/// config files and always know which services are available.
/// Shared constructor: explicit Chroma endpoint, explicit AI loader.
///
/// The Chroma endpoint points at a port nothing can be listening on, and the
/// AI loader is injected, so a test can never reach the developer's live
/// services — or trigger a 120 MB model download — by accident.
fn build(
    jobs: Arc<dyn JobRepository>,
    repo: Arc<dyn FeedItemRepository>,
    chroma_enabled: bool,
    ai_loader: crate::services::job_worker::AiLoader,
) -> JobWorker {
    let chroma = ChromaHolder::with_config(crate::chroma::ChromaConfig {
        host: "http://127.0.0.1".into(),
        port: 9,
        collection_name: "test".into(),
        enabled: chroma_enabled,
    });
    JobWorker::new(
        jobs,
        repo,
        Arc::new(tokio::sync::RwLock::new(None)),
        AiActivityStore::new(),
        None, // no HTTP fetcher: the website handler is not exercised here
        chroma,
        Arc::new(TagMatcher::local()),
    )
    .with_ai_loader(ai_loader)
}

/// Classification available, indexing on (the common production shape).
fn worker(
    jobs: Arc<dyn JobRepository>,
    repo: Arc<dyn FeedItemRepository>,
    ai: Option<Arc<dyn AiService>>,
) -> JobWorker {
    build(jobs, repo, true, loader_for(ai))
}

/// Worker with an explicit Chroma switch.
fn worker_with(
    jobs: Arc<dyn JobRepository>,
    repo: Arc<dyn FeedItemRepository>,
    ai: Option<Arc<dyn AiService>>,
    chroma_enabled: bool,
) -> JobWorker {
    build(jobs, repo, chroma_enabled, loader_for(ai))
}

/// Indexing switched off, as on a machine that never enabled semantic search.
fn worker_without_chroma(
    jobs: Arc<dyn JobRepository>,
    repo: Arc<dyn FeedItemRepository>,
) -> JobWorker {
    build(jobs, repo, false, Arc::new(|| None))
}

/// A worker whose AI service only appears on the second lookup — the shape of
/// "the user configured AI while the app was running".
fn worker_with_deferred_ai(
    jobs: Arc<dyn JobRepository>,
    repo: Arc<dyn FeedItemRepository>,
    ai: Arc<dyn AiService>,
) -> JobWorker {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let loader = Arc::new(move || {
        if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            None
        } else {
            Some(ai.clone())
        }
    });
    build(jobs, repo, false, loader)
}

/// An always-ready loader for the injected fake.
fn loader_for(ai: Option<Arc<dyn AiService>>) -> crate::services::job_worker::AiLoader {
    Arc::new(move || ai.clone())
}

// ---------------------------------------------------------------------------
// Failure classification
// ---------------------------------------------------------------------------

/// Why this matters: a dead article URL or a JavaScript-only page used to be
/// retried five times, so a bulk refresh printed thousands of identical lines
/// and spent the whole retry budget on work that can never succeed.
#[test]
fn permanent_failures_are_recognised() {
    use crate::error::AppError;
    use crate::services::job_worker::is_permanent_failure;

    let website = crate::models::job::JobKind::WebsiteMarkdown.as_str();
    let classify = crate::models::job::JobKind::Classify.as_str();

    // Gone for good.
    for message in [
        "Network error: HTTP status: 404 Not Found",
        "Network error: HTTP status: 410 Gone",
        "Network error: HTTP 404 Not Found",
        "Operation failed: No main content found in HTML",
    ] {
        assert!(
            is_permanent_failure(website, &AppError::Network(message.into())),
            "{message} should be permanent"
        );
    }

    // Worth another attempt: auth/UA rotation, throttling, server errors,
    // timeouts, oversized bodies.
    for message in [
        "Network error: HTTP status: 403 Forbidden",
        "Network error: HTTP status: 429 Too Many Requests",
        "Network error: HTTP status: 408 Request Timeout",
        "Network error: HTTP status: 500 Internal Server Error",
        "Network error: Failed to read response body: connection reset",
        "Network error: response exceeds the 8388608 byte limit",
    ] {
        assert!(
            !is_permanent_failure(website, &AppError::Network(message.into())),
            "{message} should be retryable"
        );
    }

    // Other kinds are transient by nature: a provider outage or a Chroma
    // restart must not park an article's classification forever.
    assert!(!is_permanent_failure(
        classify,
        &AppError::Network("HTTP status: 404 Not Found".into())
    ));
}

#[test]
fn http_status_is_read_from_both_message_shapes() {
    use crate::services::job_worker::http_status_in;
    assert_eq!(http_status_in("Network error: HTTP status: 404 Not Found"), Some(404));
    assert_eq!(http_status_in("Network error: HTTP 503 Service Unavailable"), Some(503));
    assert_eq!(http_status_in("Network error: connection refused"), None);
}

/// Identical problems must collapse into one counter line.
#[test]
fn failure_reasons_drop_the_varying_parts() {
    use crate::services::job_worker::shorten_reason;
    assert_eq!(
        shorten_reason("Network error: HTTP status: 404 Not Found"),
        "Network error: HTTP status: 404 Not Found"
    );
    assert_eq!(
        shorten_reason("Website fetch failed for https://a.example.com/x (item 42)"),
        "Website fetch failed for https://a.example.com/x"
    );
}

// ---------------------------------------------------------------------------
// Commit-first
// ---------------------------------------------------------------------------

/// Regression: the ingest path used to hand a `0` placeholder to the job
/// constructors. Every enrichment job then pointed at article id 0, the worker
/// could not load it, and it completed the job as "not found" — so
/// classification, website caching, and indexing silently never ran for any
/// newly fetched article.
#[test]
fn new_article_jobs_do_not_carry_a_placeholder_article_id() {
    use crate::services::feed_service::jobs_for_new_article;

    let subscription = crate::models::Subscription {
        id: 7,
        url: "https://example.com/feed".into(),
        title: None,
        website_url: None,
        rsshub_url: None,
        use_website: true,
        auto_classify: true,
        opml_attributes: None,
        http_etag: None,
        http_last_modified: None,
        created_at: "2024-01-01T00:00:00Z".parse().unwrap(),
        updated_at: "2024-01-01T00:00:00Z".parse().unwrap(),
    };

    let jobs = jobs_for_new_article(&subscription, Some("https://example.com/a".into()), true);
    assert_eq!(jobs.len(), 3);
    for job in &jobs {
        assert_eq!(
            job.item_id, None,
            "{:?} must be committed for the article being inserted, not a placeholder id",
            job.kind
        );
    }
    assert!(jobs.iter().any(|j| j.kind == JobKind::Classify));
    assert!(jobs.iter().any(|j| j.kind == JobKind::ChromaUpsert));
    assert!(jobs.iter().any(|j| j.kind == JobKind::WebsiteMarkdown));

    // Feature switches still gate the expensive work.
    let quiet = crate::models::Subscription {
        use_website: false,
        auto_classify: false,
        ..subscription.clone()
    };
    let jobs = jobs_for_new_article(&quiet, Some("https://example.com/a".into()), true);
    assert_eq!(jobs.len(), 1, "only indexing remains");
    assert_eq!(jobs[0].kind, JobKind::ChromaUpsert);
}

/// A job carrying a stale 0 (rows written before the fix) must be repaired by
/// the repository rather than silently pointing at nothing.
#[tokio::test]
async fn repository_repairs_a_zero_item_id_placeholder() {
    let pool = pool().await;
    let repo = SqliteFeedItemRepository::new(pool.clone());
    let jobs = SqliteJobRepository::new(pool.clone());

    let mut job = NewJob::classify(None);
    job.item_id = Some(0); // exactly what the old constructors produced
    let item = repo
        .create_with_jobs(new_item("a"), vec![job])
        .await
        .expect("create");

    let claimed = jobs.claim_batch(10, &["classify"]).await.expect("claim");
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].item_id, Some(item.id));
}

#[tokio::test]
async fn article_and_jobs_are_committed_together() {
    let pool = pool().await;
    let repo = SqliteFeedItemRepository::new(pool.clone());
    let jobs = SqliteJobRepository::new(pool.clone());

    let item = repo
        .create_with_jobs(
            new_item("a"),
            vec![
                NewJob::classify(None),
                NewJob::website_markdown(None, "https://example.com/a".into()),
                NewJob::chroma_upsert(None),
            ],
        )
        .await
        .expect("create with jobs");

    let stats = jobs.stats().await.expect("stats");
    assert_eq!(stats.queued, 3, "all three jobs must be queued");
    assert_eq!(stats.running, 0);

    // Jobs carry the article id and the priority/max-attempt policy of their
    // kind, so the worker can order and bound them.
    let claimed = jobs.claim_batch(10, &["website_markdown"]).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].item_id, Some(item.id));
    assert_eq!(claimed[0].priority, JobKind::WebsiteMarkdown.priority());
    assert_eq!(claimed[0].max_attempts, JobKind::WebsiteMarkdown.max_attempts());
}

#[tokio::test]
async fn duplicate_article_leaves_no_orphan_jobs() {
    let pool = pool().await;
    let repo = SqliteFeedItemRepository::new(pool.clone());
    let jobs = SqliteJobRepository::new(pool.clone());

    repo.create_with_jobs(new_item("dup"), vec![NewJob::classify(None)])
        .await
        .expect("first insert");

    // Same (subscription_id, guid): the insert is refused…
    let error = repo
        .create_with_jobs(new_item("dup"), vec![NewJob::chroma_upsert(None)])
        .await
        .expect_err("duplicate must be rejected");
    assert!(matches!(error, crate::error::AppError::Duplicate(_)));

    // …and the transaction rolled back, so no job points at a row that was
    // never created. A queue entry for a missing article would be retried
    // until its attempt budget ran out.
    let stats = jobs.stats().await.expect("stats");
    assert_eq!(stats.queued, 1, "only the first article's job exists");
}

// ---------------------------------------------------------------------------
// Worker handlers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn worker_classifies_a_whole_claim_in_one_call() {
    let pool = pool().await;
    let repo: Arc<dyn FeedItemRepository> = Arc::new(SqliteFeedItemRepository::new(pool.clone()));
    let jobs: Arc<dyn JobRepository> = Arc::new(SqliteJobRepository::new(pool.clone()));

    let a = repo.create_with_jobs(new_item("a"), vec![NewJob::classify(None)]).await.unwrap();
    let b = repo.create_with_jobs(new_item("b"), vec![NewJob::classify(None)]).await.unwrap();

    let ai = FakeAi::ok();
    let worker = worker(jobs.clone(), repo.clone(), Some(ai.clone()));
    assert!(worker.tick().await.expect("tick"), "there was queued work");

    assert_eq!(
        ai.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "two titles in one claim must cost one model call"
    );

    for id in [a.id, b.id] {
        let stored = repo.find_by_id(id).await.expect("item");
        assert_eq!(stored.tags.as_deref(), Some(r#"["machine_learning"]"#));
    }

    let stats = jobs.stats().await.expect("stats");
    assert_eq!(stats.succeeded, 2);
    assert_eq!(stats.failed, 0);
    assert_eq!(stats.queued, 0);
}

#[tokio::test]
async fn worker_failure_requeues_with_backoff_instead_of_losing_the_job() {
    let pool = pool().await;
    let repo: Arc<dyn FeedItemRepository> = Arc::new(SqliteFeedItemRepository::new(pool.clone()));
    let jobs: Arc<dyn JobRepository> = Arc::new(SqliteJobRepository::new(pool.clone()));

    repo.create_with_jobs(new_item("a"), vec![NewJob::classify(None)])
        .await
        .unwrap();

    let worker = worker(jobs.clone(), repo.clone(), Some(FakeAi::failing()));
    assert!(worker.tick().await.expect("tick"));

    let stats = jobs.stats().await.expect("stats");
    assert_eq!(stats.queued, 1, "the job goes back to the queue");
    assert_eq!(stats.failed, 0);
    assert_eq!(stats.running, 0, "the lease must be released");
    assert!(
        stats.recent_errors.iter().any(|e| e.contains("provider down")),
        "the provider error must be recorded: {:?}",
        stats.recent_errors
    );

    // The item is untouched: no half-written tags from a failed batch.
    let item = repo
        .find_by_id(1)
        .await
        .expect("item still exists without tags");
    assert!(item.tags.is_none());
}

#[tokio::test]
async fn unconfigured_ai_leaves_classification_queued_instead_of_failing_it() {
    let pool = pool().await;
    let repo: Arc<dyn FeedItemRepository> = Arc::new(SqliteFeedItemRepository::new(pool.clone()));
    let jobs: Arc<dyn JobRepository> = Arc::new(SqliteJobRepository::new(pool.clone()));

    repo.create_with_jobs(new_item("a"), vec![NewJob::classify(None)])
        .await
        .unwrap();

    let waiting = worker(jobs.clone(), repo.clone(), None);
    // Nothing is claimable: retrying a job whose dependency is missing would
    // burn its attempt budget and bury the real work in failures.
    for _ in 0..3 {
        assert!(!waiting.tick().await.expect("tick"), "nothing claimable");
        sqlx::query("UPDATE jobs SET next_attempt_at = datetime('now', '-1 second')")
            .execute(&pool)
            .await
            .expect("advance clock");
    }

    let stats = jobs.stats().await.expect("stats");
    assert_eq!(stats.queued, 1, "the job waits for configuration");
    assert_eq!(stats.failed, 0, "waiting is not failing");
    assert_eq!(
        stats.queued_by_kind.get("classify").copied(),
        Some(1),
        "the command layer uses this to explain what is waiting"
    );

    // Once a service exists, the same job is claimed and completed.
    let active = worker(jobs.clone(), repo.clone(), Some(FakeAi::ok()));
    assert!(active.tick().await.expect("tick"));
    let stats = jobs.stats().await.expect("stats");
    assert_eq!(stats.succeeded, 1);
    assert_eq!(stats.queued, 0);
    let item = repo.find_by_id(1).await.expect("item");
    assert!(item.tags.is_some(), "the article is classified");
}

/// Configuring AI while the app runs must start classification without a
/// restart: the worker asks the loader, not just the slot filled at startup.
#[tokio::test]
async fn classification_starts_when_ai_is_configured_after_startup() {
    let pool = pool().await;
    let repo: Arc<dyn FeedItemRepository> = Arc::new(SqliteFeedItemRepository::new(pool.clone()));
    let jobs: Arc<dyn JobRepository> = Arc::new(SqliteJobRepository::new(pool.clone()));

    repo.create_with_jobs(new_item("a"), vec![NewJob::classify(None)])
        .await
        .unwrap();

    let worker = worker_with_deferred_ai(jobs.clone(), repo.clone(), FakeAi::ok());
    // First round: the loader still reports "not configured".
    assert!(!worker.tick().await.expect("tick"));
    assert_eq!(jobs.stats().await.expect("stats").queued, 1);
    // Second round: the configuration appeared, so the job runs.
    assert!(worker.tick().await.expect("tick"));
    let stats = jobs.stats().await.expect("stats");
    assert_eq!(stats.succeeded, 1, "{stats:?}");
    assert!(repo.find_by_id(1).await.expect("item").tags.is_some());
}

#[tokio::test]
async fn index_jobs_wait_for_the_feature_instead_of_being_discarded() {
    let pool = pool().await;
    let repo: Arc<dyn FeedItemRepository> = Arc::new(SqliteFeedItemRepository::new(pool.clone()));
    let jobs: Arc<dyn JobRepository> = Arc::new(SqliteJobRepository::new(pool.clone()));

    repo.create_with_jobs(new_item("a"), vec![NewJob::chroma_upsert(None)])
        .await
        .unwrap();

    // Semantic search off: the job is not claimable. Completing it would lose
    // the work — the watermark sync only walks *new* items, so an article that
    // was cached from its website after the watermark passed would never be
    // indexed with the richer text.
    let disabled = worker_without_chroma(jobs.clone(), repo.clone());
    assert!(!disabled.tick().await.expect("tick"));
    let stats = jobs.stats().await.expect("stats");
    assert_eq!(stats.queued, 1);
    assert_eq!(stats.succeeded, 0);
    assert_eq!(
        stats.queued_by_kind.get("chroma_upsert").copied(),
        Some(1)
    );

    // Switching the feature on makes the same job claimable. With no server
    // behind it the attempt fails, and it fails *retryably* — silently
    // completing it would leave the article unindexed forever.
    let enabled = worker_with(jobs.clone(), repo.clone(), None, true);
    assert!(enabled.tick().await.expect("tick"), "claimed once enabled");
    let stats = jobs.stats().await.expect("stats");
    assert_eq!(stats.succeeded, 0, "nothing was indexed: {stats:?}");
    assert_eq!(stats.running, 0, "the lease is released: {stats:?}");
    assert_eq!(stats.queued, 1, "the job is retryable: {stats:?}");
    assert!(
        stats
            .recent_errors
            .iter()
            .any(|error| error.contains("unreachable")),
        "the reason is recorded: {:?}",
        stats.recent_errors
    );
}

#[tokio::test]
async fn deleted_article_completes_its_jobs_instead_of_retrying_forever() {
    let pool = pool().await;
    let repo: Arc<dyn FeedItemRepository> = Arc::new(SqliteFeedItemRepository::new(pool.clone()));
    let jobs: Arc<dyn JobRepository> = Arc::new(SqliteJobRepository::new(pool.clone()));

    let item = repo
        .create_with_jobs(new_item("a"), vec![NewJob::classify(None)])
        .await
        .unwrap();
    // Cascade delete: the subscription removal path in production.
    sqlx::query("DELETE FROM feed_items WHERE id = $1")
        .bind(item.id)
        .execute(&pool)
        .await
        .expect("delete article");

    let worker = worker(jobs.clone(), repo.clone(), Some(FakeAi::ok()));
    assert!(worker.tick().await.expect("tick"));

    let stats = jobs.stats().await.expect("stats");
    assert_eq!(stats.succeeded, 1, "nothing to classify ⇒ nothing to retry");
    assert_eq!(stats.failed, 0);
}

#[tokio::test]
async fn worker_returns_false_when_the_queue_is_empty() {
    let pool = pool().await;
    let repo: Arc<dyn FeedItemRepository> = Arc::new(SqliteFeedItemRepository::new(pool.clone()));
    let jobs: Arc<dyn JobRepository> = Arc::new(SqliteJobRepository::new(pool.clone()));
    let worker = worker(jobs, repo, None);
    assert!(!worker.tick().await.expect("tick"));
}

// ---------------------------------------------------------------------------
// Index hydration (semantic search returns real rows, not synthesized ones)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hydrate_hits_keeps_ranking_and_drops_missing_articles() {
    let pool = pool().await;
    let repo: Arc<dyn FeedItemRepository> = Arc::new(SqliteFeedItemRepository::new(pool.clone()));

    let a = repo.create(new_item("a")).await.unwrap();
    let b = repo.create(new_item("b")).await.unwrap();
    let c = repo.create(new_item("c")).await.unwrap();

    let hits = vec![
        crate::chroma::service::SemanticSearchResult {
            item_id: c.id,
            title: "stale title from the index".into(),
            url: None,
            author: None,
            score: 0.9,
        },
        crate::chroma::service::SemanticSearchResult {
            item_id: 9999, // index lags a delete
            title: "ghost".into(),
            url: None,
            author: None,
            score: 0.8,
        },
        crate::chroma::service::SemanticSearchResult {
            item_id: a.id,
            title: "a".into(),
            url: None,
            author: None,
            score: 0.7,
        },
        crate::chroma::service::SemanticSearchResult {
            item_id: b.id,
            title: "b".into(),
            url: None,
            author: None,
            score: 0.6,
        },
    ];

    let rows = crate::commands::chroma_commands::hydrate_hits(&repo, hits)
        .await
        .expect("hydrate");

    assert_eq!(
        rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![c.id, a.id, b.id],
        "vector ranking preserved, missing ids dropped"
    );
    assert!(
        rows.iter().all(|r| r.subscription_id == 1),
        "rows carry real subscription identity, not 0"
    );
}

/// The index-row projection is what the sync loop embeds; it must stay in
/// sync with the columns the repository exposes.
#[tokio::test]
async fn index_rows_are_bounded_and_complete() {
    let pool = pool().await;
    let repo = SqliteFeedItemRepository::new(pool.clone());
    let long = "x".repeat(5000);
    let mut input = new_item("long");
    input.content = Some(long.clone());

    let item = repo.create(input).await.unwrap();
    let rows: Vec<IndexRow> = repo.find_index_rows_by_ids(&[item.id]).await.unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, item.id);
    assert!(
        rows[0].content.as_ref().map(|c| c.len()).unwrap_or(0) <= 2000,
        "index projection must truncate the body so a page stays bounded"
    );
}

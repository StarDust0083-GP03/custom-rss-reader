//! End-to-end ingest test: HTTP feed → parse → commit → enrich.
//!
//! This is the closest thing to "run the app" that works headlessly. A real
//! `FeedFetcher` talks to a local HTTP server, the real parser and repositories
//! run against a real (in-memory) SQLite database with the production
//! migrations, and the real worker drains the queue with a fake model behind
//! the `AiService` trait. No display, no traffic beyond loopback.
//!
//! It exists because the commit-first refactor moved the enrichment boundary:
//! unit tests on either side of that boundary stayed green while every queued
//! job pointed at article id 0. Only a test that crosses the boundary — fetch,
//! commit, claim, enrich, read back — catches that.

use std::sync::Arc;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::ai::activity::AiActivityStore;
use crate::ai::service::{AiService, SharedAiService};
use crate::chroma::ChromaHolder;
use crate::models::job::{JobKind, NewJob};
use crate::models::{NewFeedItem, NewSubscription, Subscription};
use crate::repositories::feed_item_repo::SqliteFeedItemRepository;
use crate::repositories::job_repo::{JobRepository, SqliteJobRepository};
use crate::repositories::subscription_repo::SqliteSubscriptionRepository;
use crate::repositories::{FeedItemRepository, SubscriptionRepository};
use crate::services::{FeedService, JobWorker, SubscriptionService, TagMatcher};
use crate::tests::helpers::FakeAi;

// ---------------------------------------------------------------------------
// Local HTTP feed server
// ---------------------------------------------------------------------------

/// Serve one RSS document on loopback so the real fetcher has something to
/// talk to (and can refetch it for retry assertions).
async fn serve_feed(body: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/rss+xml; charset=utf-8\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    format!("http://{addr}/feed.xml")
}

fn rss(items: &[(&str, &str)]) -> String {
    let entries: String = items
        .iter()
        .map(|(guid, title)| {
            format!(
                "<item><guid>{guid}</guid><title>{title}</title>\
                 <link>https://example.com/{guid}</link>\
                 <description><![CDATA[<p>Body of {title}</p>]]></description></item>"
            )
        })
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel><title>Local feed</title>{entries}</channel></rss>"#
    )
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Env {
    pool: SqlitePool,
    repo: Arc<dyn FeedItemRepository>,
    jobs: Arc<dyn JobRepository>,
    subscription: Subscription,
    service: FeedService,
    fetcher: Arc<crate::feed::FeedFetcher>,
}

async fn env(feed_url: &str, auto_classify: bool, use_website: bool) -> Env {
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

    let sub_repo: Arc<dyn SubscriptionRepository> =
        Arc::new(SqliteSubscriptionRepository::new(pool.clone()));
    let feed_repo: Arc<dyn FeedItemRepository> =
        Arc::new(SqliteFeedItemRepository::new(pool.clone()));
    let jobs: Arc<dyn JobRepository> = Arc::new(SqliteJobRepository::new(pool.clone()));
    let fetcher = Arc::new(crate::feed::FeedFetcher::new().expect("http client"));

    let subscription = SubscriptionService::new(sub_repo.clone())
        .add_subscription(NewSubscription {
            url: feed_url.to_string(),
            auto_classify,
            use_website,
            ..Default::default()
        })
        .await
        .expect("create subscription");

    let service = FeedService::new(feed_repo.clone())
        .with_subscription_repo(sub_repo)
        .with_fetcher(fetcher.clone())
        .with_ai_activity(AiActivityStore::new())
        // Explicit switch: the harness must never read the developer's real
        // `~/.rss-reader/chroma_config.json`.
        .with_chroma_service(ChromaHolder::with_enabled(false));

    Env {
        pool,
        repo: feed_repo,
        jobs,
        subscription,
        service,
        fetcher,
    }
}

impl Env {
    /// Jobs as `(id, kind, item_id)` so a test can prove the mapping without
    /// disturbing the queue.
    async fn job_rows(&self) -> Vec<(i64, String, Option<i64>)> {
        sqlx::query_as("SELECT id, kind, item_id FROM jobs ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .expect("job rows")
    }

    fn worker(&self, ai: Option<Arc<dyn AiService>>) -> JobWorker {
        let ai_service: SharedAiService = Arc::new(tokio::sync::RwLock::new(ai));
        JobWorker::new(
            self.jobs.clone(),
            self.repo.clone(),
            ai_service,
            AiActivityStore::new(),
            Some(self.fetcher.clone()),
            ChromaHolder::with_enabled(false),
            Arc::new(TagMatcher::local()),
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn refresh_commits_articles_then_enriches_them_in_the_background() {
    let url = serve_feed(rss(&[("a", "Alpha"), ("b", "Beta"), ("c", "Gamma")])).await;
    let env = env(&url, true, false).await;

    // 1. Refresh: every article is committed, nothing is enriched yet.
    let saved = env
        .service
        .fetch_and_save_feed(&env.subscription)
        .await
        .expect("fetch and save");
    assert_eq!(saved.len(), 3, "all three entries are stored");
    assert!(
        saved.iter().all(|item| item.tags.is_none()),
        "the refresh must not wait for classification"
    );

    // 2. Each article owns its enrichment jobs — no placeholder id anywhere.
    // Semantic search is not configured in this harness, so indexing jobs are
    // deliberately not queued (the watermark sync indexes the backlog if the
    // feature is switched on later).
    let rows = env.job_rows().await;
    assert_eq!(rows.len(), 3, "3 articles × classify");
    for (job_id, kind, item_id) in &rows {
        let item_id = item_id.unwrap_or_else(|| panic!("job {job_id} ({kind}) has no article"));
        assert!(
            saved.iter().any(|item| item.id == item_id),
            "job {job_id} ({kind}) points at {item_id}, which is not one of the committed articles"
        );
    }

    // 3. The worker classifies the whole claim with one model call.
    let ai = FakeAi::ok();
    let worker = env.worker(Some(ai.clone()));
    assert!(worker.tick().await.expect("tick"));

    // Only classification is claimable here: the model is called once for the
    // whole claim.
    assert_eq!(ai.calls(), 1, "one batch, one model call");
    for item in &saved {
        let stored = env.repo.find_by_id(item.id).await.expect("reload");
        assert_eq!(
            stored.tags.as_deref(),
            Some(r#"["machine_learning"]"#),
            "article {} was enriched",
            item.id
        );
    }

    let stats = env.jobs.stats().await.expect("stats");
    assert_eq!(stats.failed, 0, "queue after draining: {stats:?}");
    assert_eq!(stats.queued, 0, "queue after draining: {stats:?}");
}

#[tokio::test]
async fn refetching_the_same_feed_creates_no_duplicates_and_no_new_jobs() {
    let url = serve_feed(rss(&[("a", "Alpha"), ("b", "Beta")])).await;
    let env = env(&url, true, false).await;

    let first = env
        .service
        .fetch_and_save_feed(&env.subscription)
        .await
        .expect("first fetch");
    assert_eq!(first.len(), 2);
    let queued_after_first = env.jobs.stats().await.expect("stats").queued;

    let second = env
        .service
        .fetch_and_save_feed(&env.subscription)
        .await
        .expect("second fetch");
    assert!(second.is_empty(), "guid/link dedup must skip unchanged entries");
    assert_eq!(
        env.jobs.stats().await.expect("stats").queued,
        queued_after_first,
        "a no-op refresh must not queue duplicate enrichment"
    );
    assert_eq!(
        env.repo.find_all(None, 10, 0).await.expect("list").len(),
        2,
        "the library still holds exactly two articles"
    );
}

/// The website handler refuses non-public article URLs. That guard is what
/// keeps feed links from reaching private services — and the refusal has to
/// leave a *retryable* job with a readable reason, not a silent success.
#[tokio::test]
async fn website_jobs_fail_cleanly_for_private_article_urls() {
    let url = serve_feed(rss(&[("a", "Alpha")])).await;
    let env = env(&url, false, true).await;

    let item = env
        .repo
        .create_with_jobs(
            NewFeedItem {
                subscription_id: env.subscription.id,
                guid: Some("loopback".into()),
                title: "Local service".into(),
                link: Some("http://127.0.0.1:9/private".into()),
                ..Default::default()
            },
            vec![NewJob::website_markdown(
                None,
                "http://127.0.0.1:9/private".into(),
            )],
        )
        .await
        .expect("seed article");

    let worker = env.worker(None);
    assert!(worker.tick().await.expect("tick"));

    let stats = env.jobs.stats().await.expect("stats");
    assert_eq!(stats.queued, 1, "the job stays retryable: {stats:?}");
    assert_eq!(stats.failed, 0);
    assert!(
        stats
            .recent_errors
            .iter()
            .any(|error| error.contains("public http(s) URL")),
        "the guard's reason must be recorded: {:?}",
        stats.recent_errors
    );

    // Nothing half-written: no website flag, no cached body.
    let stored = env.repo.find_by_id(item.id).await.expect("reload");
    assert!(!stored.is_website_content);
    assert!(stored.content_md.is_none());
    assert_eq!(
        env.job_rows()
            .await
            .iter()
            .filter(|(_, kind, _)| kind == JobKind::WebsiteMarkdown.as_str())
            .count(),
        1
    );
}

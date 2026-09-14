// Prevents additional console window on Windows in release builds, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// Core modules
mod ai;
mod chroma;
mod content_processor;
mod database;
mod error;
mod feed;
mod models;
mod repositories;
mod services;

// Tauri command modules
mod commands;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use tauri::Manager;

use ai::activity::AiActivityStore;
use ai::service::SharedAiService;
use database::init_database;
use feed::FeedFetcher;
use repositories::feed_item_repo::SqliteFeedItemRepository;
use repositories::job_repo::SqliteJobRepository;
use repositories::subscription_repo::SqliteSubscriptionRepository;
use services::{FeedService, JobWorker, SubscriptionService};

// Import all Tauri command functions
use commands::*;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // Initialize database
            let pool = tauri::async_runtime::block_on(async {
                init_database()
                    .await
                    .expect("Failed to initialize database")
            });

            // Wire up services with repository pattern
            let ai_activity = AiActivityStore::new();
            ai_activity.attach_app(app.handle().clone());
            let feed_repo = Arc::new(SqliteFeedItemRepository::new(pool.clone()));
            let job_repo: Arc<dyn repositories::job_repo::JobRepository> =
                Arc::new(SqliteJobRepository::new(pool.clone()));
            let sub_repo = Arc::new(SqliteSubscriptionRepository::new(pool.clone()));
            let subscription_service = SubscriptionService::new(sub_repo.clone());
            let fetcher = FeedFetcher::new().map_err(|e| {
                eprintln!("Failed to build HTTP client: {}", e);
                Box::new(e) as Box<dyn std::error::Error>
            })?;
            let fetcher = Arc::new(fetcher);
            // ChromaDB connects lazily on first use (health check, search,
            // sync) and auto-reconnects — the app must not stay broken just
            // because the server was down when it launched.
            let chroma_service = crate::chroma::ChromaHolder::default();
            let ai_service: SharedAiService = Arc::new(tokio::sync::RwLock::new(
                commands::ai_commands::load_configured_ai_service(),
            ));
            // Local ONNX embedder for snapping generated tags onto the
            // catalog; loads the model lazily on first classification.
            let tag_matcher = Arc::new(services::TagMatcher::local());
            let feed_service = FeedService::new(feed_repo.clone())
                .with_subscription_repo(sub_repo.clone())
                .with_fetcher(fetcher.clone())
                .with_ai_service(ai_service.clone())
                .with_chroma_service(chroma_service.clone())
                .with_ai_activity(ai_activity.clone())
                .with_tag_matcher(tag_matcher.clone());

            let sync_chroma = chroma_service.clone();
            let sync_repo = feed_repo.clone();

            let app_state = commands::AppState {
                subscription_service,
                feed_service,
                feed_repo: feed_repo.clone(),
                jobs: job_repo.clone(),
                fetcher: fetcher.clone(),
                ai_service: ai_service.clone(),
                ai_activity: ai_activity.clone(),
                chroma_service: chroma_service.clone(),
                tag_matcher: tag_matcher.clone(),
            };

            app.manage(app_state);

            // Durable enrichment worker: classification, website caching, and
            // semantic indexing run here, never inside a feed refresh.
            Arc::new(JobWorker::new(
                job_repo,
                feed_repo.clone(),
                ai_service,
                ai_activity,
                Some(fetcher.clone()),
                chroma_service.clone(),
                tag_matcher,
            ))
            .spawn();

            // Incremental ChromaDB sync at startup: indexes everything since
            // the persisted watermark (e.g. items fetched while ChromaDB was
            // disabled or unreachable) and drains the pending queues. The
            // holder is a no-op when ChromaDB is disabled or unreachable.
            tauri::async_runtime::spawn(async move {
                crate::chroma::sync::run_background_sync(sync_repo, sync_chroma).await;
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Subscription commands
            add_subscription,
            remove_subscription,
            list_subscriptions,
            get_subscription,
            update_subscription,
            toggle_use_website,
            toggle_auto_classify,
            // Feed commands
            fetch_feed,
            fetch_all_feeds,
            refresh_subscriptions,
            fetch_website_markdown,
            import_opml,
            export_opml,
            // Item commands
            get_items,
            search_items,
            get_item,
            reset_item_content_md,
            get_items_by_subscription,
            get_items_by_tag,
            get_all_tags,
            // Background jobs
            get_job_stats,
            retry_failed_jobs,
            requeue_expired_jobs,
            mark_item_read,
            mark_all_read,
            toggle_favorite,
            toggle_read_later,
            toggle_ignored,
            get_favorites,
            get_read_later,
            get_unread,
            get_today_items,
            save_item_tags,
            // Tag management
            get_tag_catalog,
            get_blocked_tags,
            create_tag,
            rename_tag,
            merge_tags,
            consolidate_single_use_tags,
            delete_tag,
            restore_tag,
            tag_dictionary_status,
            generate_tag_explanations,
            index_tag_dictionary,
            get_tag_overview,
            get_topic_workspace,
            apply_topic_changes,
            suggest_topic_assignments,
            get_tag_match_config,
            set_tag_match_config,
            // Webview / browser
            open_url_in_browser,
            // AI commands
            translate_content_bilingual,
            translate_item_bilingual,
            translate_item_bilingual_streaming,
            translate_html_content_streaming,
            classify_item,
            recommend_reads,
            set_ai_config,
            get_ai_config,
            get_ai_activity,
            // ChromaDB commands
            set_chroma_config,
            enable_chroma_and_index,
            get_chroma_config,
            semantic_search,
            find_similar_items,
            reindex_chromadb,
            chroma_sync,
            chroma_index_status,
            chroma_sync_progress,
            chroma_backfill_markdown,
            chroma_health_check,
            // Debug commands (dev builds only)
            #[cfg(debug_assertions)]
            test_html2md,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// Prevents additional console window on Windows in release builds, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// Core modules
mod ai;
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

use database::init_database;
use feed::FeedFetcher;
use repositories::feed_item_repo::SqliteFeedItemRepository;
use repositories::subscription_repo::SqliteSubscriptionRepository;
use services::{FeedService, SubscriptionService};

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
            let feed_repo = Arc::new(SqliteFeedItemRepository::new(pool.clone()));
            let sub_repo = Arc::new(SqliteSubscriptionRepository::new(pool.clone()));
            let subscription_service = SubscriptionService::new(sub_repo.clone());
            let fetcher = Arc::new(FeedFetcher::new());
            let feed_service = FeedService::new(feed_repo.clone())
                .with_subscription_repo(sub_repo.clone())
                .with_fetcher(fetcher.clone());

            let app_state = commands::AppState {
                subscription_service,
                feed_service,
                feed_repo,
                fetcher,
                ai_service: None,
                pool: pool.clone(),
            };

            app.manage(pool);
            app.manage(app_state);

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
            fetch_website_content,
            import_opml,
            export_opml,
            // Item commands
            get_items,
            search_items,
            get_item,
            get_items_by_subscription,
            get_items_by_tag,
            get_all_tags,
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
            // Webview / browser
            open_url_in_browser,
            // AI commands
            translate_content_bilingual,
            translate_item_bilingual,
            translate_item_bilingual_streaming,
            translate_html_content_streaming,
            classify_item,
            set_ai_config,
            get_ai_config,
            // Debug commands
            test_html2md,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

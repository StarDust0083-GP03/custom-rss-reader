// Prevents additional console window on Windows in release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod database;
mod debug;
mod feed;
mod opml;
mod ai;

use database::init_database;
use tauri::Manager;

// Tauri commands
use commands::{
    feeds::{fetch_all_feeds, fetch_feed, refresh_subscriptions, fetch_website_content, translate_website_content},
    items::{get_item, get_items, get_items_by_subscription, search_items, get_items_by_tag, get_all_tags},
    item_actions::{mark_item_read, mark_all_read, toggle_favorite, toggle_read_later, get_favorites, get_read_later, get_unread, get_today_items, save_item_tags},
    opml::import_opml,
    subs::{
        add_subscription, get_subscription, list_subscriptions, remove_subscription,
        toggle_use_website, toggle_auto_classify, update_subscription,
    },
    ai::{translate_content_bilingual, translate_item_bilingual, translate_item_bilingual_streaming, classify_item, set_ai_config},
    webview::open_url_in_webview,
};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // Initialize database
            let pool = tauri::async_runtime::block_on(async {
                init_database(app.handle())
                    .await
                    .expect("Failed to initialize database")
            });

            // Store pool in app state (need Manager trait for manage method)
            app.manage(pool);

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
            translate_website_content,
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
            get_favorites,
            get_read_later,
            get_unread,
            get_today_items,
            save_item_tags,
            open_url_in_webview,
            // OPML commands
            import_opml,
            commands::opml::export_opml,
            // AI commands
            translate_content_bilingual,
            translate_item_bilingual,
            translate_item_bilingual_streaming,
            classify_item,
            set_ai_config,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

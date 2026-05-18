use tauri::State;

use crate::error::Result;
use crate::models::{FeedItem, Subscription};

use super::AppState;

// ---- Feed fetching ----

#[tauri::command]
pub async fn fetch_feed(
    state: State<'_, AppState>,
    subscription_id: i64,
) -> Result<Vec<FeedItem>> {
    let sub = state
        .subscription_service
        .get_subscription(subscription_id)
        .await?;
    state.feed_service.fetch_and_save_feed(&sub).await
}

#[tauri::command]
pub async fn fetch_all_feeds(state: State<'_, AppState>) -> Result<crate::services::FetchSummary> {
    let summary = state.feed_service.fetch_and_save_all_feeds().await;
    Ok(summary)
}

#[tauri::command]
pub async fn refresh_subscriptions(
    state: State<'_, AppState>,
    subscription_ids: Vec<i64>,
) -> Result<Vec<(i64, std::result::Result<Vec<FeedItem>, String>)>> {
    state
        .feed_service
        .refresh_subscriptions(&subscription_ids)
        .await
}

#[tauri::command]
pub async fn fetch_website_content(
    state: State<'_, AppState>,
    url: String,
    item_id: Option<i64>,
) -> Result<String> {
    let html = state.fetcher.fetch_website_content(&url).await?;

    if let Some(item_id) = item_id {
        let md = crate::content_processor::html_to_markdown_pipeline(&html)?;
        state.feed_repo.update_content_md(item_id, &md).await?;
        // Mark item as website content so translation/display uses content_md
        sqlx::query("UPDATE feed_items SET is_website_content = 1 WHERE id = $1")
            .bind(item_id)
            .execute(&state.pool)
            .await
            .ok();
    }

    Ok(html)
}

// ---- OPML ----

#[tauri::command]
pub async fn import_opml(
    state: State<'_, AppState>,
    file_path: String,
) -> Result<Vec<Subscription>> {
    let content = std::fs::read_to_string(&file_path)
        .map_err(|e| crate::error::AppError::Internal(format!("Failed to read OPML file: {}", e)))?;

    let subscriptions = parse_opml_simple(&content)?;

    let mut created = Vec::new();
    for sub in subscriptions {
        match state.subscription_service.add_subscription(sub).await {
            Ok(s) => created.push(s),
            Err(_) => continue,
        }
    }

    Ok(created)
}

#[tauri::command]
pub async fn export_opml(
    state: State<'_, AppState>,
    file_path: String,
) -> Result<()> {
    let subscriptions = state.subscription_service.list_subscriptions().await?;
    let opml = generate_opml(&subscriptions)?;

    std::fs::write(&file_path, opml)
        .map_err(|e| crate::error::AppError::Internal(format!("Failed to write OPML file: {}", e)))?;

    Ok(())
}

// ---- OPML parsing/generation ----

/// Simple OPML parser using quick-xml's low-level reader.
fn parse_opml_simple(content: &str) -> Result<Vec<crate::models::NewSubscription>> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(content);
    let mut buf = Vec::new();
    let mut subscriptions = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref e)) | Ok(Event::Start(ref e)) => {
                if e.name().as_ref() == b"outline" {
                    let mut url = String::new();
                    let mut title = String::new();

                    for attr in e.attributes().flatten() {
                        match std::str::from_utf8(attr.key.as_ref()).unwrap_or("") {
                            "xmlUrl" => {
                                url = attr.unescape_value().unwrap_or_default().to_string()
                            }
                            "title" | "text" => {
                                title = attr.unescape_value().unwrap_or_default().to_string()
                            }
                            _ => {}
                        }
                    }

                    if !url.is_empty() {
                        subscriptions.push(crate::models::NewSubscription {
                            url,
                            title: Some(title).filter(|t| !t.is_empty()),
                            ..Default::default()
                        });
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(crate::error::AppError::Parse(format!(
                    "OPML parse error: {}",
                    e
                )));
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(subscriptions)
}

/// Generate an OPML XML string from subscriptions.
fn generate_opml(subscriptions: &[Subscription]) -> Result<String> {
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<opml version="2.0">
  <head>
    <title>RSS Reader Subscriptions</title>
  </head>
  <body>
"#,
    );

    for sub in subscriptions {
        let title = sub.title.as_deref().unwrap_or("");
        let escaped_title = escape_xml(title);
        let escaped_url = escape_xml(&sub.url);
        xml.push_str(&format!(
            r#"    <outline text="{}" title="{}" type="rss" xmlUrl="{}"/>"#,
            escaped_title, escaped_title, escaped_url
        ));
        xml.push('\n');
    }

    xml.push_str("  </body>\n</opml>\n");
    Ok(xml)
}

/// Escape XML special characters.
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

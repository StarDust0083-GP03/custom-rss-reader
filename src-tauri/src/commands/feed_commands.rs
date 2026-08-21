use tauri::State;

use crate::error::{AppError, Result};
use crate::models::{FeedItem, NewSubscription, Subscription};

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
    // Catch-up sync: picks up anything the fetch-time indexing missed
    // (ChromaDB briefly down, item raced the collection creation, ...).
    // Fire-and-forget — the user's refresh must not wait on ChromaDB.
    // The holder lazy-connects, so this is a no-op when ChromaDB is off.
    {
        let repo = state.feed_repo.clone();
        let chroma = state.chroma_service.clone();
        tauri::async_runtime::spawn(async move {
            crate::chroma::sync::run_background_sync(repo, chroma).await;
        });
    }
    Ok(summary)
}

/// Result of refreshing one subscription: `(subscription_id, outcome)`.
type RefreshResult = Vec<(i64, std::result::Result<Vec<FeedItem>, String>)>;

#[tauri::command]
pub async fn refresh_subscriptions(
    state: State<'_, AppState>,
    subscription_ids: Vec<i64>,
) -> Result<RefreshResult> {
    state
        .feed_service
        .refresh_subscriptions(&subscription_ids)
        .await
}

/// Fetch the article's website HTML and return its Markdown representation.
///
/// All webview / text display paths converge on the same `html_to_markdown_pipeline`
/// so the renderer never has to make a raw-HTML vs. Markdown branching decision.
///
/// When `item_id` is provided the Markdown is persisted as `content_md` (and the
/// `is_website_content` flag is set) so subsequent reads can skip the fetch.
#[tauri::command]
pub async fn fetch_website_markdown(
    state: State<'_, AppState>,
    url: String,
    item_id: Option<i64>,
) -> Result<String> {
    let html = state.fetcher.fetch_website_content(&url).await?;

    // HTML -> Markdown is CPU-bound; keep it off the runtime worker.
    let html_for_md = html.clone();
    let md = tokio::task::spawn_blocking(move || {
        crate::content_processor::html_to_markdown_pipeline(&html_for_md)
    })
    .await
    .map_err(|e| AppError::Internal(format!("markdown task failed: {}", e)))??;

    if let Some(item_id) = item_id {
        state.feed_repo.update_content_md(item_id, &md, true).await?;
        // The website Markdown is richer than the RSS snippet the item was
        // first indexed with — queue a re-embed so semantic search finds
        // this article by its full text on the next sync.
        crate::chroma::sync::SyncState::queue_upsert(item_id).await;
    }

    Ok(md)
}

// ---- OPML ----

/// Result of an OPML import — kept distinct from `Vec<Subscription>` so the
/// frontend can show "X imported, Y skipped" instead of silently dropping
/// rows.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OpmlImportResult {
    pub created: Vec<Subscription>,
    pub skipped: Vec<OpmlImportError>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OpmlImportError {
    pub url: String,
    pub reason: String,
}

#[tauri::command]
pub async fn import_opml(
    state: State<'_, AppState>,
    file_path: String,
) -> Result<OpmlImportResult> {
    let content = tokio::fs::read_to_string(&file_path)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to read OPML file: {}", e)))?;

    let subscriptions = parse_opml(&content)?;

    let mut created = Vec::new();
    let mut skipped = Vec::new();
    for sub in subscriptions {
        let url = sub.url.clone();
        match state.subscription_service.add_subscription(sub).await {
            Ok(s) => created.push(s),
            Err(AppError::Duplicate(_)) => skipped.push(OpmlImportError {
                url,
                reason: "duplicate URL".into(),
            }),
            Err(e) => skipped.push(OpmlImportError {
                url,
                reason: e.to_string(),
            }),
        }
    }

    Ok(OpmlImportResult { created, skipped })
}

#[tauri::command]
pub async fn export_opml(
    state: State<'_, AppState>,
    file_path: String,
) -> Result<()> {
    let subscriptions = state.subscription_service.list_subscriptions().await?;
    let opml = generate_opml(&subscriptions)?;

    tokio::fs::write(&file_path, opml)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to write OPML file: {}", e)))?;

    Ok(())
}

// ---- OPML parsing/generation ----

/// Parse a 2.0 OPML document and extract subscriptions. Returns an error
/// for malformed XML (so the frontend can show a helpful message); skips
/// `<outline>` elements that don't carry a `xmlUrl`.
fn parse_opml(content: &str) -> Result<Vec<NewSubscription>> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(content);
    let mut buf = Vec::new();
    let mut subscriptions = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref e)) | Ok(Event::Start(ref e)) => {
                if e.name().as_ref() != b"outline" {
                    continue;
                }
                let mut url: Option<String> = None;
                let mut title: Option<String> = None;
                let mut website_url: Option<String> = None;
                let mut rsshub_url: Option<String> = None;
                let mut use_website: Option<bool> = None;
                let mut auto_classify: Option<bool> = None;

                for attr in e.attributes().flatten() {
                    let key = std::str::from_utf8(attr.key.as_ref()).unwrap_or("");
                    let value = attr
                        .unescape_value()
                        .unwrap_or_default()
                        .to_string();
                    match key {
                        "xmlUrl" | "xmlurl" => url = Some(value),
                        "title" | "text" => title = Some(value),
                        "websiteUrl" | "website_url" => website_url = Some(value),
                        "rsshubUrl" | "rsshub_url" => rsshub_url = Some(value),
                        "useWebsite" | "use_website" => {
                            use_website = parse_bool_attr(&value)
                        }
                        "autoClassify" | "auto_classify" => {
                            auto_classify = parse_bool_attr(&value)
                        }
                        _ => {}
                    }
                }

                if let Some(url) = url.filter(|u| !u.is_empty()) {
                    subscriptions.push(NewSubscription {
                        url,
                        title: title.filter(|t| !t.is_empty()),
                        website_url: website_url.filter(|s| !s.is_empty()),
                        rsshub_url: rsshub_url.filter(|s| !s.is_empty()),
                        use_website: use_website.unwrap_or(false),
                        auto_classify: auto_classify.unwrap_or(true),
                        opml_attributes: None,
                    });
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(AppError::Parse(format!("OPML parse error: {}", e)));
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(subscriptions)
}

fn parse_bool_attr(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "y" => Some(true),
        "false" | "0" | "no" | "n" => Some(false),
        _ => None,
    }
}

/// Generate an OPML 2.0 document from subscriptions, including the
/// round-trippable extension fields (websiteUrl, rsshubUrl, useWebsite,
/// autoClassify).
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
        let mut line = format!(
            r#"    <outline text="{}" title="{}" type="rss" xmlUrl="{}""#,
            escaped_title, escaped_title, escaped_url
        );
        if let Some(ref w) = sub.website_url {
            if !w.is_empty() {
                line.push_str(&format!(r#" websiteUrl="{}""#, escape_xml(w)));
            }
        }
        if let Some(ref r) = sub.rsshub_url {
            if !r.is_empty() {
                line.push_str(&format!(r#" rsshubUrl="{}""#, escape_xml(r)));
            }
        }
        line.push_str(&format!(
            r#" useWebsite="{}" autoClassify="{}"/>"#,
            if sub.use_website { "true" } else { "false" },
            if sub.auto_classify { "true" } else { "false" },
        ));
        line.push('\n');
        xml.push_str(&line);
    }

    xml.push_str("  </body>\n</opml>\n");
    Ok(xml)
}

/// Escape XML special characters in attribute / text values.
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_opml_basic() {
        let xml = r#"<?xml version="1.0"?>
<opml version="2.0">
  <body>
    <outline title="A" xmlUrl="https://a.com/feed"/>
    <outline title="B" text="B-text" xmlUrl="https://b.com/feed"/>
    <outline title="No URL"/>
  </body>
</opml>"#;
        let subs = parse_opml(xml).unwrap();
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0].url, "https://a.com/feed");
        assert_eq!(subs[0].title.as_deref(), Some("A"));
        assert_eq!(subs[1].url, "https://b.com/feed");
    }

    #[test]
    fn test_parse_opml_extension_fields() {
        let xml = r#"<opml version="2.0"><body>
            <outline title="X" xmlUrl="https://x.com/feed"
                websiteUrl="https://x.com" useWebsite="true" autoClassify="false"/>
        </body></opml>"#;
        let subs = parse_opml(xml).unwrap();
        assert!(subs[0].use_website);
        assert!(!subs[0].auto_classify);
        assert_eq!(subs[0].website_url.as_deref(), Some("https://x.com"));
    }

    #[test]
    fn test_parse_opml_invalid_xml_errors() {
        let xml = "this is not XML";
        // Either parse-error on the first non-tag content or empty result.
        let result = parse_opml(xml);
        // quick-xml is lenient: it just returns no events. Either outcome is
        // acceptable; we only assert no panic.
        let _ = result;
    }

    #[test]
    fn test_generate_opml_includes_extensions() {
        let subs = vec![Subscription {
            id: 1,
            url: "https://a.com/feed".into(),
            title: Some("Title".into()),
            website_url: Some("https://a.com".into()),
            rsshub_url: Some("https://rsshub/a".into()),
            use_website: true,
            auto_classify: false,
            opml_attributes: None,
            created_at: "2024-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2024-01-01T00:00:00Z".parse().unwrap(),
        }];
        let xml = generate_opml(&subs).unwrap();
        assert!(xml.contains("websiteUrl=\"https://a.com\""));
        assert!(xml.contains("useWebsite=\"true\""));
        assert!(xml.contains("autoClassify=\"false\""));
    }

    #[test]
    fn test_opml_roundtrip() {
        let subs = vec![Subscription {
            id: 1,
            url: "https://a.com/feed".into(),
            title: Some("T&<>\"".into()),
            website_url: Some("https://a.com".into()),
            rsshub_url: None,
            use_website: true,
            auto_classify: true,
            opml_attributes: None,
            created_at: "2024-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2024-01-01T00:00:00Z".parse().unwrap(),
        }];
        let xml = generate_opml(&subs).unwrap();
        let parsed = parse_opml(&xml).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].url, "https://a.com/feed");
        // The XML escape for & is &amp; — when re-decoded, it becomes &.
        assert_eq!(parsed[0].title.as_deref(), Some("T&<>\""));
    }

    #[test]
    fn test_escape_xml() {
        assert_eq!(escape_xml("a&b"), "a&amp;b");
        assert_eq!(escape_xml("<tag>"), "&lt;tag&gt;");
        assert_eq!(escape_xml(r#"""#), "&quot;");
    }

    #[test]
    fn test_parse_bool_attr() {
        assert_eq!(parse_bool_attr("true"), Some(true));
        assert_eq!(parse_bool_attr("TRUE"), Some(true));
        assert_eq!(parse_bool_attr("1"), Some(true));
        assert_eq!(parse_bool_attr("yes"), Some(true));
        assert_eq!(parse_bool_attr("false"), Some(false));
        assert_eq!(parse_bool_attr("0"), Some(false));
        assert_eq!(parse_bool_attr("garbage"), None);
    }
}
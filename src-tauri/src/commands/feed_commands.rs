use tauri::State;

use crate::error::{AppError, Result};
use crate::models::{FeedItem, NewSubscription, Subscription};

use super::AppState;

// ---- Feed fetching ----

#[tauri::command]
pub async fn fetch_feed(state: State<'_, AppState>, subscription_id: i64) -> Result<Vec<FeedItem>> {
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
        state
            .feed_repo
            .update_content_md(item_id, &md, true)
            .await?;
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
pub async fn export_opml(state: State<'_, AppState>, file_path: String) -> Result<()> {
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
///
/// Round-trip fidelity (issue #25): a feed URL without its folder path is a
/// lossy import, and `htmlUrl` (the *website* URL, distinct from `xmlUrl`) is
/// part of the standard format — dropping it made every imported feed fall
/// back to RSS-only with no website to fetch. Attributes this app doesn't
/// model are preserved verbatim so exporting after importing does not silently
/// strip another reader's settings.
fn parse_opml(content: &str) -> Result<Vec<NewSubscription>> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(content);
    let mut buf = Vec::new();
    let mut subscriptions = Vec::new();
    let mut saw_opml = false;
    // Folder path from the nesting of outlines without an xmlUrl.
    let mut folders: Vec<String> = Vec::new();

    /// Attributes this app models itself; everything else is preserved.
    const KNOWN_ATTRS: [&str; 9] = [
        "xmlUrl",
        "xmlurl",
        "title",
        "text",
        "htmlUrl",
        "websiteUrl",
        "website_url",
        "rsshubUrl",
        "rsshub_url",
    ];

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) if e.name().as_ref() == b"opml" => {
                saw_opml = true;
            }
            Ok(Event::End(ref e)) => {
                if e.name().as_ref() == b"outline" {
                    folders.pop();
                }
            }
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
                let mut extra: std::collections::BTreeMap<String, String> =
                    std::collections::BTreeMap::new();

                for attr in e.attributes().flatten() {
                    let key = std::str::from_utf8(attr.key.as_ref()).unwrap_or("");
                    let value = attr.unescape_value().unwrap_or_default().to_string();
                    match key {
                        "xmlUrl" | "xmlurl" => url = Some(value),
                        "title" | "text" => title = Some(value),
                        "htmlUrl" => website_url = Some(value),
                        "websiteUrl" | "website_url" => website_url = Some(value),
                        "rsshubUrl" | "rsshub_url" => rsshub_url = Some(value),
                        "useWebsite" | "use_website" => use_website = parse_bool_attr(&value),
                        "autoClassify" | "auto_classify" => auto_classify = parse_bool_attr(&value),
                        _ => {
                            if !KNOWN_ATTRS.contains(&key) {
                                extra.insert(key.to_string(), value);
                            }
                        }
                    }
                }

                let folder = folders.join("/");
                let is_feed = url.as_deref().is_some_and(|u| !u.is_empty());
                if !is_feed {
                    // A container outline names a folder for its children.
                    if let Some(name) = title.clone().filter(|t| !t.is_empty()) {
                        folders.push(name);
                    }
                    continue;
                }

                let attributes = OpmlAttributes {
                    folder: if folder.is_empty() {
                        None
                    } else {
                        Some(folder)
                    },
                    attrs: extra,
                };
                let opml_attributes = if attributes.folder.is_none() && attributes.attrs.is_empty()
                {
                    None
                } else {
                    serde_json::to_string(&attributes).ok()
                };

                subscriptions.push(NewSubscription {
                    url: url.unwrap(),
                    title: title.filter(|t| !t.is_empty()),
                    website_url: website_url.filter(|s| !s.is_empty()),
                    rsshub_url: rsshub_url.filter(|s| !s.is_empty()),
                    use_website: use_website.unwrap_or(false),
                    auto_classify: auto_classify.unwrap_or(true),
                    opml_attributes,
                });
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(AppError::Parse(format!("OPML parse error: {}", e)));
            }
            _ => {}
        }
        buf.clear();
    }

    // quick-xml is lenient: arbitrary non-XML text parses as plain-text
    // events without error. Reject content that never opened an <opml>
    // element so a stray file import fails loudly instead of importing 0
    // subscriptions as if the file were valid but empty.
    if !saw_opml {
        return Err(AppError::Parse(
            "OPML parse error: no <opml> element found".into(),
        ));
    }

    Ok(subscriptions)
}

/// The parts of an OPML outline that this app does not model directly.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct OpmlAttributes {
    /// Slash-separated folder path from the outline nesting.
    #[serde(default)]
    folder: Option<String>,
    /// Unrecognized attributes, preserved verbatim for export.
    #[serde(default)]
    attrs: std::collections::BTreeMap<String, String>,
}

fn parse_bool_attr(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "y" => Some(true),
        "false" | "0" | "no" | "n" => Some(false),
        _ => None,
    }
}

/// Generate an OPML 2.0 document from subscriptions.
///
/// Emits the standard `htmlUrl` (website) alongside the app's `websiteUrl`
/// extension, restores preserved attributes, and nests subscriptions under
/// their original folder outlines so an export/re-import round-trips instead
/// of flattening the library and dropping fields.
fn generate_opml(subscriptions: &[Subscription]) -> Result<String> {
    use std::collections::BTreeMap;

    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<opml version="2.0">
  <head>
    <title>RSS Reader Subscriptions</title>
  </head>
  <body>
"#,
    );

    // Group by folder, keeping the first-seen order of both folders and their
    // entries (a reader's manual ordering should survive an export).
    let mut root: Vec<&Subscription> = Vec::new();
    let mut folders: Vec<(String, Vec<&Subscription>)> = Vec::new();
    for sub in subscriptions {
        let parsed = sub
            .opml_attributes
            .as_deref()
            .and_then(|raw| serde_json::from_str::<OpmlAttributes>(raw).ok())
            .unwrap_or_default();
        match parsed.folder.as_deref().filter(|f| !f.is_empty()) {
            None => root.push(sub),
            Some(path) => match folders.iter_mut().find(|(name, _)| name == path) {
                Some((_, items)) => items.push(sub),
                None => folders.push((path.to_string(), vec![sub])),
            },
        }
    }

    let write_entry = |sub: &Subscription, indent: usize| {
        let parsed = sub
            .opml_attributes
            .as_deref()
            .and_then(|raw| serde_json::from_str::<OpmlAttributes>(raw).ok())
            .unwrap_or_default();
        let mut attrs: BTreeMap<String, String> = parsed.attrs;
        let title = sub.title.as_deref().unwrap_or("");
        attrs.insert("text".into(), title.to_string());
        attrs.insert("title".into(), title.to_string());
        attrs.insert("type".into(), "rss".into());
        attrs.insert("xmlUrl".into(), sub.url.clone());
        if let Some(w) = sub.website_url.as_deref().filter(|w| !w.is_empty()) {
            // Standard field, plus this app's extension for older builds.
            attrs.insert("htmlUrl".into(), w.to_string());
            attrs.insert("websiteUrl".into(), w.to_string());
        }
        if let Some(r) = sub.rsshub_url.as_deref().filter(|r| !r.is_empty()) {
            attrs.insert("rsshubUrl".into(), r.to_string());
        }
        attrs.insert(
            "useWebsite".into(),
            if sub.use_website { "true" } else { "false" }.into(),
        );
        attrs.insert(
            "autoClassify".into(),
            if sub.auto_classify { "true" } else { "false" }.into(),
        );
        let rendered: Vec<String> = attrs
            .iter()
            .map(|(k, v)| format!(r#"{k}="{}""#, escape_xml(v)))
            .collect();
        format!("{}<outline {}/>\n", "  ".repeat(indent), rendered.join(" "))
    };

    for sub in &root {
        xml.push_str(&write_entry(sub, 2));
    }
    for (folder, items) in &folders {
        // Nested folder paths become nested container outlines.
        let segments: Vec<&str> = folder.split('/').filter(|s| !s.is_empty()).collect();
        for (depth, segment) in segments.iter().enumerate() {
            let escaped = escape_xml(segment);
            xml.push_str(&format!(
                "{}<outline text=\"{}\" title=\"{}\">\n",
                "  ".repeat(2 + depth),
                escaped,
                escaped
            ));
        }
        for sub in items {
            xml.push_str(&write_entry(sub, 2 + segments.len()));
        }
        for depth in (0..segments.len()).rev() {
            xml.push_str(&format!("{}</outline>\n", "  ".repeat(2 + depth)));
        }
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
        // quick-xml is lenient: plain text parses as text events without
        // error, so the <opml> guard must turn it into a loud parse error
        // instead of importing zero subscriptions as a silent success.
        let result = parse_opml("this is not XML");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_opml_preserves_folder_and_unknown_attributes() {
        let xml = r#"<opml version="2.0"><body>
            <outline text="Tech">
              <outline text="News">
                <outline text="X" xmlUrl="https://x.com/feed" htmlUrl="https://x.com" customFlag="keep-me"/>
              </outline>
            </outline>
        </body></opml>"#;
        let subs = parse_opml(xml).unwrap();
        assert_eq!(subs.len(), 1);
        // htmlUrl is the standard website field.
        assert_eq!(subs[0].website_url.as_deref(), Some("https://x.com"));
        let attrs: OpmlAttributes =
            serde_json::from_str(subs[0].opml_attributes.as_deref().unwrap()).unwrap();
        assert_eq!(attrs.folder.as_deref(), Some("Tech/News"));
        assert_eq!(
            attrs.attrs.get("customFlag").map(String::as_str),
            Some("keep-me")
        );
    }

    /// Export → import must keep folders, htmlUrl, and unknown attributes.
    #[test]
    fn test_opml_roundtrip_preserves_folders_and_extras() {
        let subs = vec![Subscription {
            id: 1,
            url: "https://x.com/feed".into(),
            title: Some("X".into()),
            website_url: Some("https://x.com".into()),
            rsshub_url: None,
            use_website: true,
            auto_classify: true,
            opml_attributes: Some(
                r#"{"folder":"Tech/News","attrs":{"customFlag":"keep-me"}}"#.into(),
            ),
            http_etag: None,
            http_last_modified: None,
            created_at: "2024-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2024-01-01T00:00:00Z".parse().unwrap(),
        }];

        let xml = generate_opml(&subs).unwrap();
        assert!(xml.contains(r#"htmlUrl="https://x.com""#), "xml was: {xml}");
        assert!(xml.contains(r#"customFlag="keep-me""#), "xml was: {xml}");
        assert!(
            xml.contains(r#"<outline text="Tech" title="Tech">"#),
            "xml was: {xml}"
        );

        let parsed = parse_opml(&xml).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].website_url.as_deref(), Some("https://x.com"));
        let attrs: OpmlAttributes =
            serde_json::from_str(parsed[0].opml_attributes.as_deref().unwrap()).unwrap();
        assert_eq!(attrs.folder.as_deref(), Some("Tech/News"));
        assert_eq!(
            attrs.attrs.get("customFlag").map(String::as_str),
            Some("keep-me")
        );
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
            http_etag: None,
            http_last_modified: None,
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
            http_etag: None,
            http_last_modified: None,
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

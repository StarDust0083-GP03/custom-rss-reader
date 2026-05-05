use crate::error::{AppError, Result};
use crate::models::NewFeedItem;
use feed_rs::parser;
use html2md;

/// Parse RSS/Atom feed content into `NewFeedItem` vectors.
///
/// Validates content before parsing: checks for empty content,
/// compressed data (should be decompressed at fetcher level),
/// JSON-only feeds, and HTML error pages (Cloudflare, Obsidian, etc.).
pub fn parse_feed(feed_content: &str, subscription_id: i64) -> Result<Vec<NewFeedItem>> {
    let trimmed = feed_content.trim();

    if trimmed.is_empty() {
        return Err(AppError::Parse("Empty feed content".into()));
    }

    // Safety check: compressed data should be decompressed at the fetcher level
    let bytes = feed_content.as_bytes();
    if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        return Err(AppError::Parse(
            "Feed is still gzip compressed (should be decompressed at fetcher level)".into(),
        ));
    }
    if bytes.len() >= 1 && bytes[0] == 0x78 {
        return Err(AppError::Parse(
            "Feed is still deflate compressed (should be decompressed at fetcher level)".into(),
        ));
    }

    // Check for JSON-only feeds (some endpoints return JSON instead of XML)
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return Err(AppError::Parse(
            "Feed returned JSON format instead of XML/Atom. This URL may not be a valid RSS/Atom feed."
                .into(),
        ));
    }

    // Check if content is HTML (error page)
    if feed_content.contains("<!DOCTYPE html>") || feed_content.contains("<html>") {
        let error_msg = if feed_content.contains("Obsidian") {
            "This appears to be an Obsidian Publish page, not a valid RSS feed. Please check the feed URL."
        } else if feed_content.contains("Cloudflare") {
            "Feed is protected by Cloudflare. Please try a different URL or contact the site owner."
        } else {
            "Feed returned HTML instead of RSS/Atom. The URL may not be a valid feed."
        };
        return Err(AppError::Parse(error_msg.into()));
    }

    let feed = parser::parse(feed_content.as_bytes()).map_err(|e| {
        AppError::Parse(format!(
            "{} (content type: {})",
            e,
            detect_content_type(feed_content)
        ))
    })?;

    if feed.entries.is_empty() {
        return Err(AppError::Parse("No items found in feed".into()));
    }

    let items: Vec<NewFeedItem> = feed
        .entries
        .into_iter()
        .map(|entry| {
            let content = entry
                .content
                .and_then(|c| c.body)
                .or(entry.summary.clone().map(|s| s.content));

            let guid = if entry.id.is_empty() {
                entry.links.first().map(|l| l.href.clone())
            } else {
                Some(entry.id.clone())
            };

            NewFeedItem {
                subscription_id,
                guid,
                title: entry
                    .title
                    .map(|t| t.content)
                    .unwrap_or_else(|| "Untitled".to_string()),
                link: entry.links.first().map(|l| l.href.clone()),
                content: content.clone(),
                content_md: content.clone().map(|c| html2md::parse_html(&c)),
                description: entry.summary.map(|s| s.content),
                author: entry.authors.first().map(|a| a.name.clone()),
                published_at: entry.published,
                is_website_content: false,
                is_read: false,
                is_favorite: false,
                is_read_later: false,
                is_ignored: false,
                tags: None,
                category: None,
                translated_title: None,
                translated_content: None,
                translated_at: None,
                ..Default::default()
            }
        })
        .collect();

    Ok(items)
}

/// Detect the content type of a feed string for diagnostic messages.
fn detect_content_type(content: &str) -> String {
    let content = content.trim();
    if content.starts_with("<?xml") {
        "XML".to_string()
    } else if content.contains("<rss") {
        "RSS".to_string()
    } else if content.contains("<feed") {
        "Atom".to_string()
    } else if content.starts_with('{') || content.starts_with('[') {
        "JSON".to_string()
    } else if content.contains("<!DOCTYPE") || content.contains("<html") {
        "HTML".to_string()
    } else {
        "Unknown".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty_content() {
        let result = parse_feed("", 1);
        assert!(matches!(result.unwrap_err(), AppError::Parse(_)));
    }

    #[test]
    fn test_parse_html_content() {
        let result = parse_feed("<!DOCTYPE html><html><body>Not a feed</body></html>", 1);
        assert!(matches!(result.unwrap_err(), AppError::Parse(_)));
    }

    #[test]
    fn test_parse_cloudflare_html() {
        let result = parse_feed("<html><body>Cloudflare challenge page</body></html>", 1);
        assert!(matches!(result.unwrap_err(), AppError::Parse(_)));
    }

    #[test]
    fn test_parse_obsidian_html() {
        let result = parse_feed("<html><body>Obsidian Publish content</body></html>", 1);
        assert!(matches!(result.unwrap_err(), AppError::Parse(_)));
    }

    #[test]
    fn test_parse_json_content() {
        let result = parse_feed("{\"key\": \"value\"}", 1);
        assert!(matches!(result.unwrap_err(), AppError::Parse(_)));
    }

    #[test]
    fn test_parse_atom_with_content_md() {
        let feed_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test Feed</title>
  <entry>
    <title>Article with HTML Content</title>
    <id>urn:uuid:test-content-md</id>
    <content type="html">&lt;p&gt;Visit &lt;a href="https://example.com"&gt;our site&lt;/a&gt; for details.&lt;/p&gt;&lt;p&gt;&lt;img src="https://example.com/photo.jpg" alt="Photo"/&gt;&lt;/p&gt;</content>
  </entry>
</feed>"#;
        let items = parse_feed(feed_xml, 1).unwrap();
        assert_eq!(items.len(), 1);

        let item = &items[0];
        // content_md must be populated from the HTML content
        assert!(
            item.content_md.is_some(),
            "content_md should be populated during parse"
        );
        let md = item.content_md.as_ref().unwrap();
        assert!(!md.is_empty(), "content_md should not be empty");

        // Must contain markdown link syntax from the <a> tag
        assert!(
            md.contains("[our site]"),
            "content_md should contain converted link text: got '{}'",
            md
        );
        assert!(
            md.contains("(https://example.com)"),
            "content_md should contain converted link URL: got '{}'",
            md
        );

        // Must contain markdown image syntax from the <img> tag
        assert!(
            md.contains("![Photo]"),
            "content_md should contain converted image alt: got '{}'",
            md
        );
        assert!(
            md.contains("(https://example.com/photo.jpg)"),
            "content_md should contain converted image src: got '{}'",
            md
        );
    }

    #[test]
    fn test_detect_content_type() {
        assert_eq!(detect_content_type("<?xml version=\"1.0\"?>"), "XML");
        assert_eq!(detect_content_type("<rss version=\"2.0\">"), "RSS");
        assert_eq!(detect_content_type("<feed xmlns=\"http://www.w3.org/2005/Atom\">"), "Atom");
        assert_eq!(detect_content_type("{\"key\": \"value\"}"), "JSON");
        assert_eq!(detect_content_type("<!DOCTYPE html>"), "HTML");
        assert_eq!(detect_content_type("random text"), "Unknown");
    }
}

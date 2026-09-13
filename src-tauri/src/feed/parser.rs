use crate::error::{AppError, Result};
use crate::models::NewFeedItem;
use feed_rs::parser;

/// Detect whether the document declares itself as an HTML page
/// (handles `<!DOCTYPE html>`, `<html lang=...>`, etc.).
fn is_html_document(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    lower.contains("<!doctype html") || lower.contains("<html ") || lower.contains("<html>")
}

/// Parse RSS/Atom feed content into `NewFeedItem` vectors.
///
/// Validates content before parsing: empty content, compressed bytes (the
/// fetcher is responsible for decompression), JSON-only feeds, and HTML
/// error pages (Cloudflare, Obsidian, etc.).
///
/// `content_md` is populated lazily by [`ensure_content_md`] the first time
/// the item is needed — this avoids blocking the fetch loop on a
/// CPU-intensive HTML→Markdown conversion for every freshly parsed entry.
pub fn parse_feed(feed_content: &str, subscription_id: i64) -> Result<Vec<NewFeedItem>> {
    let trimmed = feed_content.trim();

    if trimmed.is_empty() {
        return Err(AppError::Parse("Empty feed content".into()));
    }

    // After the fetcher rewrite (which lets reqwest handle decompression),
    // reaching here with a gzip/deflate signature is a bug — but if it
    // happens we want to surface it loudly rather than silently mis-parse.
    let bytes = feed_content.as_bytes();
    if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        return Err(AppError::Parse(
            "Feed is still gzip compressed (should be decompressed at fetcher level)".into(),
        ));
    }

    // JSON-only feeds (some endpoints return JSON instead of XML)
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return Err(AppError::Parse(
            "Feed returned JSON format instead of XML/Atom. This URL may not be a valid RSS/Atom feed."
                .into(),
        ));
    }

    // Parse FIRST, diagnose second. Scanning the raw text for `<html` used to
    // reject valid feeds whose entries embed an HTML document (a full-page
    // article body is HTML inside XML), because the marker can legitimately
    // appear anywhere in the payload. A document that only fails to parse is
    // where an HTML error page is worth naming.
    let feed = match parser::parse(feed_content.as_bytes()) {
        Ok(feed) => feed,
        Err(e) => {
            if is_html_document(feed_content) {
                let error_msg = if feed_content.contains("Obsidian") {
                    "This appears to be an Obsidian Publish page, not a valid RSS feed. Please check the feed URL."
                } else if feed_content.contains("Cloudflare") {
                    "Feed is protected by Cloudflare. Please try a different URL or contact the site owner."
                } else {
                    "Feed returned HTML instead of RSS/Atom. The URL may not be a valid feed."
                };
                return Err(AppError::Parse(error_msg.into()));
            }
            return Err(AppError::Parse(format!(
                "{} (content type: {})",
                e,
                detect_content_type(feed_content)
            )));
        }
    };

    if feed.entries.is_empty() {
        // A well-formed feed with no entries is not a parse failure: some
        // publishers ship an empty channel between publication runs. The
        // caller sees zero new items instead of a scary error.
        println!("[feed] parsed a valid feed with no entries; nothing to import");
        return Ok(Vec::new());
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
                // content_md is computed lazily on first read (see
                // ensure_content_md) so we don't pay the html2md cost for
                // every entry during bulk fetch.
                content_md: None,
                content,
                description: entry.summary.map(|s| s.content),
                author: entry.authors.first().map(|a| a.name.clone()),
                published_at: entry.published,
                ..Default::default()
            }
        })
        .collect();

    Ok(items)
}

/// Compute `content_md` from RSS `content` (HTML → Markdown).
///
/// Tries the full `html_to_markdown_pipeline` first (which extracts main
/// content and cleans UI chrome). Falls back to a direct `html2md::parse_html`
/// when the pipeline rejects the input — RSS content is often already the
/// article body and may not satisfy the pipeline's main-content selectors.
///
/// Returns the empty string when the conversion yields no useful text; the
/// caller is expected to treat that as "nothing to show" and skip caching.
///
/// `html2md::parse_html` is CPU-bound and synchronous; call sites that need
/// to run it on a Tokio worker thread should wrap with `spawn_blocking`.
pub fn ensure_content_md(content: &str) -> String {
    if content.is_empty() {
        return String::new();
    }
    let md = crate::content_processor::html_to_markdown_pipeline(content)
        .unwrap_or_else(|_| html2md::parse_html(content));
    if md.trim().is_empty() {
        String::new()
    } else {
        md
    }
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
    } else if is_html_document(content) {
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
    fn test_parse_html_with_attributes() {
        // Regression: <html lang=...> previously slipped past the "<html>" check.
        let result = parse_feed(r#"<html lang="en"><body>not a feed</body></html>"#, 1);
        assert!(matches!(result.unwrap_err(), AppError::Parse(_)));
    }

    /// Regression: a valid RSS document whose entry content embeds HTML (a
    /// full-page article body) must not be mistaken for an HTML error page.
    #[test]
    fn test_parse_feed_with_embedded_html_content() {
        let feed = r#"<?xml version="1.0"?>
        <rss version="2.0"><channel><title>Blog</title>
          <item>
            <title>Article</title>
            <guid>https://example.com/1</guid>
            <link>https://example.com/1</link>
            <description><![CDATA[<html><body><p>Full-page body</p></body></html>]]></description>
          </item>
        </channel></rss>"#;
        let items = parse_feed(feed, 1).expect("valid feed with HTML content must parse");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Article");
    }

    /// A well-formed but empty feed is an empty result, not an error.
    #[test]
    fn test_parse_empty_but_valid_feed() {
        let feed = r#"<?xml version="1.0"?>
        <rss version="2.0"><channel><title>Empty</title></channel></rss>"#;
        let items = parse_feed(feed, 1).expect("valid empty feed must not fail");
        assert!(items.is_empty());
    }

    #[test]
    fn test_parse_json_content() {
        let result = parse_feed("{\"key\": \"value\"}", 1);
        assert!(matches!(result.unwrap_err(), AppError::Parse(_)));
    }

    #[test]
    fn test_ensure_content_md_returns_empty_for_empty_input() {
        assert!(ensure_content_md("").is_empty());
    }

    #[test]
    fn test_ensure_content_md_converts_basic_html() {
        let html = r#"<p>Hello <a href="https://example.com">world</a>!</p>"#;
        let md = ensure_content_md(html);
        assert!(md.contains("[world]"));
        assert!(md.contains("(https://example.com)"));
    }

    #[test]
    fn test_detect_content_type() {
        assert_eq!(detect_content_type("<?xml version=\"1.0\"?>"), "XML");
        assert_eq!(detect_content_type("<rss version=\"2.0\">"), "RSS");
        assert_eq!(
            detect_content_type("<feed xmlns=\"http://www.w3.org/2005/Atom\">"),
            "Atom"
        );
        assert_eq!(detect_content_type("{\"key\": \"value\"}"), "JSON");
        assert_eq!(detect_content_type("<!DOCTYPE html>"), "HTML");
        assert_eq!(detect_content_type("<html lang=\"en\">"), "HTML");
        assert_eq!(detect_content_type("random text"), "Unknown");
    }
}

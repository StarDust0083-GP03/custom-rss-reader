use crate::debug::DebugLogger;
use feed_rs::parser;
use thiserror::Error;

use crate::database::schema::NewFeedItem;

#[derive(Error, Debug)]
pub enum ParseError {
    #[error("Feed parsing error: {0}")]
    FeedError(String),
    #[error("No items found in feed")]
    NoItems,
    #[error("Invalid feed format: {0}")]
    InvalidFormat(String),
}

/// Parse feed content with debug logging
pub fn parse_with_logger(
    feed_content: &str,
    subscription_id: i64,
    debug_logger: &DebugLogger,
) -> Result<Vec<NewFeedItem>, ParseError> {
    debug_logger.log_info(
        "parse",
        &format!(
            "Parsing feed for subscription {}, content length: {}",
            subscription_id,
            feed_content.len()
        ),
    );

    // Log content preview for debugging
    let preview = if feed_content.len() > 500 {
        // Handle UTF-8 boundaries properly
        let end_index = feed_content
            .char_indices()
            .nth(500)
            .map(|(i, _)| i)
            .unwrap_or(feed_content.len());
        format!("{}...", &feed_content[..end_index])
    } else {
        feed_content.to_string()
    };
    debug_logger.log_info("parse", &format!("Content preview: {}", preview));

    // Check if content is empty
    if feed_content.trim().is_empty() {
        return Err(ParseError::FeedError("Empty feed content".to_string()));
    }

    // Note: Compressed data should be handled at the fetcher level
    // This check is kept as a safety measure
    if feed_content.as_bytes().len() >= 2 && feed_content.as_bytes()[0..2] == [0x1f, 0x8b] {
        debug_logger.log_error(
            "parse",
            "Feed appears to still be gzip compressed (should be decompressed at fetcher level)",
        );
        return Err(ParseError::InvalidFormat(
            "Feed is still compressed (gzip). This is a bug in the fetcher.".to_string(),
        ));
    }
    if feed_content.as_bytes().len() >= 1 && feed_content.as_bytes()[0] == 0x78 {
        debug_logger.log_error(
            "parse",
            "Feed appears to still be deflate compressed (should be decompressed at fetcher level)",
        );
        return Err(ParseError::InvalidFormat(
            "Feed is still compressed (deflate). This is a bug in the fetcher.".to_string(),
        ));
    }

    // Check if content is JSON (some feeds return JSON)
    if feed_content.trim().starts_with('{') || feed_content.trim().starts_with('[') {
        debug_logger.log_error("parse", "Feed returned JSON instead of XML/Atom format");
        return Err(ParseError::InvalidFormat("Feed returned JSON format instead of XML/Atom. This URL may not be a valid RSS/Atom feed.".to_string()));
    }

    // Check for empty content that might cause JSON parsing errors
    let trimmed = feed_content.trim();
    if trimmed.is_empty() || trimmed.len() < 10 {
        debug_logger.log_error("parse", "Feed content is empty or too short");
        return Err(ParseError::FeedError(
            "Feed content is empty or too short to be valid".to_string(),
        ));
    }

    // Check if content is HTML (error page)
    if feed_content.contains("<!DOCTYPE html>") || feed_content.contains("<html>") {
        debug_logger.log_error("parse", "Feed returned HTML instead of RSS/Atom");

        // Try to provide more helpful error message
        let error_msg = if feed_content.contains("Obsidian") {
            "This appears to be an Obsidian Publish page, not a valid RSS feed. Please check the feed URL."
        } else if feed_content.contains("Cloudflare") {
            "Feed is protected by Cloudflare. Please try a different URL or contact the site owner."
        } else {
            "Feed returned HTML instead of RSS/Atom. The URL may not be a valid feed."
        };

        return Err(ParseError::InvalidFormat(error_msg.to_string()));
    }

    let feed = parser::parse(feed_content.as_bytes()).map_err(|e| {
        debug_logger.log_error("parse", &format!("Feed parsing error: {}", e));
        ParseError::FeedError(format!(
            "{} (content type: {})",
            e,
            detect_content_type(feed_content)
        ))
    })?;

    if feed.entries.is_empty() {
        debug_logger.log_error("parse", "No items found in feed");
        return Err(ParseError::NoItems);
    }

    debug_logger.log_info(
        "parse",
        &format!("Found {} entries in feed", feed.entries.len()),
    );

    let items: Vec<NewFeedItem> = feed
        .entries
        .into_iter()
        .map(|entry| {
            // Get content from various sources
            let content = entry
                .content
                .and_then(|c| c.body)
                .or(entry.summary.clone().map(|s| s.content));

            // Get GUID from id or link
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
                description: entry.summary.map(|s| s.content),
                author: entry.authors.first().map(|a| a.name.clone()),
                published_at: entry.published,
                is_website_content: false,
                is_read: false,
                is_favorite: false,
                is_read_later: false,
                tags: None,
                category: None,
                translated_title: None,
                translated_content: None,
                translated_at: None,
            }
        })
        .collect();

    // 记录解析后的条目
    if let Ok(items_json) = serde_json::to_string_pretty(&items) {
        debug_logger.log_parsed_items(subscription_id, &items_json);
    }

    debug_logger.log_info(
        "parse",
        &format!("Successfully parsed {} items", items.len()),
    );

    Ok(items)
}

// Helper function to detect content type
fn detect_content_type(content: &str) -> String {
    let content = content.trim();
    if content.starts_with("<?xml") {
        "XML".to_string()
    } else if content.contains("<rss>") {
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

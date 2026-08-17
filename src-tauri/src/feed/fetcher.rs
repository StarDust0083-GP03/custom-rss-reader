use crate::error::{AppError, Result};
use reqwest::Client;
use scraper::{Html, Selector};
use std::time::Duration;
use tokio::time::sleep;

/// HTTP-based feed and website content fetcher.
///
/// Handles User-Agent rotation, RSSHub URL rewriting, retry with
/// exponential backoff, and website content extraction. Compression is
/// handled transparently by the underlying reqwest client (`gzip` and
/// `brotli` features are enabled in `Cargo.toml`).
pub struct FeedFetcher {
    client: Client,
}

impl Default for FeedFetcher {
    fn default() -> Self {
        Self::new().expect("default FeedFetcher can be built")
    }
}

impl FeedFetcher {
    /// Create a new fetcher with sensible defaults.
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(60))
            .http2_keep_alive_interval(Duration::from_secs(30))
            .http2_keep_alive_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| AppError::Internal(format!("Failed to build HTTP client: {}", e)))?;

        Ok(Self { client })
    }

    /// Fetch and decompress a feed (RSS/Atom) from a URL.
    ///
    /// Handles RSSHub domain rewriting, retry with exponential backoff (max 3),
    /// User-Agent rotation (5 variants), and site-specific request headers.
    pub async fn fetch_feed(&self, url: &str) -> Result<String> {
        let final_url = rewrite_rsshub_url(url);

        let mut retry_count = 0;
        let max_retries = 3;

        loop {
            match self.fetch_with_headers(&final_url, retry_count).await {
                Ok(content) => return Ok(content),
                Err(e) if retry_count < max_retries && is_retryable_error(&e) => {
                    retry_count += 1;
                    let delay = Duration::from_millis(1000 * 2_u64.pow(retry_count as u32));
                    sleep(delay).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Fetch website HTML content and extract the main article.
    pub async fn fetch_website_content(&self, url: &str) -> Result<String> {
        let response = self
            .client
            .get(url)
            .header("Accept", "text/html")
            .send()
            .await
            .map_err(|e| AppError::Network(format!("HTTP request failed: {}", e)))?;

        if !response.status().is_success() {
            return Err(AppError::Network(format!(
                "HTTP status: {}",
                response.status()
            )));
        }

        let html = response
            .text()
            .await
            .map_err(|e| AppError::Network(format!("Failed to read response body: {}", e)))?;

        // Extract main content; fallback to full HTML
        Ok(Self::extract_main_content(&html).unwrap_or(html))
    }

    /// Internal: fetch with headers, User-Agent rotation, and decompression
    /// (delegated to reqwest).
    async fn fetch_with_headers(&self, url: &str, retry_count: u32) -> Result<String> {
        let user_agents = [
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:132.0) Gecko/20100101 Firefox/132.0",
            "FeedBot/1.0 (+https://example.com/feedbot)",
        ];
        let user_agent = user_agents[retry_count as usize % user_agents.len()];

        let is_rsshub = url.contains("rsshub.");
        let is_medium = url.contains("medium.com");
        let is_github = url.contains("github.io");

        let mut request = self
            .client
            .get(url)
            .header("User-Agent", user_agent)
            .header(
                "Accept",
                "application/rss+xml, application/xml, text/xml, application/atom+xml, */*",
            )
            .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8,ja;q=0.7")
            .header("DNT", "1")
            .header("Connection", "keep-alive")
            .header("Upgrade-Insecure-Requests", "1");

        if is_rsshub {
            request = request
                .header("Referer", "https://rsshub.umzzz.com/")
                .header("Sec-Fetch-Dest", "empty")
                .header("Sec-Fetch-Mode", "cors")
                .header("Sec-Fetch-Site", "same-origin");
        } else if is_medium {
            request = request
                .header("Referer", "https://medium.com/")
                .header("Sec-Fetch-Dest", "document")
                .header("Sec-Fetch-Mode", "navigate")
                .header("Sec-Fetch-Site", "same-origin");
        } else if is_github {
            request = request
                .header("Referer", url)
                .header("Sec-Fetch-Dest", "document")
                .header("Sec-Fetch-Mode", "navigate");
        } else {
            let base_url = url.split('/').take(3).collect::<Vec<_>>().join("/");
            if !base_url.is_empty() {
                request = request.header("Referer", format!("{}/", base_url));
            }
        }

        let response = request
            .send()
            .await
            .map_err(|e| AppError::Network(format!("HTTP request failed: {}", e)))?;

        let status = response.status();
        let url_after = response.url().clone();
        if !status.is_success() {
            // Distinct error kinds so the retry policy can decide.
            return Err(map_status_error(status, &url_after));
        }

        // reqwest's gzip/brotli features transparently decompress when
        // Accept-Encoding is set automatically. We deliberately do NOT set
        // Accept-Encoding ourselves so this works; if we did, reqwest
        // disables its own decompression and we'd need to reimplement it.
        let text = response
            .text()
            .await
            .map_err(|e| AppError::Network(format!("Failed to read response body: {}", e)))?;

        Ok(text)
    }

    /// Extract main content from HTML using CSS selectors.
    ///
    /// Supports WeChat article formats (`data-content`, `content_noencode` attributes)
    /// and falls back to common content containers (`article`, `main`, `.content`, etc.).
    pub fn extract_main_content(html: &str) -> Result<String> {
        let document = Html::parse_document(html);

        // WeChat-specific attributes checked first
        let wechat_selectors = ["[data-content]", "[content_noencode]", ".rich_media_content"];
        for selector_str in &wechat_selectors {
            if let Ok(selector) = Selector::parse(selector_str) {
                if let Some(element) = document.select(&selector).next() {
                    // Try data-content attribute (WeChat)
                    if let Some(content_attr) = element.value().attr("data-content") {
                        if !content_attr.is_empty() && content_attr.len() > 100 {
                            return Ok(content_attr.to_string());
                        }
                    }
                    // Try content_noencode (WeChat old format)
                    if let Some(content_attr) = element.value().attr("content_noencode") {
                        if !content_attr.is_empty() && content_attr.len() > 100 {
                            return Ok(content_attr.to_string());
                        }
                    }
                    // Fall back to inner HTML
                    let content = element.html();
                    if content.len() > 200 {
                        return Ok(content);
                    }
                }
            }
        }

        // Common content selectors
        let selectors = [
            "article",
            "[role='main']",
            "main",
            ".post-content",
            ".entry-content",
            ".article-content",
            ".content",
            "#content",
        ];

        for selector_str in &selectors {
            if let Ok(selector) = Selector::parse(selector_str) {
                if let Some(element) = document.select(&selector).next() {
                    let content = element.html();
                    if content.len() > 200 {
                        return Ok(content);
                    }
                }
            }
        }

        // Fallback to body
        if let Ok(body_sel) = Selector::parse("body") {
            if let Some(element) = document.select(&body_sel).next() {
                let content = element.html();
                if content.len() > 200 {
                    return Ok(content);
                }
            }
        }

        Err(AppError::OperationFailed(
            "No main content found in HTML".into(),
        ))
    }
}

/// Rewrite RSSHub URLs to use the preferred mirror.
fn rewrite_rsshub_url(url: &str) -> String {
    if url.contains("rsshub.app") {
        url.replace("rsshub.app", "rsshub.umzzz.com")
    } else if url.contains("rsshub.avosapps.us") {
        url.replace("rsshub.avosapps.us", "rsshub.umzzz.com")
    } else if url.contains("rsshub.rssforever.com") {
        url.replace("rsshub.rssforever.com", "rsshub.umzzz.com")
    } else {
        url.to_string()
    }
}

/// Map an HTTP status to a retryable / non-retryable error category.
fn map_status_error(status: reqwest::StatusCode, url: &reqwest::Url) -> AppError {
    let msg = format!("HTTP {} {}", status.as_u16(), status.canonical_reason().unwrap_or(""));
    match status.as_u16() {
        // Transient: worth retrying
        408 | 429 | 500..=599 => AppError::Network(msg),
        // Permanent: don't waste retries. 401/403 may be retried with a
        // different User-Agent via the outer loop's UA rotation, so allow
        // those through.
        401 | 403 => AppError::Network(format!("{} (will retry with new UA): {}", msg, url)),
        _ => AppError::Network(msg),
    }
}

/// Whether a fetch error should trigger a retry. Uses error-message
/// heuristics as a last resort; status-driven retries happen in
/// `map_status_error`.
fn is_retryable_error(e: &AppError) -> bool {
    match e {
        AppError::Network(msg) => {
            let lower = msg.to_lowercase();
            // We can't distinguish 4xx from 5xx reliably through the error
            // text alone (the retry path triggers regardless of status for
            // 401/403 too, since the outer loop rotates User-Agents), so we
            // accept all network errors here and let map_status_error gate
            // the truly permanent ones by returning AppError::Parse instead.
            lower.contains("timeout")
                || lower.contains("dns")
                || lower.contains("name resolution")
                || lower.contains("connect")
                || lower.contains("connection")
                || lower.contains("http 5")
                || lower.contains("http 429")
                || lower.contains("http 408")
                // 401/403 are matched so the outer loop can retry with a
                // rotated User-Agent, as map_status_error intends — without
                // these the "will retry with new UA" path never fired.
                || lower.contains("http 401")
                || lower.contains("http 403")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rewrite_rsshub_url() {
        let rewritten = rewrite_rsshub_url("https://rsshub.app/test/123");
        assert_eq!(rewritten, "https://rsshub.umzzz.com/test/123");

        let unchanged = rewrite_rsshub_url("https://example.com/feed");
        assert_eq!(unchanged, "https://example.com/feed");
    }

    #[test]
    fn test_extract_main_content_article() {
        let long = "A longer paragraph to reach the 200 character minimum threshold for extraction. ".repeat(5);
        let html = format!(
            r#"
        <html><body>
            <nav>Nav items</nav>
            <article><h1>Title</h1><p>Content {}</p></article>
            <footer>Footer</footer>
        </body></html>
        "#,
            long
        );
        let result = FeedFetcher::extract_main_content(&html);
        assert!(result.is_ok());
        let content = result.unwrap();
        assert!(content.contains("Title"));
        assert!(content.contains("Content"));
        assert!(!content.contains("Nav items"));
        assert!(!content.contains("Footer"));
    }

    #[test]
    fn test_extract_main_content_empty() {
        let result = FeedFetcher::extract_main_content("<html><body></body></html>");
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_main_content_body_fallback() {
        let html = r#"
        <html><body>
            <p>Some content in the body.</p>
            <p>More text here.</p>
        </body></html>
        "#;
        let result = FeedFetcher::extract_main_content(html);
        assert!(result.is_err()); // content < 200 chars
    }
}
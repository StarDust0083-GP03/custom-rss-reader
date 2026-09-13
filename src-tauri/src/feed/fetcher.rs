use crate::error::{AppError, Result};
use reqwest::Client;
use scraper::{Html, Selector};
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;
use tokio::time::sleep;

/// Largest decompressed body accepted for a feed (bytes).
const MAX_FEED_BYTES: usize = 16 * 1024 * 1024;

/// Largest decompressed body accepted for an article page (bytes).
const MAX_WEBSITE_BYTES: usize = 8 * 1024 * 1024;

/// HTTP validators for a conditional feed request.
#[derive(Debug, Clone, Copy, Default)]
pub struct FeedValidators<'a> {
    pub etag: Option<&'a str>,
    pub last_modified: Option<&'a str>,
}

/// Result of a (possibly conditional) feed fetch.
#[derive(Debug, Clone)]
pub struct FetchedFeed {
    /// `None` when the server answered 304 Not Modified.
    pub body: Option<String>,
    pub final_url: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// HTTP-based feed and website content fetcher.
///
/// Handles User-Agent rotation, retry with exponential backoff, bounded body
/// reads, and website content extraction. Compression is handled transparently
/// by the underlying reqwest client (`gzip` and `brotli` features are enabled
/// in `Cargo.toml`).
///
/// The URL given by the caller is the URL fetched. A mirror is only used when
/// `RSSHUB_MIRROR` is set explicitly — silently substituting a host meant the
/// app talked to a server the subscription never named.
pub struct FeedFetcher {
    client: Client,
    /// Separate client for article pages so redirects cannot turn a public
    /// feed link into a request to localhost or a private network.
    website_client: Client,
}

impl Default for FeedFetcher {
    fn default() -> Self {
        Self::new().expect("default FeedFetcher can be built")
    }
}

impl FeedFetcher {
    /// Create a new fetcher with sensible defaults.
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: build_client(false)?,
            website_client: build_client(true)?,
        })
    }

    /// Fetch a feed, sending `If-None-Match`/`If-Modified-Since` when the
    /// caller has validators from the previous fetch.
    ///
    /// Retries with exponential backoff (max 3), rotating the User-Agent so a
    /// 403 can pass on a later attempt.
    pub async fn fetch_feed_conditional(
        &self,
        url: &str,
        validators: FeedValidators<'_>,
    ) -> Result<FetchedFeed> {
        let target = apply_rsshub_mirror(url);

        let mut retry_count = 0;
        let max_retries = 3;

        loop {
            match self
                .fetch_with_headers(&target, retry_count, validators)
                .await
            {
                Ok(feed) => return Ok(feed),
                Err(e) if retry_count < max_retries && is_retryable_error(&e) => {
                    retry_count += 1;
                    let delay = Duration::from_millis(1000 * 2_u64.pow(retry_count));
                    sleep(delay).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Fetch website HTML content and extract the main article.
    pub async fn fetch_website_content(&self, url: &str) -> Result<String> {
        let url = validate_website_url(url)?;
        let response = self
            .website_client
            .get(url)
            .header("Accept", "text/html,application/xhtml+xml")
            .send()
            .await
            .map_err(|e| AppError::Network(format!("HTTP request failed: {}", e)))?;

        if !response.status().is_success() {
            return Err(AppError::Network(format!(
                "HTTP status: {}",
                response.status()
            )));
        }
        ensure_text_response(&response, "article page")?;
        let charset = declared_charset(&response);

        let html = read_bounded_text(response, MAX_WEBSITE_BYTES, charset.as_deref()).await?;

        // Extract main content; fallback to full HTML
        Ok(Self::extract_main_content(&html).unwrap_or(html))
    }

    /// Internal: fetch with headers, User-Agent rotation, and decompression
    /// (delegated to reqwest).
    async fn fetch_with_headers(
        &self,
        url: &str,
        retry_count: u32,
        validators: FeedValidators<'_>,
    ) -> Result<FetchedFeed> {
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

        // Conditional GET: the server may answer 304 instead of resending an
        // unchanged feed, which removes the parse work for quiet feeds.
        if let Some(etag) = validators.etag {
            request = request.header("If-None-Match", etag);
        }
        if let Some(last_modified) = validators.last_modified {
            request = request.header("If-Modified-Since", last_modified);
        }

        let response = request
            .send()
            .await
            .map_err(|e| AppError::Network(format!("HTTP request failed: {}", e)))?;

        let status = response.status();
        let url_after = response.url().to_string();
        let etag = header_string(&response, reqwest::header::ETAG);
        let last_modified = header_string(&response, reqwest::header::LAST_MODIFIED);

        if status == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(FetchedFeed {
                body: None,
                final_url: url_after,
                etag,
                last_modified,
            });
        }
        if !status.is_success() {
            // Distinct error kinds so the retry policy can decide.
            let parsed = validate_website_url(&url_after).or_else(|_| {
                reqwest::Url::parse(&url_after).map_err(|_| AppError::Internal("bad url".into()))
            })?;
            return Err(map_status_error(status, &parsed));
        }
        ensure_text_response(&response, "feed")?;
        let charset = declared_charset(&response);

        // reqwest's gzip/brotli features transparently decompress when
        // Accept-Encoding is set automatically. We deliberately do NOT set
        // Accept-Encoding ourselves so this works; if we did, reqwest
        // disables its own decompression and we'd need to reimplement it.
        // Reading chunk-wise bounds the DECOMPRESSED size, which is what
        // actually occupies memory.
        let text = read_bounded_text(response, MAX_FEED_BYTES, charset.as_deref()).await?;

        Ok(FetchedFeed {
            body: Some(text),
            final_url: url_after,
            etag,
            last_modified,
        })
    }

    /// Extract main content from HTML using CSS selectors.
    ///
    /// Supports WeChat article formats (`data-content`, `content_noencode` attributes)
    /// and falls back to common content containers (`article`, `main`, `.content`, etc.).
    pub fn extract_main_content(html: &str) -> Result<String> {
        let document = Html::parse_document(html);

        // WeChat-specific attributes checked first
        let wechat_selectors = [
            "[data-content]",
            "[content_noencode]",
            ".rich_media_content",
        ];
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

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

fn build_client(website_only: bool) -> Result<Client> {
    let mut builder = Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_keepalive(Duration::from_secs(60))
        .http2_keep_alive_interval(Duration::from_secs(30))
        .http2_keep_alive_timeout(Duration::from_secs(10));

    if website_only {
        builder = builder.redirect(reqwest::redirect::Policy::custom(|attempt| {
            if is_safe_website_url(attempt.url()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }));
    }

    builder
        .build()
        .map_err(|e| AppError::Internal(format!("Failed to build HTTP client: {}", e)))
}

fn validate_website_url(raw: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw.trim()).map_err(|_| {
        AppError::Validation("Website URL must be a valid public http(s) URL".into())
    })?;
    if !is_safe_website_url(&url) {
        return Err(AppError::Validation(
            "Website URL must be a valid public http(s) URL".into(),
        ));
    }
    Ok(url)
}

fn is_safe_website_url(url: &reqwest::Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host
        .trim_end_matches('.')
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    if host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host == "metadata.google.internal"
    {
        return false;
    }
    match host.parse::<IpAddr>() {
        Ok(ip) => !is_non_public_ip(ip),
        Err(_) => true,
    }
}

fn is_non_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip == Ipv4Addr::BROADCAST
        }
        IpAddr::V6(ip) => {
            // IPv4-mapped (`::ffff:a.b.c.d`) and the deprecated IPv4-compatible
            // (`::a.b.c.d`) forms both carry an IPv4 destination. None of the v6
            // predicates below match them, so without this normalization the
            // article-fetch guard reports loopback, private, and link-local
            // destinations as public. `::` and `::1` are checked after, by the
            // v6 predicates that describe them exactly.
            if !ip.is_unspecified() && !ip.is_loopback() {
                if let Some(v4) = ip.to_ipv4() {
                    return is_non_public_ip(IpAddr::V4(v4));
                }
            }
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
        }
    }
}

/// Hosts treated as RSSHub entry points for mirror substitution.
const RSSHUB_HOSTS: [&str; 3] = ["rsshub.app", "rsshub.avosapps.us", "rsshub.rssforever.com"];

/// Substitute a configured RSSHub mirror for a known RSSHub host.
///
/// Opt-in via the `RSSHUB_MIRROR` environment variable (e.g.
/// `RSSHUB_MIRROR=https://rsshub.example.com`). Without it the URL is fetched
/// exactly as stored: the previous unconditional rewrite quietly sent every
/// rsshub.app subscription to a third-party host.
fn apply_rsshub_mirror(url: &str) -> String {
    let Ok(mirror) = std::env::var("RSSHUB_MIRROR") else {
        return url.to_string();
    };
    let mirror = mirror.trim().trim_end_matches('/');
    if mirror.is_empty() {
        return url.to_string();
    }
    for host in RSSHUB_HOSTS {
        if url.contains(host) {
            let base = mirror
                .strip_prefix("https://")
                .or_else(|| mirror.strip_prefix("http://"))
                .unwrap_or(mirror);
            return url.replace(host, base);
        }
    }
    url.to_string()
}

/// The charset declared by `Content-Type`, e.g. `gb18030`.
fn declared_charset(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value.split(';').find_map(|part| {
                let part = part.trim();
                part.strip_prefix("charset=")
                    .or_else(|| part.strip_prefix("charset ="))
                    .map(|charset| charset.trim().trim_matches('"').to_ascii_lowercase())
            })
        })
        .filter(|charset| !charset.is_empty())
}

/// Decode a body using the charset the server declared.
///
/// Decoding strictly as UTF-8 was a regression: pages served as GB18030/Big5
/// (common for the Chinese sites this reader follows) or latin-1 failed the
/// whole fetch, so the article silently kept its RSS teaser instead of the
/// full text. Undecodable bytes are replaced, exactly like the browser (and
/// the previous `reqwest::Response::text`) would do — losing a character beats
/// losing the article.
fn decode_body(bytes: Vec<u8>, charset: Option<&str>) -> String {
    let encoding = charset
        .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    let (text, _, had_errors) = encoding.decode(&bytes);
    if had_errors {
        eprintln!(
            "[fetch] replaced undecodable bytes while decoding a {} response",
            encoding.name()
        );
    }
    text.into_owned()
}

/// Read a response body, refusing bodies past `limit`.
async fn read_bounded_text(
    mut response: reqwest::Response,
    limit: usize,
    charset: Option<&str>,
) -> Result<String> {
    if let Some(length) = response.content_length() {
        if length as usize > limit {
            return Err(AppError::Network(format!(
                "response of {} bytes exceeds the {} byte limit",
                length, limit
            )));
        }
    }
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| AppError::Network(format!("Failed to read response body: {}", e)))?
    {
        if body.len() + chunk.len() > limit {
            return Err(AppError::Network(format!(
                "response exceeds the {} byte limit",
                limit
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(decode_body(body, charset))
}

/// Reject clearly non-text responses (a URL that points at a PDF or an image
/// is a user error worth naming, not an empty feed).
fn ensure_text_response(response: &reqwest::Response, what: &str) -> Result<()> {
    let Some(content_type) = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    else {
        return Ok(());
    };
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let is_binary = mime.starts_with("image/")
        || mime.starts_with("video/")
        || mime.starts_with("audio/")
        || matches!(
            mime.as_str(),
            "application/pdf"
                | "application/zip"
                | "application/gzip"
                | "application/octet-stream"
                | "application/x-font-ttf"
        );
    if is_binary {
        return Err(AppError::Parse(format!(
            "URL returned {mime}, not {what} text"
        )));
    }
    Ok(())
}

/// Read a response header as a string, if present and valid ASCII.
fn header_string(
    response: &reqwest::Response,
    header: reqwest::header::HeaderName,
) -> Option<String> {
    response
        .headers()
        .get(header)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string())
}

/// Map an HTTP status to a retryable / non-retryable error category.
fn map_status_error(status: reqwest::StatusCode, url: &reqwest::Url) -> AppError {
    let msg = format!(
        "HTTP {} {}",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    );
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

    /// Without an explicit mirror the URL is fetched exactly as stored.
    #[test]
    fn test_rsshub_mirror_is_opt_in() {
        unsafe { std::env::remove_var("RSSHUB_MIRROR") };
        assert_eq!(
            apply_rsshub_mirror("https://rsshub.app/test/123"),
            "https://rsshub.app/test/123"
        );
        assert_eq!(
            apply_rsshub_mirror("https://example.com/feed"),
            "https://example.com/feed"
        );

        unsafe { std::env::set_var("RSSHUB_MIRROR", "https://mirror.example.com/") };
        assert_eq!(
            apply_rsshub_mirror("https://rsshub.app/test/123"),
            "https://mirror.example.com/test/123"
        );
        // A non-RSSHub feed is never rewritten, even with a mirror set.
        assert_eq!(
            apply_rsshub_mirror("https://example.com/feed"),
            "https://example.com/feed"
        );
        unsafe { std::env::remove_var("RSSHUB_MIRROR") };
    }

    #[test]
    fn test_extract_main_content_article() {
        let long =
            "A longer paragraph to reach the 200 character minimum threshold for extraction. "
                .repeat(5);
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

    #[test]
    fn test_website_url_blocks_local_and_private_destinations() {
        for raw in [
            "http://localhost/article",
            "http://127.0.0.1/article",
            "http://10.0.0.5/article",
            "http://[::1]/article",
            "http://169.254.169.254/latest/meta-data",
        ] {
            let url = reqwest::Url::parse(raw).unwrap();
            assert!(!is_safe_website_url(&url), "should block {raw}");
        }
    }

    #[test]
    fn test_website_url_allows_public_http_and_https() {
        for raw in [
            "https://example.com/article",
            "http://198.51.100.10/article",
        ] {
            let url = reqwest::Url::parse(raw).unwrap();
            assert!(is_safe_website_url(&url), "should allow {raw}");
        }
    }

    /// IPv4-mapped IPv6 literals (`::ffff:127.0.0.1`) are IPv4 addresses, but
    /// none of the IPv6 predicates match them — they must be unwrapped first
    /// or loopback/link-local/private destinations look public.
    #[test]
    fn test_website_url_blocks_ipv4_mapped_ipv6() {
        for raw in [
            "http://[::ffff:127.0.0.1]/article",
            "http://[::ffff:10.0.0.5]/article",
            "http://[::ffff:192.168.1.1]/article",
            "http://[::ffff:169.254.169.254]/latest/meta-data",
            "http://[::127.0.0.1]/article",
            "http://[::ffff:7f00:1]/article",
        ] {
            let url = reqwest::Url::parse(raw).unwrap();
            assert!(!is_safe_website_url(&url), "should block {raw}");
        }
    }

    #[test]
    fn body_decoding_honours_the_declared_charset() {
        // "中文" in GB18030 and in UTF-8 must decode to the same text.
        let gb18030 = vec![0xD6u8, 0xD0, 0xCE, 0xC4];
        assert_eq!(decode_body(gb18030.clone(), Some("gb18030")), "中文");
        assert_eq!(decode_body(gb18030, Some("GBK")), "中文");
        assert_eq!(
            decode_body("中文".as_bytes().to_vec(), Some("utf-8")),
            "中文"
        );
        // No charset declared: assume UTF-8.
        assert_eq!(decode_body("中文".as_bytes().to_vec(), None), "中文");
        // Undecodable bytes are replaced rather than failing the fetch.
        let broken = vec![0x41u8, 0xFF, 0xFE, 0x42];
        let decoded = decode_body(broken, Some("utf-8"));
        assert!(decoded.starts_with('A') && decoded.ends_with('B'));
    }

    #[test]
    fn test_is_non_public_ip_table() {
        for raw in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.0.1",
            "169.254.169.254",
            "0.0.0.0",
            "255.255.255.255",
            "::1",
            "::",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
        ] {
            let ip: IpAddr = raw.parse().unwrap();
            assert!(is_non_public_ip(ip), "should be non-public: {raw}");
        }
        for raw in [
            "8.8.8.8",
            "198.51.100.10",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            let ip: IpAddr = raw.parse().unwrap();
            assert!(!is_non_public_ip(ip), "should be public: {raw}");
        }
    }
}

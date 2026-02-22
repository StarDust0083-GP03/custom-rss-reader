use crate::debug::DebugLogger;
use reqwest::Client;
use scraper::{Html, Selector};
use std::time::Duration;
use thiserror::Error;
use tokio::time::sleep;
use std::env;

#[derive(Error, Debug)]
pub enum FetchError {
    #[error("HTTP error: {0}")]
    HttpError(#[from] reqwest::Error),
    #[error("No content found")]
    NoContent,
    #[error("Decompression error: {0}")]
    DecompressionError(String),
}

pub struct FeedFetcher {
    client: Client,
    debug_logger: DebugLogger,
}

impl FeedFetcher {
    pub fn new() -> Self {
        Self::with_proxy(None)
    }

    pub fn with_proxy(proxy_url: Option<String>) -> Self {
        let mut builder = Client::builder()
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(60))
            .http2_keep_alive_interval(Duration::from_secs(30))
            .http2_keep_alive_timeout(Duration::from_secs(10))
            .connection_verbose(true);

        // Try to get proxy from environment variable or parameter
        let proxy = proxy_url.or_else(|| env::var("HTTP_PROXY").ok())
            .or_else(|| env::var("http_proxy").ok())
            .or_else(|| env::var("HTTPS_PROXY").ok())
            .or_else(|| env::var("https_proxy").ok())
            .or_else(|| env::var("ALL_PROXY").ok())
            .or_else(|| env::var("all_proxy").ok());

        if let Some(proxy_addr) = proxy {
            match reqwest::Proxy::all(&proxy_addr) {
                Ok(proxy) => {
                    builder = builder.proxy(proxy);
                    eprintln!("[FeedFetcher] Using proxy: {}", proxy_addr);
                }
                Err(e) => {
                    eprintln!("[FeedFetcher] Failed to set proxy {}: {}", proxy_addr, e);
                }
            }
        }

        let client = builder.build().unwrap();

        let debug_logger = DebugLogger::new_temp();

        Self {
            client,
            debug_logger,
        }
    }

    pub fn with_debug(mut self, debug_logger: DebugLogger) -> Self {
        self.debug_logger = debug_logger;
        self
    }

    /// 尝试解压内容（如果需要）
    fn try_decompress(&self, bytes: &[u8]) -> Result<String, FetchError> {
        // 如果内容为空，返回错误
        if bytes.is_empty() {
            return Err(FetchError::NoContent);
        }

        // 检查是否是 gzip 压缩数据 (magic bytes: 0x1f 0x8b)
        if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
            self.debug_logger.log_info("try_decompress", "Detected gzip compressed data, attempting decompression");
            match self.decompress_gzip(bytes) {
                Ok(text) => return Ok(text),
                Err(e) => {
                    self.debug_logger.log_error("try_decompress", &format!("Gzip decompression failed: {}", e));
                    // 继续尝试其他方法
                }
            }
        }

        // 检查是否是 deflate 压缩数据 (通常以 0x78 开头)
        if bytes.len() >= 1 && bytes[0] == 0x78 {
            self.debug_logger.log_info("try_decompress", "Detected deflate compressed data, attempting decompression");
            match self.decompress_deflate(bytes) {
                Ok(text) => return Ok(text),
                Err(e) => {
                    self.debug_logger.log_error("try_decompress", &format!("Deflate decompression failed: {}", e));
                    // 继续尝试其他方法
                }
            }
        }

        // 尝试直接转换为 UTF-8
        match std::str::from_utf8(bytes) {
            Ok(text) => Ok(text.to_string()),
            Err(_) => {
                // 如果不是有效的 UTF-8，尝试 Latin-1
                let text = bytes.iter().map(|&b| b as char).collect::<String>();
                self.debug_logger.log_info("try_decompress", "Content is not valid UTF-8, using Latin-1 fallback");
                Ok(text)
            }
        }
    }

    /// 解压 gzip 数据
    fn decompress_gzip(&self, bytes: &[u8]) -> Result<String, FetchError> {
        use flate2::read::GzDecoder;
        use std::io::Read;

        let mut decoder = GzDecoder::new(bytes);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed)
            .map_err(|e| FetchError::DecompressionError(format!("Gzip decompression failed: {}", e)))?;

        // Try UTF-8 first, then Latin-1 fallback
        match String::from_utf8(decompressed.clone()) {
            Ok(text) => Ok(text),
            Err(_) => {
                // Latin-1 fallback
                let text = decompressed.iter().map(|&b| b as char).collect::<String>();
                Ok(text)
            }
        }
    }

    /// 解压 deflate 数据
    fn decompress_deflate(&self, bytes: &[u8]) -> Result<String, FetchError> {
        use flate2::read::DeflateDecoder;
        use std::io::Read;

        let mut decoder = DeflateDecoder::new(bytes);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed)
            .map_err(|e| FetchError::DecompressionError(format!("Deflate decompression failed: {}", e)))?;

        // Try UTF-8 first, then Latin-1 fallback
        match String::from_utf8(decompressed.clone()) {
            Ok(text) => Ok(text),
            Err(_) => {
                // Latin-1 fallback
                let text = decompressed.iter().map(|&b| b as char).collect::<String>();
                Ok(text)
            }
        }
    }

    pub async fn fetch_feed(&self, url: &str) -> Result<String, FetchError> {
        // 将各种 RSSHub 实例替换为镜像实例 rsshub.umzzz.com
        let final_url = if url.contains("rsshub.app") {
            url.replace("rsshub.app", "rsshub.umzzz.com")
        } else if url.contains("rsshub.avosapps.us") {
            url.replace("rsshub.avosapps.us", "rsshub.umzzz.com")
        } else if url.contains("rsshub.rssforever.com") {
            url.replace("rsshub.rssforever.com", "rsshub.umzzz.com")
        } else {
            url.to_string()
        };

        self.debug_logger
            .log_info("fetch_feed", &format!("Fetching URL: {} (original: {})", final_url, url));

        // 尝试多种方式获取内容，包括重试和不同的 User-Agent
        let mut retry_count = 0;
        let max_retries = 3;

        loop {
            match self.fetch_with_headers(&final_url, retry_count).await {
                Ok(content) => return Ok(content),
                Err(FetchError::HttpError(e)) if retry_count < max_retries => {
                    let error_str = e.to_string();
                    let is_timeout = error_str.contains("timeout") || error_str.contains("timed out");
                    let is_dns = error_str.contains("dns") || error_str.contains("name resolution");
                    let is_connection = error_str.contains("connect") || error_str.contains("connection");
                    let is_403 = e.status() == Some(reqwest::StatusCode::FORBIDDEN);

                    if is_timeout || is_dns || is_connection || is_403 {
                        self.debug_logger.log_warning("fetch_feed", &format!("Connection error, retrying... ({}/{})", retry_count + 1, max_retries));
                        retry_count += 1;
                        let delay = Duration::from_millis(1000 * 2_u64.pow(retry_count as u32));
                        sleep(delay).await;
                        continue;
                    }
                    return Err(FetchError::HttpError(e));
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn fetch_with_headers(&self, final_url: &str, retry_count: u32) -> Result<String, FetchError> {
        let user_agents = vec![
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:132.0) Gecko/20100101 Firefox/132.0",
            "FeedBot/1.0 (+https://example.com/feedbot)",
        ];

        let user_agent = user_agents[retry_count as usize % user_agents.len()];

        // 检测是否是 RSSHub URL
        let is_rsshub = final_url.contains("rsshub.");
        let is_medium = final_url.contains("medium.com");
        let is_github = final_url.contains("github.io");

        let mut request = self.client.get(final_url);

        // 添加更完整的请求头，模仿真实浏览器
        request = request
            .header("User-Agent", user_agent)
            .header("Accept", "application/rss+xml, application/xml, text/xml, application/atom+xml, */*")
            .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8,ja;q=0.7")
            .header("Accept-Encoding", "gzip, deflate, br")
            .header("DNT", "1")
            .header("Connection", "keep-alive")
            .header("Upgrade-Insecure-Requests", "1");

        // 对于 RSSHub，添加额外的请求头
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
                .header("Referer", final_url)
                .header("Sec-Fetch-Dest", "document")
                .header("Sec-Fetch-Mode", "navigate");
        } else {
            // 添加通用 Referer
            let base_url = final_url.split('/').take(3).collect::<Vec<_>>().join("/");
            if !base_url.is_empty() {
                request = request.header("Referer", &format!("{}/", base_url));
            }
        }

        let response = request.send().await?;

        if !response.status().is_success() {
            let error = format!("HTTP status: {}", response.status());
            self.debug_logger.log_error("fetch_feed", &error);
            return Err(FetchError::HttpError(
                response.error_for_status().unwrap_err(),
            ));
        }

        // 首先获取原始字节
        let bytes = response.bytes().await?;

        // 尝试解压内容
        let content = self.try_decompress(&bytes)?;

        // 记录原始 RSS 内容
        self.debug_logger.log_raw_feed(0, final_url, &content);

        Ok(content)
    }

    pub async fn fetch_website_content(&self, url: &str) -> Result<String, FetchError> {
        self.debug_logger.log_info(
            "fetch_website_content",
            &format!("Fetching website: {}", url),
        );

        let response = self
            .client
            .get(url)
            .header("Accept", "text/html")
            .send()
            .await?;

        if !response.status().is_success() {
            let error = format!("HTTP status: {}", response.status());
            self.debug_logger.log_error("fetch_website_content", &error);
            return Err(FetchError::HttpError(
                response.error_for_status().unwrap_err(),
            ));
        }

        let html = response.text().await?;

        // 记录原始 HTML 内容
        self.debug_logger.log_website_content(0, url, &html);

        // Extract main content using heuristics
        let result = match self.extract_main_content(&html) {
            Ok(content) => {
                self.debug_logger.log_info("fetch_website_content", &format!("Extracted content length: {}", content.len()));
                content
            }
            Err(e) => {
                self.debug_logger.log_warning("fetch_website_content", &format!("Content extraction failed: {}, returning full HTML", e));
                html
            }
        };
        
        // Log first 500 chars for debugging
        let preview = result.chars().take(500).collect::<String>();
        self.debug_logger.log_info("fetch_website_content", &format!("Content preview: {}", preview));
        
        Ok(result)
    }

    fn extract_main_content(&self, html: &str) -> Result<String, FetchError> {
        let document = Html::parse_document(html);

        // Special handling for WeChat articles - check for content_noencode
        let selectors_with_wechat = vec![
            // WeChat specific
            "[data-content]",
            "[content_noencode]",
            ".rich_media_content",
            // Common selectors
            "article",
            "[role='main']",
            "main",
            ".post-content",
            ".entry-content",
            ".article-content",
            ".content",
            "#content",
        ];

        for selector_str in selectors_with_wechat {
            if let Ok(selector) = Selector::parse(selector_str) {
                if let Some(element) = document.select(&selector).next() {
                    // Try to get content from data-content attribute first (WeChat)
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
                    // Fall back to HTML content
                    let content = element.html();
                    if content.len() > 200 {
                        return Ok(content);
                    }
                }
            }
        }

        // Fallback to body content
        let body_selector = Selector::parse("body").unwrap();
        if let Some(element) = document.select(&body_selector).next() {
            let content = element.html();
            if content.len() > 200 {
                return Ok(content);
            }
        }

        Err(FetchError::NoContent)
    }
}

impl Default for FeedFetcher {
    fn default() -> Self {
        Self::new()
    }
}

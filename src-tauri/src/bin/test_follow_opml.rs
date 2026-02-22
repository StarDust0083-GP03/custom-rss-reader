
// Simple OPML parser
#[derive(Debug, Clone)]
struct OpmlOutline {
    text: String,
    title: String,
    xml_url: String,
    html_url: String,
}

fn parse_opml(content: &str) -> Vec<OpmlOutline> {
    let mut outlines = Vec::new();

    // Simple XML parsing - find all outline tags
    // The content is all on one line, so we need to parse differently
    let mut pos = 0;
    while pos < content.len() {
        // Find next outline tag
        if let Some(start) = content[pos..].find("<outline") {
            let start = pos + start;
            // Find the end of this tag
            if let Some(end) = content[start..].find('>') {
                let end = start + end + 1;
                let tag_content = &content[start..end];

                // Extract attributes
                let xml_url = extract_attr(tag_content, "xmlUrl").unwrap_or_default();

                // Only include if it has xmlUrl
                if !xml_url.is_empty() {
                    let text = extract_attr(tag_content, "text").unwrap_or_default();
                    let title = extract_attr(tag_content, "title").unwrap_or_default();
                    let html_url = extract_attr(tag_content, "htmlUrl").unwrap_or_default();

                    outlines.push(OpmlOutline {
                        text,
                        title,
                        xml_url,
                        html_url,
                    });
                }

                pos = end;
            } else {
                break;
            }
        } else {
            break;
        }
    }

    outlines
}

fn extract_attr(line: &str, attr: &str) -> Option<String> {
    let pattern = format!(r#"{}=""#, attr);
    let start = line.find(&pattern)?;
    let start = start + pattern.len();
    let end = line[start..].find("\"")?;
    Some(line[start..start + end].to_string())
}

fn normalize_rsshub_url(url: &str) -> String {
    // Replace rsshub.app with rsshub.umzzz.com
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

fn is_rsshub_url(url: &str) -> bool {
    url.contains("rsshub.app")
        || url.contains("rsshub.avosapps.us")
        || url.contains("rsshub.rssforever.com")
        || url.contains("rsshub.umzzz.com")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== RSS Feed URL Handling Test ===\n");

    // Read OPML file
    let opml_path = std::path::Path::new("/home/hsf/Downloads/follow.opml");
    let content = std::fs::read_to_string(opml_path)?;
    println!("✓ Loaded OPML file from {:?}\n", opml_path);

    // Parse OPML
    let outlines = parse_opml(&content);
    println!("✓ Found {} feeds in OPML\n", outlines.len());

    // Categorize feeds
    let mut rsshub_feeds = Vec::new();
    let mut normal_feeds = Vec::new();

    for outline in &outlines {
        if is_rsshub_url(&outline.xml_url) {
            rsshub_feeds.push(outline.clone());
        } else {
            normal_feeds.push(outline.clone());
        }
    }

    println!("--- Feed Classification ---");
    println!("Normal URLs: {}", normal_feeds.len());
    println!("RSSHub URLs: {}", rsshub_feeds.len());
    println!();

    // Show some examples of RSSHub feeds
    println!("--- RSSHub Feed Examples ---");
    for (i, feed) in rsshub_feeds.iter().take(5).enumerate() {
        println!("{}. {} - {}", i + 1, feed.title, feed.xml_url);
    }
    println!("... and {} more\n", rsshub_feeds.len().saturating_sub(5));

    // Show some examples of normal feeds
    println!("--- Normal Feed Examples ---");
    for (i, feed) in normal_feeds.iter().take(5).enumerate() {
        println!("{}. {} - {}", i + 1, feed.title, feed.xml_url);
    }
    println!("... and {} more\n", normal_feeds.len().saturating_sub(5));

    // Test URL normalization
    println!("=== URL Normalization Test ===\n");

    println!("--- RSSHub URL Replacement ---");
    for feed in rsshub_feeds.iter().take(10) {
        let original = &feed.xml_url;
        let normalized = normalize_rsshub_url(original);

        let status = if normalized.contains("rsshub.umzzz.com") {
            "✓"
        } else {
            "✗"
        };

        println!("{} {}", status, feed.title);
        println!("  Original:   {}", original);
        println!("  Normalized: {}", normalized);

        if normalized.contains("rsshub.umzzz.com") {
            println!("  ✓ Correctly replaced with rsshub.umzzz.com");
        } else {
            println!("  ✗ Failed to replace rsshub domain");
        }
        println!();
    }

    // Test that normal URLs are not modified
    println!("--- Normal URL Preservation ---");
    for feed in normal_feeds.iter().take(5) {
        let original = &feed.xml_url;
        let normalized = normalize_rsshub_url(original);

        if original == &normalized {
            println!("✓ {} - URL unchanged (correct)", feed.title);
        } else {
            println!("✗ {} - URL was modified (incorrect)", feed.title);
            println!("  Original:   {}", original);
            println!("  Normalized: {}", normalized);
        }
    }
    println!();

    // Test actual feed fetching for a few feeds
    println!("=== Feed Fetching Test ===\n");

    // Build client with connection pool settings
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    println!("Testing RSSHub feeds:");
    let mut rsshub_success = 0;
    let mut rsshub_failed = 0;

    for feed in rsshub_feeds.iter().take(10) {
        let url = normalize_rsshub_url(&feed.xml_url);
        print!("  {} ... ", feed.title);

        match fetch_feed(&client, &url).await {
            Ok(info) => {
                println!("✓ {}", info);
                rsshub_success += 1;
            }
            Err(e) => {
                println!("✗ Failed: {}", e);
                rsshub_failed += 1;
            }
        }
    }

    println!("\nTesting normal feeds:");
    let mut normal_success = 0;
    let mut normal_failed = 0;

    for feed in normal_feeds.iter().take(10) {
        let url = &feed.xml_url;
        print!("  {} ... ", feed.title);

        match fetch_feed(&client, url).await {
            Ok(info) => {
                println!("✓ {}", info);
                normal_success += 1;
            }
            Err(e) => {
                println!("✗ Failed: {}", e);
                normal_failed += 1;
            }
        }
    }

    println!();
    println!("=== Test Summary ===");
    println!("Total feeds: {}", outlines.len());
    println!("RSSHub feeds: {} (tested: {}, success: {}, failed: {})",
        rsshub_feeds.len(), rsshub_success + rsshub_failed, rsshub_success, rsshub_failed);
    println!("Normal feeds: {} (tested: {}, success: {}, failed: {})",
        normal_feeds.len(), normal_success + normal_failed, normal_success, normal_failed);
    println!();

    // Final verdict
    println!("=== Verdict ===");
    if rsshub_failed == 0 && normal_failed == 0 {
        println!("✓ All tested feeds fetched successfully!");
        println!("✓ RSSHub URL normalization working correctly!");
        println!("✓ Normal URL handling working correctly!");
    } else {
        println!("Some feeds failed to fetch:");
        if rsshub_failed > 0 {
            println!("  - {} RSSHub feed(s) failed", rsshub_failed);
        }
        if normal_failed > 0 {
            println!("  - {} normal feed(s) failed", normal_failed);
        }
    }

    Ok(())
}

async fn fetch_feed(client: &reqwest::Client, url: &str) -> Result<String, Box<dyn std::error::Error>> {
    let response = client
        .get(url)
        .header("Accept", "application/rss+xml, application/xml, text/xml, application/atom+xml")
        .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
        .send()
        .await;

    let response = match response {
        Ok(r) => r,
        Err(e) => {
            // Try to get more error details
            let err_str = e.to_string();
            if err_str.contains("timed out") {
                return Err(format!("Connection timed out").into());
            } else if err_str.contains("dns") || err_str.contains("DNS") {
                return Err(format!("DNS resolution failed").into());
            } else if err_str.contains("connection") || err_str.contains("Connection") {
                return Err(format!("Connection refused/reset: {}", err_str).into());
            } else {
                return Err(format!("Connection error: {}", err_str).into());
            }
        }
    };

    let status = response.status();
    let status_code = status.as_u16();

    if !status.is_success() {
        let error_body = response.text().await.unwrap_or_default();
        if error_body.len() > 200 {
            return Err(format!("HTTP {} - Body preview: {}", status_code, &error_body[..200]).into());
        } else {
            return Err(format!("HTTP {} - {}", status_code, error_body).into());
        }
    }

    // Check if response is RSS/Atom
    let content_type = response.headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    let bytes = response.bytes().await?;

    // Quick check for RSS/Atom content
    let content = String::from_utf8_lossy(&bytes);
    if content.contains("<rss") || content.contains("<feed") || content.contains("<entry") {
        Ok(format!("HTTP {} - {}", status_code, content_type))
    } else {
        // Show preview of what we got
        let preview = if content.len() > 300 {
            format!("{}...", &content[..300])
        } else {
            content.to_string()
        };
        Err(format!("Not valid RSS/Atom content (content-type: {}). Preview: {}", content_type, preview).into())
    }
}

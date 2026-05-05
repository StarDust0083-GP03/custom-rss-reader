use crate::error::{AppError, Result};
use scraper::{Html, Selector};

/// Extract the main content from an HTML page using CSS selectors.
///
/// Returns the HTML of the first matching content element, or an error if none found.
/// Selectors are ordered by specificity, mirroring the original backend's
/// `FeedFetcher::extract_main_content` and the Readability.js approach used by MD-This-Page.
pub fn extract_main_content(html: &str) -> Result<String> {
    let document = Html::parse_document(html);

    let selectors = vec![
        "article",
        "[role='main']",
        "main",
        ".post-content",
        ".entry-content",
        ".article-content",
        ".content",
        "#content",
        ".post",
        ".entry",
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

    // Fallback to <body>
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

/// Convert HTML to Markdown.
///
/// Uses the `html2md` crate, which follows a similar approach to Turndown.js
/// (the library used by MD-This-Page).
pub fn html_to_markdown(html: &str) -> Result<String> {
    if html.trim().is_empty() {
        return Err(AppError::Validation("Empty HTML content".into()));
    }

    let md = html2md::parse_html(html);
    Ok(md)
}

/// Full pipeline: extract main content from HTML, then convert to Markdown.
///
/// This mirrors the MD-This-Page two-step approach:
/// 1. Content extraction (Readability.js → scraper selectors)
/// 2. HTML-to-Markdown conversion (Turndown.js → html2md)
///
/// Before extraction, non-content elements (nav, footer, button, svg, etc.)
/// are stripped to prevent them from appearing in the final markdown.
pub fn html_to_markdown_pipeline(html: &str) -> Result<String> {
    let cleaned = clean_html_for_markdown(html);
    let main_content = extract_main_content(&cleaned)?;
    let md = html_to_markdown(&main_content)?;
    let md = clean_markdown(&md);
    Ok(collapse_empty_lines(&md))
}

/// Strip non-content HTML elements that have no place in a reading view.
///
/// Removes: nav, footer, aside, button, form, svg, script, style, noscript
/// — anything that is UI chrome, decoration, or interactivity rather than
/// article text. Also strips their entire contents (not just the tags).
fn clean_html_for_markdown(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut pos = 0;

    // Tags to remove entirely (opening + contents + closing).
    let block_tags = ["nav", "footer", "aside", "button", "form", "svg",
                       "script", "style", "noscript", "select", "input", "textarea"];

    while pos < html.len() {
        // Look for the start of any tag we want to remove
        let mut earliest_start = None;
        let mut earliest_tag = None;

        for tag in &block_tags {
            // Look for <tag or <tag  or <tag>
            if let Some(s) = find_open_tag_start(html, pos, tag) {
                match earliest_start {
                    None => {
                        earliest_start = Some(s);
                        earliest_tag = Some(tag);
                    }
                    Some(current) if s < current => {
                        earliest_start = Some(s);
                        earliest_tag = Some(tag);
                    }
                    _ => {}
                }
            }
        }

        match earliest_start {
            Some(start) => {
                // Copy everything before the tag
                result.push_str(&html[pos..start]);

                // Find the closing tag
                let close_tag = format!("</{}>", earliest_tag.unwrap());
                if let Some(end) = html[start..].find(&close_tag) {
                    pos = start + end + close_tag.len();
                } else {
                    // No matching close tag — skip past '>' of opening tag
                    if let Some(gt) = html[start..].find('>') {
                        pos = start + gt + 1;
                    } else {
                        pos = start + 1;
                    }
                }
            }
            None => {
                // No more block tags to remove
                result.push_str(&html[pos..]);
                break;
            }
        }
    }

    result
}

/// Find the start of an opening tag for the given element name.
fn find_open_tag_start(html: &str, from: usize, tag: &str) -> Option<usize> {
    let bytes = html.as_bytes();
    let tag_bytes = tag.as_bytes();
    let mut i = from;

    while i < html.len() {
        if bytes[i] == b'<' {
            // Check if it's followed by the tag name (possibly with leading '/')
            let after_lt = i + 1;
            if after_lt < html.len() {
                let start = if bytes[after_lt] == b'/' { after_lt + 1 } else { after_lt };
                if start + tag_bytes.len() <= html.len() {
                    if &html.as_bytes()[start..start + tag_bytes.len()] == tag_bytes {
                        // Make sure it's followed by space, >, or /
                        let after_tag = start + tag_bytes.len();
                        if after_tag >= html.len()
                            || bytes[after_tag] == b'>'
                            || bytes[after_tag] == b' '
                            || bytes[after_tag] == b'/'
                        {
                            return Some(i);
                        }
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// Collapse three or more consecutive newlines into two (one blank line).
fn collapse_empty_lines(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut consecutive = 0;

    for ch in s.chars() {
        if ch == '\n' {
            consecutive += 1;
            if consecutive <= 2 {
                result.push(ch);
            }
        } else {
            consecutive = 0;
            result.push(ch);
        }
    }

    result
}

/// Clean up markdown output by removing non-content patterns:
///
/// - Empty links `[](url)` (from icon-only `<a>` tags)
/// - Links whose URL contains non-content patterns like `/signin`, `/vote/`, etc.
pub fn clean_markdown(md: &str) -> String {
    let mut result = String::with_capacity(md.len());
    let bytes = md.as_bytes();
    let len = md.len();
    let mut i = 0;

    let non_content_patterns = [
        "/signin", "/vote/", "/bookmark/", "/share/",
        "clap", "actionUrl=", "source=", "post_",
        "bookmark_", "clap_footer",
    ];

    while i < len {
        // Empty markdown link: [](
        if i + 2 < len && &bytes[i..i + 3] == b"[](" {
            let mut depth = 1;
            let mut j = i + 3;
            while j < len && depth > 0 {
                if bytes[j] == b'(' { depth += 1; }
                else if bytes[j] == b')' { depth -= 1; }
                j += 1;
            }
            i = j;
            continue;
        }

        // Empty image: ![](
        if i + 3 < len && &bytes[i..i + 4] == b"![](" {
            let mut depth = 1;
            let mut j = i + 4;
            while j < len && depth > 0 {
                if bytes[j] == b'(' { depth += 1; }
                else if bytes[j] == b')' { depth -= 1; }
                j += 1;
            }
            i = j;
            continue;
        }

        // Non-content link: [text](url with signin/vote/etc)
        // Scan ahead from '[' to find matching '](', then check URL
        if bytes[i] == b'[' {
            let after_open = &md[i + 1..];
            if let Some(close_bracket) = after_open.find(']') {
                let close_bracket_abs = i + 1 + close_bracket;
                // Check if followed by '('
                if close_bracket_abs + 1 < len && bytes[close_bracket_abs + 1] == b'(' {
                    let url_start = close_bracket_abs + 2;
                    let mut depth = 1;
                    let mut j = url_start;
                    while j < len && depth > 0 {
                        if bytes[j] == b'(' { depth += 1; }
                        else if bytes[j] == b')' { depth -= 1; }
                        j += 1;
                    }
                    let url = &md[url_start..j - 1];
                    if non_content_patterns.iter().any(|p| url.contains(p)) {
                        i = j; // skip the entire [text](url)
                        continue;
                    }
                }
            }
        }

        let ch = md[i..].chars().next().unwrap_or(' ');
        let len_ch = ch.len_utf8();
        result.push_str(&md[i..i + len_ch]);
        i += len_ch;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate a long paragraph to exceed the 200-char threshold.
    fn long_para(text: &str) -> String {
        // Repeat the text to exceed 200 chars
        let repeat = (210 / text.len()).max(2);
        (0..repeat).map(|_| text).collect::<Vec<_>>().join(" ")
    }

    // -----------------------------------------------------------------------
    // extract_main_content
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_main_content_article() {
        let html = format!(
            r#"
        <html><body>
            <nav>Nav stuff</nav>
            <article>
                <h1>Article Title</h1>
                <p>{}</p>
                <p>More paragraphs here.</p>
            </article>
            <footer>Footer stuff</footer>
        </body></html>
        "#,
            long_para("This is the main content of the article.")
        );
        let result = extract_main_content(&html).unwrap();
        assert!(result.contains("Article Title"));
        assert!(result.contains("main content of the article"));
        assert!(!result.contains("Nav stuff"));
        assert!(!result.contains("Footer stuff"));
    }

    #[test]
    fn test_extract_main_content_role_main() {
        let html = format!(
            r#"
        <html><body>
            <div role="main">
                <h1>Main Content</h1>
                <p>{}</p>
            </div>
        </body></html>
        "#,
            long_para("Content here, enough to pass the threshold check.")
        );
        let result = extract_main_content(&html).unwrap();
        assert!(result.contains("Main Content"));
    }

    #[test]
    fn test_extract_main_content_empty_html() {
        let result = extract_main_content("<html><body></body></html>");
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_main_content_fallback_body() {
        let html = format!(
            r#"
        <html><body>
            <p>{}</p>
            <p>{}</p>
        </body></html>
        "#,
            long_para("Just a paragraph without any semantic container."),
            long_para("Another paragraph that makes the body content long enough to extract.")
        );
        let result = extract_main_content(&html).unwrap();
        assert!(result.contains("Just a paragraph"));
    }

    // -----------------------------------------------------------------------
    // html_to_markdown
    // -----------------------------------------------------------------------

    #[test]
    fn test_html_to_markdown_heading() {
        let html = "<h1>Title</h1><h2>Subtitle</h2><h3>Section</h3>";
        let md = html_to_markdown(html).unwrap();
        // html2md uses Setext-style for h1/h2, ATX with closing for h3+
        assert!(md.contains("Title"));
        assert!(md.contains("Subtitle"));
        assert!(md.contains("Section"));
        // h3 uses ### Section ### format
        assert!(md.contains("###"));
    }

    #[test]
    fn test_html_to_markdown_link() {
        let html = r#"<a href="https://example.com">Example</a>"#;
        let md = html_to_markdown(html).unwrap();
        assert!(md.contains("[Example]"));
        assert!(md.contains("(https://example.com)"));
    }

    #[test]
    fn test_html_to_markdown_image() {
        let html = r#"<img src="https://example.com/img.png" alt="An image"/>"#;
        let md = html_to_markdown(html).unwrap();
        assert!(md.contains("![An image]"));
        assert!(md.contains("(https://example.com/img.png)"));
    }

    #[test]
    fn test_html_to_markdown_list() {
        let html = "<ul><li>Item 1</li><li>Item 2</li><li>Item 3</li></ul>";
        let md = html_to_markdown(html).unwrap();
        // html2md uses asterisk for unordered lists
        assert!(md.contains("Item 1"));
        assert!(md.contains("Item 2"));
        assert!(md.contains("Item 3"));
        assert!(md.contains('*'));
    }

    #[test]
    fn test_html_to_markdown_ordered_list() {
        let html = "<ol><li>First</li><li>Second</li></ol>";
        let md = html_to_markdown(html).unwrap();
        assert!(md.contains("1. First"));
        assert!(md.contains("2. Second"));
    }

    #[test]
    fn test_html_to_markdown_code_block() {
        let html = "<pre><code>let x = 1;</code></pre>";
        let md = html_to_markdown(html).unwrap();
        assert!(md.contains("let x = 1;"));
    }

    #[test]
    fn test_html_to_markdown_empty() {
        let result = html_to_markdown("");
        assert!(result.is_err());
    }

    #[test]
    fn test_html_to_markdown_basic_paragraph() {
        let html = "<p>Hello <strong>world</strong>.</p>";
        let md = html_to_markdown(html).unwrap();
        assert!(md.contains("Hello"));
        assert!(md.contains("world"));
    }

    #[test]
    fn test_html_to_markdown_linked_image() {
        // Image wrapped in a link — common RSS pattern
        let html = r#"<a href="https://example.com"><img src="https://example.com/img.jpg" alt="Photo"/></a>"#;
        let md = html_to_markdown(html).unwrap();
        // Should produce markdown link containing markdown image
        assert!(md.contains("[![Photo]"));
        assert!(md.contains("(https://example.com/img.jpg)"));
        assert!(md.contains("](https://example.com)"));
    }

    #[test]
    fn test_html_to_markdown_image_no_alt() {
        let html = r#"<img src="https://example.com/img.png"/>"#;
        let md = html_to_markdown(html).unwrap();
        // Should produce image with empty alt text
        assert!(md.contains("![]"));
        assert!(md.contains("(https://example.com/img.png)"));
    }

    #[test]
    fn test_html_to_markdown_link_with_title() {
        let html = r#"<a href="https://example.com" title="Example Site">Click here</a>"#;
        let md = html_to_markdown(html).unwrap();
        // html2md may or may not preserve title attribute in markdown,
        // but at minimum the link text and URL must be present
        assert!(md.contains("[Click here]"));
        assert!(md.contains("(https://example.com)"));
    }

    #[test]
    fn test_html_to_markdown_multiple_images() {
        let html = r#"<p><img src="a.jpg" alt="A"/></p><p><img src="b.jpg" alt="B"/></p>"#;
        let md = html_to_markdown(html).unwrap();
        assert!(md.contains("![A]"));
        assert!(md.contains("(a.jpg)"));
        assert!(md.contains("![B]"));
        assert!(md.contains("(b.jpg)"));
    }

    // -----------------------------------------------------------------------
    // html_to_markdown_pipeline (full pipeline)
    // -----------------------------------------------------------------------

    #[test]
    fn test_html_to_markdown_pipeline_full() {
        let html = format!(
            r#"
        <html><body>
            <nav>Ads and nav</nav>
            <article>
                <h1>News Title</h1>
                <p>First paragraph with <strong>important</strong> info. {}</p>
                <p>Second paragraph with <a href="https://example.com">a link</a>. {}</p>
            </article>
            <footer>Copyright</footer>
        </body></html>
        "#,
            long_para("More detailed content that makes the article long enough."),
            long_para("Additional text to ensure we exceed the 200-character minimum threshold for extraction.")
        );
        let md = html_to_markdown_pipeline(&html).unwrap();
        // Article heading preserved
        assert!(md.contains("News Title"));
        // Content preserved
        assert!(md.contains("First paragraph"));
        assert!(md.contains("important"));
        assert!(md.contains("Second paragraph"));
        // Link converted
        assert!(md.contains("[a link]"));
        assert!(md.contains("(https://example.com)"));
        // Nav and footer stripped
        assert!(!md.contains("Ads and nav"));
        assert!(!md.contains("Copyright"));
    }

    #[test]
    fn test_html_to_markdown_pipeline_empty() {
        let result = html_to_markdown_pipeline("<html><body></body></html>");
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // clean_html_for_markdown
    // -----------------------------------------------------------------------

    #[test]
    fn test_clean_html_removes_nav() {
        let html = "<html><body><nav>Nav links</nav><article><p>Content</p></article></body></html>";
        let cleaned = clean_html_for_markdown(html);
        assert!(!cleaned.contains("Nav links"));
        assert!(cleaned.contains("Content"));
    }

    #[test]
    fn test_clean_html_removes_button_and_svg() {
        let html = r#"<button class="share">Share</button><svg><path d="M0 0"/></svg><p>Real content</p>"#;
        let cleaned = clean_html_for_markdown(html);
        assert!(!cleaned.contains("Share"));
        assert!(!cleaned.contains("svg"));
        assert!(cleaned.contains("Real content"));
    }

    #[test]
    fn test_clean_html_removes_footer_and_aside() {
        let html = "<aside>Sidebar</aside><main><p>Main</p></main><footer>Footer</footer>";
        let cleaned = clean_html_for_markdown(html);
        assert!(!cleaned.contains("Sidebar"));
        assert!(!cleaned.contains("Footer"));
        assert!(cleaned.contains("Main"));
    }

    // -----------------------------------------------------------------------
    // collapse_empty_lines
    // -----------------------------------------------------------------------

    #[test]
    fn test_collapse_empty_lines_single_newlines() {
        assert_eq!(collapse_empty_lines("line1\nline2"), "line1\nline2");
    }

    #[test]
    fn test_collapse_empty_lines_double_newlines() {
        assert_eq!(collapse_empty_lines("line1\n\nline2"), "line1\n\nline2");
    }

    #[test]
    fn test_collapse_empty_lines_triple_newlines() {
        assert_eq!(collapse_empty_lines("line1\n\n\nline2"), "line1\n\nline2");
    }

    #[test]
    fn test_collapse_empty_lines_many_newlines() {
        assert_eq!(collapse_empty_lines("line1\n\n\n\n\nline2"), "line1\n\nline2");
    }

    #[test]
    fn test_collapse_empty_lines_no_newlines() {
        assert_eq!(collapse_empty_lines("no newlines here"), "no newlines here");
    }

    #[test]
    fn test_collapse_empty_lines_mixed() {
        let input = "header\n\n\npara1\n\n\n\npara2\n\nfooter";
        let expected = "header\n\npara1\n\npara2\n\nfooter";
        assert_eq!(collapse_empty_lines(input), expected);
    }

    // -----------------------------------------------------------------------
    // clean_markdown
    // -----------------------------------------------------------------------

    #[test]
    fn test_clean_markdown_empty_link() {
        let md = "some text [](https://medium.com/m/signin?foo=bar) more";
        let cleaned = clean_markdown(md);
        assert_eq!(cleaned, "some text  more");
    }

    #[test]
    fn test_clean_markdown_non_content_link() {
        let md = r#"click [clap](/m/signin?actionUrl=...) here"#;
        let cleaned = clean_markdown(md);
        assert_eq!(cleaned, "click  here");
    }

    #[test]
    fn test_clean_markdown_preserves_normal_links() {
        let md = "read [article](https://example.com/article) ok";
        let cleaned = clean_markdown(md);
        assert_eq!(cleaned, "read [article](https://example.com/article) ok");
    }

    #[test]
    fn test_clean_markdown_empty_image() {
        let md = "text ![](https://example.com/img.png) more";
        let cleaned = clean_markdown(md);
        assert_eq!(cleaned, "text  more");
    }

    #[test]
    fn test_clean_markdown_preserves_normal_image() {
        let md = "text ![alt](https://example.com/img.png) more";
        let cleaned = clean_markdown(md);
        assert_eq!(cleaned, "text ![alt](https://example.com/img.png) more");
    }

    #[test]
    fn test_clean_markdown_vote_link() {
        let md = "stuff [](/m/vote/p/123) ok";
        let cleaned = clean_markdown(md);
        assert_eq!(cleaned, "stuff  ok");
    }

    #[test]
    fn test_clean_markdown_bookmark_link() {
        let md = "stuff [](/m/bookmark/p/123) ok";
        let cleaned = clean_markdown(md);
        assert_eq!(cleaned, "stuff  ok");
    }

    #[test]
    fn test_clean_markdown_no_changes() {
        let md = "just plain text\nwith [a link](https://example.com) and ![img](img.jpg)";
        let cleaned = clean_markdown(md);
        assert_eq!(cleaned, md);
    }

    #[test]
    fn test_clean_markdown_preserves_image_with_alt() {
        // Image with alt text should NOT be removed
        let md = "Look at ![photo](/img/photo.jpg) here";
        let cleaned = clean_markdown(md);
        assert_eq!(cleaned, "Look at ![photo](/img/photo.jpg) here");
    }
}

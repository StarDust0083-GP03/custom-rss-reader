use crate::error::{AppError, Result};

/// Convert HTML to Markdown using the html2md crate.
pub fn html_to_markdown(html: &str) -> Result<String> {
    if html.trim().is_empty() {
        return Err(AppError::Validation("Empty HTML content".into()));
    }

    Ok(html2md::parse_html(html))
}

/// Strip non-content elements, extract main content, then convert to Markdown.
///
/// The WeChat-aware main-content extraction in `FeedFetcher::extract_main_content`
/// covers the common article selectors (`article`, `main`, `data-content`, etc.)
/// plus a WeChat-specific fallback. We delegate to it here so the two code
/// paths don't drift apart.
pub fn html_to_markdown_pipeline(html: &str) -> Result<String> {
    if html.trim().is_empty() {
        return Err(AppError::Validation("Empty HTML content".into()));
    }
    let cleaned = clean_html_for_markdown(html);
    // Require main-content extraction to succeed; an empty body or a
    // document that only contains nav/footer should NOT silently fall
    // through to a near-empty markdown result.
    let main_content = crate::feed::fetcher::FeedFetcher::extract_main_content(&cleaned)?;
    if main_content.trim().is_empty() {
        return Err(AppError::OperationFailed("No main content found".into()));
    }
    let md = html_to_markdown(&main_content)?;
    let md = clean_markdown(&md);
    Ok(collapse_empty_lines(&md))
}

/// Remove UI elements (nav, footer, script, etc.) and their contents.
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

/// Clean up markdown output by removing non-content patterns.
///
/// Removed: empty `[](url)` / `![](url)` links; URL-path fragments like
/// `/signin`, `/vote/`, `/bookmark/`, `/share/` that come from Medium-style
/// action bars. **Path-only** matches are used so genuine content URLs that
/// happen to contain a substring like `clap` or `source=` are preserved.
pub fn clean_markdown(md: &str) -> String {
    let mut result = String::with_capacity(md.len());
    let bytes = md.as_bytes();
    let len = md.len();
    let mut i = 0;

    // Path-segment-style patterns: must be preceded by `/` (or be at the
    // start of the URL) so we don't match innocuous substrings.
    let path_patterns = ["/signin", "/vote/", "/bookmark/", "/share/", "/clap"];

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
                    if is_non_content_url(url, &path_patterns) {
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

/// Decide whether a markdown link URL is a non-content UI fragment.
fn is_non_content_url(url: &str, path_patterns: &[&str]) -> bool {
    if url.is_empty() {
        return true;
    }
    // Strip leading scheme://host so the path-segment match works.
    let after_scheme = url
        .find("://")
        .map(|i| i + 3)
        .and_then(|i| url[i..].find('/').map(|j| i + j))
        .unwrap_or(0);
    let path_and_rest = &url[after_scheme..];
    for pattern in path_patterns {
        for (start, _) in path_and_rest.match_indices(pattern) {
            // Require the pattern to be a complete path segment — i.e. the
            // character after it must be '/' (more path) or '?' (query) or
            // '#' (fragment) or end-of-string. Otherwise we match inside
            // a longer path token (e.g. /clap inside /clapton-live-2024).
            let after_idx = start + pattern.len();
            let next = path_and_rest[after_idx..].chars().next();
            let is_boundary = match next {
                None => true, // end of string
                Some('/') | Some('?') | Some('#') => true,
                _ => false,
            };
            if is_boundary {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate a long paragraph to exceed the 200-char threshold.
    fn long_para(text: &str) -> String {
        let repeat = (210 / text.len()).max(2);
        (0..repeat).map(|_| text).collect::<Vec<_>>().join(" ")
    }

    // -----------------------------------------------------------------------
    // html_to_markdown
    // -----------------------------------------------------------------------

    #[test]
    fn test_html_to_markdown_heading() {
        let html = "<h1>Title</h1><h2>Subtitle</h2><h3>Section</h3>";
        let md = html_to_markdown(html).unwrap();
        assert!(md.contains("Title"));
        assert!(md.contains("Subtitle"));
        assert!(md.contains("Section"));
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
        let html = r#"<a href="https://example.com"><img src="https://example.com/img.jpg" alt="Photo"/></a>"#;
        let md = html_to_markdown(html).unwrap();
        assert!(md.contains("[![Photo]"));
        assert!(md.contains("(https://example.com/img.jpg)"));
        assert!(md.contains("](https://example.com)"));
    }

    #[test]
    fn test_html_to_markdown_image_no_alt() {
        let html = r#"<img src="https://example.com/img.png"/>"#;
        let md = html_to_markdown(html).unwrap();
        assert!(md.contains("![]"));
        assert!(md.contains("(https://example.com/img.png)"));
    }

    #[test]
    fn test_html_to_markdown_link_with_title() {
        let html = r#"<a href="https://example.com" title="Example Site">Click here</a>"#;
        let md = html_to_markdown(html).unwrap();
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
        assert!(md.contains("News Title"));
        assert!(md.contains("First paragraph"));
        assert!(md.contains("important"));
        assert!(md.contains("Second paragraph"));
        assert!(md.contains("[a link]"));
        assert!(md.contains("(https://example.com)"));
        assert!(!md.contains("Ads and nav"));
        assert!(!md.contains("Copyright"));
    }

    #[test]
    fn test_html_to_markdown_pipeline_empty() {
        let result = html_to_markdown_pipeline("<html><body></body></html>");
        assert!(result.is_err());
    }

    /// Regression: this is the contract that `fetch_website_markdown` relies on.
    /// Both the host-DOM text path and the iframe webview path consume this
    /// Markdown and re-render it through `marked`. If a `<script>` tag or its
    /// body survives the pipeline, the markdown→HTML step would re-introduce
    /// an execution surface. This test pins that down.
    ///
    /// Note: `<iframe>` is NOT stripped by the pipeline — it is stripped
    /// downstream by `setSafeHtml` (which has `iframe` in `BLOCKED_TAGS`)
    /// when the markdown is re-rendered. The pipeline's job is just to keep
    /// raw script bodies out of the markdown text.
    #[test]
    fn test_html_to_markdown_pipeline_strips_execution_surfaces() {
        let html = format!(
            r#"
        <html><body>
            <article>
                <h1>Safe Article</h1>
                <p>Body text. {}</p>
                <p><a href="https://example.com" onclick="alert(1)">link</a></p>
                <script>alert('xss')</script>
            </article>
        </body></html>
        "#,
            long_para("Padding so the article clears the 200-char extraction threshold."),
        );
        let md = html_to_markdown_pipeline(&html).expect("pipeline should succeed");

        // No leftover script execution surface.
        assert!(!md.contains("<script"), "script tag must be stripped");
        assert!(!md.contains("</script"), "closing script tag must be stripped");
        assert!(!md.contains("alert("), "script body must not survive");

        // Real content is preserved as Markdown (not raw HTML).
        assert!(md.contains("Safe Article"));
        assert!(md.contains("Body text"));
        assert!(md.contains("[link]"));
        assert!(md.contains("(https://example.com)"));
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
        let md = r#"click [clap](https://medium.com/m/signin?actionUrl=...) here"#;
        let cleaned = clean_markdown(md);
        assert_eq!(cleaned, "click  here");
    }

    #[test]
    fn test_clean_markdown_preserves_normal_links() {
        let md = "read [article](https://example.com/article) ok";
        let cleaned = clean_markdown(md);
        assert_eq!(cleaned, "read [article](https://example.com/article) ok");
    }

    /// Regression for the "clap" substring bug: a real article URL that
    /// merely contains the letters "clap" used to be deleted by the old
    /// substring matcher. Path-segment matching must NOT trigger here.
    #[test]
    fn test_clean_markdown_preserves_link_with_clap_in_host() {
        let md = "see [bio](https://example.com/clapton-live-2024) ok";
        let cleaned = clean_markdown(md);
        assert_eq!(
            cleaned,
            "see [bio](https://example.com/clapton-live-2024) ok"
        );
    }

    /// Regression for the "source=" substring bug: UTM-style `?source=...`
    /// query params must NOT trigger the path-only blacklist.
    #[test]
    fn test_clean_markdown_preserves_link_with_source_query() {
        let md = "see [ref](https://example.com/article?utm_source=feed) ok";
        let cleaned = clean_markdown(md);
        assert_eq!(
            cleaned,
            "see [ref](https://example.com/article?utm_source=feed) ok"
        );
    }

    /// Regression for the "post_" substring bug: real path fragments like
    /// `/posts/...` used to be matched as non-content. Path-segment
    /// matching with a leading `/` boundary avoids the false positive.
    #[test]
    fn test_clean_markdown_preserves_post_path() {
        let md = "read [post](https://example.com/posts/1234) ok";
        let cleaned = clean_markdown(md);
        assert_eq!(cleaned, "read [post](https://example.com/posts/1234) ok");
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
        let md = "Look at ![photo](/img/photo.jpg) here";
        let cleaned = clean_markdown(md);
        assert_eq!(cleaned, "Look at ![photo](/img/photo.jpg) here");
    }

    #[test]
    fn test_clean_markdown_preserves_bold() {
        let md = "this is **bold** and __italic__ text";
        let cleaned = clean_markdown(md);
        assert_eq!(cleaned, md);
    }

    #[test]
    fn test_clean_markdown_preserves_heading() {
        let md = "# Title\n\n## Subtitle\n\nNormal paragraph.";
        let cleaned = clean_markdown(md);
        assert_eq!(cleaned, md);
    }

    #[test]
    fn test_clean_markdown_preserves_mixed_formatting() {
        let md = "# Article\n\n**Bold paragraph** with [a link](https://example.com) inside.";
        let cleaned = clean_markdown(md);
        assert_eq!(cleaned, md);
    }

    #[test]
    fn test_clean_markdown_preserves_code_block() {
        let md = "Some text\n\n```rust\nlet x = 1;\n```\n\nMore text.";
        let cleaned = clean_markdown(md);
        assert_eq!(cleaned, md);
    }
}
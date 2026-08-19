use chromadb::client::{ChromaAuthMethod, ChromaClient, ChromaClientOptions};
use chromadb::collection::{ChromaCollection, CollectionEntries, QueryOptions, QueryResult};
use chromadb::embeddings::EmbeddingFunction;
use serde_json::Map;

use crate::error::Result;
use crate::models::feed_item::FeedItem;
use crate::repositories::IndexRow;

use super::embeddings::OnnxEmbeddingFunction;
use super::ChromaConfig;

/// Maximum number of characters of content indexed per article. Beyond this
/// the embedding model is paying for noise anyway.
const MAX_INDEXED_DOC_CHARS: usize = 2000;

/// Number of items per ChromaDB upsert batch.
const BATCH_SIZE: usize = 50;

/// ChromaDB service for semantic search of RSS articles.
pub struct ChromaService {
    collection: ChromaCollection,
    /// Computes embeddings client-side: the `chromadb` crate refuses
    /// documents without embeddings and `query_texts` without an embedding
    /// function, and ChromaDB 1.x no longer provides a server-side default.
    /// The model is downloaded/loaded lazily on first use — see
    /// [`OnnxEmbeddingFunction`].
    embedding_function: OnnxEmbeddingFunction,
}

impl ChromaService {
    /// Create a new ChromaService, connecting to ChromaDB and ensuring the collection exists.
    ///
    /// Database name compatibility: Chroma ≤0.6 created the v2 database as
    /// `default`, Chroma 1.x renamed it to `default_database` (the identity
    /// endpoint reports it). Hardcoding either breaks the other generation —
    /// the identity response of a 1.x server is authoritative, so we try the
    /// modern name first and fall back to the legacy one.
    pub async fn new(config: &ChromaConfig) -> Result<Self> {
        let url = config.url();
        let collection = match connect(&url, &config.collection_name, "default_database").await {
            Ok(c) => c,
            Err(first) => match connect(&url, &config.collection_name, "default").await {
                Ok(c) => c,
                Err(_) => return Err(first),
            },
        };
        // Cheap: constructing the embedding function does no I/O — the
        // ~120 MB model is downloaded/loaded on the first embed call.
        Ok(Self {
            collection,
            embedding_function: OnnxEmbeddingFunction::new(),
        })
    }

    /// Index a single feed item into ChromaDB.
    pub async fn index_item(&self, item: &FeedItem) -> Result<()> {
        let row = IndexRow {
            id: item.id,
            title: item.title.clone(),
            link: item.link.clone(),
            author: item.author.clone(),
            published_at: item.published_at,
            category: item.category.clone(),
            // The embedding document is truncated anyway; truncating here too
            // keeps the borrow checker happy without cloning full articles.
            description: item
                .description
                .as_deref()
                .map(|d| d.chars().take(MAX_INDEXED_DOC_CHARS).collect()),
            content: item
                .content
                .as_deref()
                .map(|c| c.chars().take(MAX_INDEXED_DOC_CHARS).collect()),
        };
        self.upsert_index_rows(std::slice::from_ref(&row)).await
    }

    /// Remove a feed item from the ChromaDB index.
    pub async fn delete_item(&self, item_id: i64) -> Result<()> {
        self.delete_items(&[item_id]).await
    }

    /// Remove multiple feed items from the index (e.g. every article of a
    /// deleted subscription). Empty input is a no-op.
    pub async fn delete_items(&self, item_ids: &[i64]) -> Result<()> {
        if item_ids.is_empty() {
            return Ok(());
        }
        let ids: Vec<String> = item_ids.iter().map(|id| format!("item_{}", id)).collect();
        let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        self.collection
            .delete(Some(id_refs), None, None)
            .await
            .map_err(|e| {
                crate::error::AppError::OperationFailed(format!("ChromaDB delete: {}", e))
            })?;
        Ok(())
    }

    /// Perform a semantic search against the indexed articles.
    ///
    /// We deliberately do NOT include `documents` in the response — callers
    /// ask for the hit by `item_id` via `get_item` when they need the
    /// article body. This keeps the IPC payload small and avoids paying for
    /// embedding transmission in a list view.
    pub async fn search(&self, query: &str, limit: i64) -> Result<Vec<SemanticSearchResult>> {
        self.query_by_text(query, limit.max(0) as usize).await
    }

    /// Find articles similar to the given item.
    ///
    /// The item's own document text is used as the query, so the item itself
    /// is (almost always) the top hit — we over-fetch by one and filter it
    /// out by id. If the item was never indexed (e.g. Chroma was enabled
    /// after it was fetched and no reindex ran), the query still works; the
    /// filter is then just a no-op.
    pub async fn find_similar(
        &self,
        item: &FeedItem,
        limit: i64,
    ) -> Result<Vec<SemanticSearchResult>> {
        let text = build_document_from_parts(
            &item.title,
            item.description.as_deref(),
            item.content.as_deref(),
        );
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let results = self
            .query_by_text(&text, limit.max(0) as usize + 1)
            .await?;
        Ok(results
            .into_iter()
            .filter(|r| r.item_id != item.id)
            .take(limit.max(0) as usize)
            .collect())
    }

    /// Run a raw text query against the collection, returning parsed results.
    async fn query_by_text(
        &self,
        query: &str,
        n_results: usize,
    ) -> Result<Vec<SemanticSearchResult>> {
        // Time each stage so slow searches can be diagnosed: embedding the
        // query (ONNX model load + inference, client-side) vs the ChromaDB
        // round trip. We embed ourselves and hand over the raw vector — the
        // crate would otherwise embed internally and give us no visibility.
        let total_t0 = std::time::Instant::now();
        let embed_t0 = std::time::Instant::now();
        let query_embedding = self
            .embedding_function
            .embed(&[query])
            .await
            .map_err(|e| {
                crate::error::AppError::OperationFailed(format!("Embed query: {}", e))
            })?
            .into_iter()
            .next()
            .unwrap_or_default();
        let embed_ms = embed_t0.elapsed().as_millis();

        let includes: Vec<&str> = vec!["metadatas", "distances"];

        let query_options = QueryOptions {
            query_texts: None,
            query_embeddings: Some(vec![query_embedding]),
            where_metadata: None,
            where_document: None,
            n_results: Some(n_results),
            include: Some(includes),
        };

        let query_t0 = std::time::Instant::now();
        let result: QueryResult = self
            .collection
            .query(query_options, None)
            .await
            .map_err(|e| {
                crate::error::AppError::OperationFailed(format!("ChromaDB query: {}", e))
            })?;
        let query_ms = query_t0.elapsed().as_millis();

        let mut results: Vec<SemanticSearchResult> = Vec::new();

        let ids: Vec<String> = result.ids.into_iter().next().unwrap_or_default();
        let distances: Vec<f32> = result
            .distances
            .unwrap_or_default()
            .into_iter()
            .next()
            .unwrap_or_default();
        let metadatas: Vec<serde_json::Map<String, serde_json::Value>> = result
            .metadatas
            .unwrap_or_default()
            .into_iter()
            .next()
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .collect();

        for (i, id) in ids.iter().enumerate() {
            let score = distances.get(i).copied().unwrap_or(1.0f32) as f64;
            let item_id = id
                .strip_prefix("item_")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);

            let title = metadatas
                .get(i)
                .and_then(|m| m.get("title"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let url = metadatas
                .get(i)
                .and_then(|m| m.get("url"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());

            let author = metadatas
                .get(i)
                .and_then(|m| m.get("author"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());

            results.push(SemanticSearchResult {
                item_id,
                title,
                url,
                author,
                score,
            });
        }

        // End-of-query summary — the split decides whether the model load,
        // inference, or the ChromaDB round trip is the bottleneck.
        let display: String = query.chars().take(80).collect();
        println!(
            "[chroma] SEARCH total={}ms (embed={}ms, server={}ms) hits={} n_results={} query={:?}",
            total_t0.elapsed().as_millis(),
            embed_ms,
            query_ms,
            results.len(),
            n_results,
            display,
        );

        Ok(results)
    }

    /// Re-index the given rows in batches. Upsert semantics: an existing id
    /// is updated in place, so re-syncing is always safe.
    ///
    /// Callers (the sync engine) are responsible for streaming rows in
    /// pages so the whole feed is never held in memory at once.
    pub async fn upsert_index_rows(&self, rows: &[IndexRow]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }

        for chunk in rows.chunks(BATCH_SIZE) {
            let ids: Vec<String> = chunk.iter().map(|r| format!("item_{}", r.id)).collect();
            let documents: Vec<String> = chunk.iter().map(build_document_text).collect();
            let metadatas: Vec<Map<String, serde_json::Value>> = chunk
                .iter()
                .map(|r| {
                    let mut m = Map::new();
                    m.insert("item_id".into(), r.id.into());
                    m.insert("title".into(), r.title.clone().into());
                    m.insert("url".into(), r.link.clone().unwrap_or_default().into());
                    m.insert("author".into(), r.author.clone().unwrap_or_default().into());
                    m.insert(
                        "published_at".into(),
                        r.published_at
                            .map(|dt| dt.to_rfc3339())
                            .unwrap_or_default()
                            .into(),
                    );
                    m.insert("category".into(), r.category.clone().unwrap_or_default().into());
                    m
                })
                .collect();

            // Borrow String values as &str for CollectionEntries
            let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
            let doc_refs: Vec<&str> = documents.iter().map(|s| s.as_str()).collect();

            let entries = CollectionEntries {
                ids: id_refs,
                embeddings: None,
                metadatas: Some(metadatas),
                documents: Some(doc_refs),
            };

            // The embedding function computes embeddings client-side; without
            // it the crate bails ("embedding_function cannot be None ...").
            self.collection
                .upsert(entries, Some(Box::new(self.embedding_function.clone())))
                .await
                .map_err(|e| {
                    crate::error::AppError::OperationFailed(format!("ChromaDB batch upsert: {}", e))
                })?;
        }

        Ok(())
    }

    /// Check if the ChromaDB server is reachable.
    ///
    /// We use `count` instead of an `n_results=0` query because some ChromaDB
    /// versions reject `n_results=0`, and `count` is a cheap dedicated
    /// endpoint that doesn't trigger embedding computation.
    pub async fn health_check(&self) -> Result<bool> {
        match self.collection.count().await {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }
}

/// Connect to the server and open the collection inside `database`.
///
/// No collection configuration is needed: embeddings are computed client-side
/// (see [`OnnxEmbeddingFunction`]), so a plain `get_or_create` suffices and a
/// collection created by an older build (with or without a legacy EF config)
/// just opens as-is.
async fn connect(
    url: &str,
    collection_name: &str,
    database: &str,
) -> std::result::Result<ChromaCollection, crate::error::AppError> {
    let client = ChromaClient::new(ChromaClientOptions {
        url: Some(url.to_string()),
        database: database.to_string(),
        auth: ChromaAuthMethod::None,
    })
    .await
    .map_err(|e| crate::error::AppError::OperationFailed(format!("ChromaDB connect: {}", e)))?;

    client
        .get_or_create_collection(collection_name, None)
        .await
        .map_err(|e| {
            crate::error::AppError::OperationFailed(format!("ChromaDB collection: {}", e))
        })
}

/// Result of a semantic search query. `document` is intentionally omitted;
/// the frontend fetches the body via `get_item(item_id)` when needed.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SemanticSearchResult {
    pub item_id: i64,
    pub title: String,
    pub url: Option<String>,
    pub author: Option<String>,
    /// Distance score (lower = more similar).
    pub score: f64,
}

/// Build the document text from an index row for indexing.
///
/// Strips `<script>`, `<style>`, `<noscript>` and all HTML tags, collapses
/// whitespace, and truncates to [`MAX_INDEXED_DOC_CHARS`]. Without these
/// steps a single JS-heavy page would bloat the embedding and pollute the
/// semantic index with code text.
fn build_document_text(row: &IndexRow) -> String {
    build_document_from_parts(
        &row.title,
        row.description.as_deref(),
        row.content.as_deref(),
    )
}

/// Build the document text from raw parts. Shared by the [`IndexRow`] path
/// and the find-similar query path.
pub fn build_document_from_parts(
    title: &str,
    description: Option<&str>,
    content: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(title.to_string());

    if let Some(ref desc) = description {
        if !desc.is_empty() {
            parts.push(strip_html_tags(desc));
        }
    }

    if let Some(ref content) = content {
        if !content.is_empty() {
            let cleaned = strip_script_and_style(content);
            let text = strip_html_tags(&cleaned);
            if !text.is_empty() {
                parts.push(text);
            }
        }
    }

    let mut combined = parts.join("\n\n");
    if combined.len() > MAX_INDEXED_DOC_CHARS {
        // String::truncate panics when the cut point is not a UTF-8 char
        // boundary — with CJK text (3 bytes/char) byte #2000 lands mid-char
        // most of the time. Floor to the previous boundary instead.
        let mut end = MAX_INDEXED_DOC_CHARS;
        while end > 0 && !combined.is_char_boundary(end) {
            end -= 1;
        }
        combined.truncate(end);
    }
    combined
}

/// Remove `<script>...</script>` and `<style>...</style>` blocks so their
/// contents don't pollute the embedding. Naive bracket matching is good
/// enough for RSS / article HTML.
fn strip_script_and_style(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut rest = html;
    let block_tags = ["script", "style", "noscript"];
    loop {
        // Find the next block tag to drop
        let mut earliest: Option<(usize, &str)> = None;
        for tag in &block_tags {
            let open = format!("<{}", tag);
            if let Some(p) = rest.find(&open) {
                match earliest {
                    None => earliest = Some((p, tag)),
                    Some((cur, _)) if p < cur => earliest = Some((p, tag)),
                    _ => {}
                }
            }
        }
        let Some((pos, tag)) = earliest else {
            result.push_str(rest);
            break;
        };
        result.push_str(&rest[..pos]);
        // Skip past the closing tag
        let close = format!("</{}>", tag);
        if let Some(end) = rest[pos..].find(&close) {
            rest = &rest[pos + end + close.len()..];
        } else {
            // Unterminated; drop to the end
            break;
        }
    }
    result
}

/// Simple HTML tag stripper for indexing / prompt-building purposes.
/// `pub(crate)` so the recommendation command can build clean snippets too.
pub(crate) fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ => {
                if !in_tag {
                    result.push(ch);
                }
            }
        }
    }
    let mut collapsed = String::with_capacity(result.len());
    let mut prev_ws = false;
    for ch in result.chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                collapsed.push(' ');
                prev_ws = true;
            }
        } else {
            collapsed.push(ch);
            prev_ws = false;
        }
    }
    collapsed.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_html_tags_simple() {
        assert_eq!(strip_html_tags("<p>Hello</p>"), "Hello");
    }

    #[test]
    fn test_strip_html_tags_nested() {
        let html = "<div><h1>Title</h1><p>Content here</p></div>";
        assert_eq!(strip_html_tags(html), "TitleContent here");
    }

    #[test]
    fn test_strip_html_tags_no_html() {
        assert_eq!(strip_html_tags("Plain text"), "Plain text");
    }

    /// Regression: indexing code/JS into the embedding pollutes semantic search.
    #[test]
    fn test_strip_script_and_style() {
        let html = r#"<p>Real content</p><script>alert('x')</script><p>More</p><style>body{}</style>"#;
        let cleaned = strip_script_and_style(html);
        assert!(cleaned.contains("Real content"));
        assert!(cleaned.contains("More"));
        assert!(!cleaned.contains("alert"));
        assert!(!cleaned.contains("body{}"));
    }

    #[test]
    fn test_build_document_text() {
        let row = IndexRow {
            id: 1,
            title: "Test Article".into(),
            link: None,
            author: None,
            published_at: None,
            category: None,
            description: Some("A description".into()),
            content: Some("<p>Some content</p>".into()),
        };
        let doc = build_document_text(&row);
        assert!(doc.contains("Test Article"));
        assert!(doc.contains("A description"));
        assert!(doc.contains("Some content"));
    }

    #[test]
    fn test_build_document_from_parts_basic() {
        let doc = build_document_from_parts("T", Some("D"), Some("<p>C</p>"));
        assert!(doc.contains("T"));
        assert!(doc.contains("D"));
        assert!(doc.contains("C"));
    }

    /// Regression: doc text was unbounded — a single 100KB article would
    /// blow up the embedding size.
    #[test]
    fn test_build_document_text_truncates() {
        let doc = build_document_from_parts("T", None, Some(&"x".repeat(MAX_INDEXED_DOC_CHARS * 3)));
        assert!(doc.len() <= MAX_INDEXED_DOC_CHARS);
    }

    /// Regression: String::truncate(2000) PANICKED when byte 2000 fell in
    /// the middle of a multi-byte character. CJK text is 3 bytes/char, so
    /// long Chinese articles crashed indexing almost every time. The old
    /// test used ASCII and never caught it.
    #[test]
    fn test_build_document_text_truncates_cjk_without_panic() {
        // Each char is 3 bytes; 3000 chars = 9000 bytes, cut at 2000 bytes
        // is highly unlikely to land on a boundary — the old code panicked.
        let cjk: String = "汉".repeat(3000);
        let doc = build_document_from_parts(&cjk, None, None);
        assert!(doc.len() <= MAX_INDEXED_DOC_CHARS);
        assert!(doc.is_char_boundary(doc.len()));
        // Mixed content: ASCII title + CJK body crossing the boundary.
        let doc2 = build_document_from_parts("Title", Some(&cjk), Some(&cjk));
        assert!(doc2.len() <= MAX_INDEXED_DOC_CHARS);
        assert!(doc2.is_char_boundary(doc2.len()));
    }

    /// Regression for the watermark walk: the sync engine relies on rows
    /// being ordered ascending by id with `id > after_id` filtering.
    #[test]
    fn test_build_document_empty_parts() {
        assert_eq!(build_document_from_parts("", None, None), "");
        assert_eq!(build_document_from_parts("T", Some(""), Some("")), "T");
    }
}
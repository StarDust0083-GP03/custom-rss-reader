use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use sqlx::SqlitePool;

use super::FeedItemRepository;
use super::IndexRow;
use crate::error::{AppError, Result};
use crate::models::{
    tag::{decode_embedding, encode_embedding, normalize_tag, MAX_TAGS_PER_ITEM},
    FeedItem, FeedItemSummary, NewFeedItem, NewJob,
};

use super::TagCatalogEntry;
use super::TagOverviewCoverage;
use super::TopicAssignment;
use super::TopicCategory;

/// Character cap applied to `description`/`content` in the [`IndexRow`]
/// projection queries. Generously above what the embedding-document builder
/// can consume (it truncates the joined document anyway).
const INDEX_TEXT_CHARS_SQL: i64 = 2000;

/// Columns selected for summary (list-view) queries. Excludes the large text
/// columns (`content`, `content_md`, `translated_content`, `guid`).
///
/// NOTE: columns are prefixed with `f.` — every summary query joins
/// `subscriptions s` to carry the source title/url (issue #3).
const SUMMARY_COLS: &str = "f.id, f.subscription_id, f.title, f.link, f.description, f.author, \
    f.published_at, f.fetched_at, f.is_website_content, f.is_read, f.is_favorite, f.is_read_later, \
    f.is_ignored, f.tags, f.translated_title, \
    (f.translated_content IS NOT NULL AND f.translated_content != '') AS has_translation, \
    s.title AS source_title, s.url AS source_url";

/// FROM clause shared by every summary query (see SUMMARY_COLS).
const SUMMARY_FROM: &str = "feed_items f LEFT JOIN subscriptions s ON s.id = f.subscription_id";

/// Private database row type mapping to the `feed_items` table.
#[derive(sqlx::FromRow)]
struct FeedItemRow {
    pub id: i64,
    pub subscription_id: i64,
    pub guid: Option<String>,
    pub title: String,
    pub link: Option<String>,
    pub content: Option<String>,
    pub content_md: Option<String>,
    pub description: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
    pub is_website_content: bool,
    pub is_read: bool,
    pub is_favorite: bool,
    pub is_read_later: bool,
    pub is_ignored: bool,
    pub tags: Option<String>,
    pub translated_title: Option<String>,
    pub translated_content: Option<String>,
    pub translated_at: Option<chrono::DateTime<chrono::Utc>>,
    pub translated_source_hash: Option<String>,
    pub translated_model: Option<String>,
    pub translated_prompt_version: Option<i64>,
}

/// Row type for summary queries (projection of `feed_items`).
#[derive(sqlx::FromRow)]
struct FeedItemSummaryRow {
    pub id: i64,
    pub subscription_id: i64,
    pub title: String,
    pub link: Option<String>,
    pub description: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
    pub is_website_content: bool,
    pub is_read: bool,
    pub is_favorite: bool,
    pub is_read_later: bool,
    pub is_ignored: bool,
    pub tags: Option<String>,
    pub translated_title: Option<String>,
    pub has_translation: bool,
    pub source_title: Option<String>,
    pub source_url: Option<String>,
}

impl From<FeedItemRow> for FeedItem {
    fn from(r: FeedItemRow) -> Self {
        FeedItem {
            id: r.id,
            subscription_id: r.subscription_id,
            guid: r.guid,
            title: r.title,
            link: r.link,
            content: r.content,
            content_md: r.content_md,
            description: r.description,
            author: r.author,
            published_at: r.published_at,
            fetched_at: r.fetched_at,
            is_website_content: r.is_website_content,
            is_read: r.is_read,
            is_favorite: r.is_favorite,
            is_read_later: r.is_read_later,
            is_ignored: r.is_ignored,
            tags: r.tags,
            translated_title: r.translated_title,
            translated_content: r.translated_content,
            translated_at: r.translated_at,
            translated_source_hash: r.translated_source_hash,
            translated_model: r.translated_model,
            translated_prompt_version: r.translated_prompt_version,
        }
    }
}

/// Private row type for the [`IndexRow`] projection queries.
#[derive(sqlx::FromRow)]
struct IndexRowImpl {
    pub id: i64,
    pub title: String,
    pub link: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
    pub description: Option<String>,
    pub content: Option<String>,
}

impl From<IndexRowImpl> for IndexRow {
    fn from(r: IndexRowImpl) -> Self {
        IndexRow {
            id: r.id,
            title: r.title,
            link: r.link,
            author: r.author,
            published_at: r.published_at,
            description: r.description,
            content: r.content,
        }
    }
}

impl From<FeedItemSummaryRow> for FeedItemSummary {
    fn from(r: FeedItemSummaryRow) -> Self {
        FeedItemSummary {
            id: r.id,
            subscription_id: r.subscription_id,
            title: r.title,
            link: r.link,
            description: r.description,
            author: r.author,
            published_at: r.published_at,
            fetched_at: r.fetched_at,
            is_website_content: r.is_website_content,
            is_read: r.is_read,
            is_favorite: r.is_favorite,
            is_read_later: r.is_read_later,
            is_ignored: r.is_ignored,
            tags: r.tags,
            translated_title: r.translated_title,
            has_translation: r.has_translation,
            source_title: r.source_title,
            source_url: r.source_url,
        }
    }
}

/// Escape LIKE special characters so user input is matched literally.
/// Use together with `ESCAPE '\'` in the SQL.
fn escape_like(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn required_tag(input: &str) -> Result<String> {
    normalize_tag(input).ok_or_else(|| {
        AppError::Validation(
            "Tag names must contain 1-64 ASCII letters or numbers, separated by underscores".into(),
        )
    })
}

async fn load_tag_maps(
    conn: &mut sqlx::SqliteConnection,
) -> Result<(HashMap<String, String>, HashSet<String>)> {
    let alias_rows: Vec<(String, String)> =
        sqlx::query_as("SELECT alias, canonical_name FROM tag_aliases")
            .fetch_all(&mut *conn)
            .await?;
    let aliases = alias_rows.into_iter().collect();
    let blocked: HashSet<String> = sqlx::query_as::<_, (String,)>("SELECT name FROM blocked_tags")
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .map(|(name,)| name)
        .collect();
    Ok((aliases, blocked))
}

fn resolve_tag(mut tag: String, aliases: &HashMap<String, String>) -> String {
    for _ in 0..16 {
        let Some(next) = aliases.get(&tag) else { break };
        if next == &tag {
            break;
        }
        tag = next.clone();
    }
    tag
}

/// SQLite implementation of the feed-item and tag vocabulary repository.
pub struct SqliteFeedItemRepository {
    pub(crate) pool: SqlitePool,
}


/// Rewrite the derived display cache after a vocabulary rename/merge/delete.
/// Raw names are kept as the source of truth; replacements are applied to the
/// raw record for an explicit rename/merge, while blocked names are omitted
/// only from the display cache.
async fn rewrite_feed_item_tags(
    conn: &mut sqlx::SqliteConnection,
    replacements: &HashMap<String, String>,
    removed: &HashSet<String>,
) -> Result<()> {
    let (aliases, blocked) = load_tag_maps(conn).await?;
    let rows: Vec<(i64, Option<String>)> = sqlx::query_as(
        "SELECT id, raw_tags FROM feed_items WHERE raw_tags IS NOT NULL AND json_valid(raw_tags)",
    )
    .fetch_all(&mut *conn)
    .await?;
    for (id, raw) in rows {
        let Some(raw) = raw else { continue };
        let Ok(names) = serde_json::from_str::<Vec<String>>(&raw) else { continue };
        let mut raw_out = Vec::new();
        let mut display = Vec::new();
        for item in names {
            let Some(normalized) = normalize_tag(&item) else { continue };
            // Raw tags are the classifier's answer and must never be rewritten
            // by an administrative synonym/delete operation.
            if !raw_out.contains(&normalized) { raw_out.push(normalized.clone()); }
            if removed.contains(&normalized) { continue; }
            let rewritten = replacements.get(&normalized).cloned().unwrap_or(normalized);
            let shown = resolve_tag(rewritten, &aliases);
            if !blocked.contains(&shown) && !display.contains(&shown) {
                display.push(shown);
                if display.len() == MAX_TAGS_PER_ITEM { break; }
            }
        }
        sqlx::query("UPDATE feed_items SET raw_tags = $2, tags = $3 WHERE id = $1")
            .bind(id)
            .bind(serde_json::to_string(&raw_out).map_err(|e| AppError::Internal(e.to_string()))?)
            .bind(serde_json::to_string(&display).map_err(|e| AppError::Internal(e.to_string()))?)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

impl SqliteFeedItemRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Run a summary query with the given WHERE suffix and bound params.
    async fn fetch_summaries(
        &self,
        where_sql: &str,
        subscription_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FeedItemSummary>> {
        let sql = format!(
            "SELECT {} FROM {} {} ORDER BY f.published_at DESC LIMIT $1 OFFSET $2",
            SUMMARY_COLS, SUMMARY_FROM, where_sql
        );
        let mut q = sqlx::query_as::<_, FeedItemSummaryRow>(&sql)
            .bind(limit)
            .bind(offset);
        if let Some(sub_id) = subscription_id {
            // subscription_id is referenced as $3 in the WHERE clause
            q = q.bind(sub_id);
        }
        let rows = q.fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|r| r.into()).collect())
    }
}

#[async_trait]
impl FeedItemRepository for SqliteFeedItemRepository {
    async fn create(&self, input: NewFeedItem) -> Result<FeedItem> {
        self.create_with_jobs(input, Vec::new()).await
    }

    async fn create_with_jobs(&self, input: NewFeedItem, jobs: Vec<NewJob>) -> Result<FeedItem> {
        // ON CONFLICT DO NOTHING relies on the (subscription_id, guid) unique
        // index as a last-resort dedup guard; in-memory dedup during fetch is
        // the primary mechanism. A conflicting insert returns no row, which we
        // surface as a Duplicate error that callers treat as "already exists".
        //
        // The article and its enrichment jobs share one transaction: either
        // both are durable, or neither is. A job can therefore never reference
        // an article that was rolled back, and an article can never be
        // committed with its enrichment silently forgotten.
        let mut tx = self.pool.begin().await.map_err(AppError::Database)?;

        let row = sqlx::query_as::<_, FeedItemRow>(
            r#"
            INSERT INTO feed_items (
                subscription_id, guid, title, link, content, content_md,
                description, author, published_at,
                is_website_content, is_read, is_favorite, is_read_later, is_ignored,
                tags,
                translated_title, translated_content, translated_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)
            ON CONFLICT DO NOTHING
            RETURNING *
            "#,
        )
        .bind(input.subscription_id)
        .bind(&input.guid)
        .bind(&input.title)
        .bind(&input.link)
        .bind(&input.content)
        .bind(&input.content_md)
        .bind(&input.description)
        .bind(&input.author)
        .bind(input.published_at)
        .bind(input.is_website_content)
        .bind(input.is_read)
        .bind(input.is_favorite)
        .bind(input.is_read_later)
        .bind(input.is_ignored)
        .bind(&input.tags)
        .bind(&input.translated_title)
        .bind(&input.translated_content)
        .bind(input.translated_at)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_feed_item_sqlx_error(e, "creating feed item"))?
        .ok_or_else(|| {
            AppError::Duplicate(format!(
                "feed item already exists (subscription {}, guid {:?})",
                input.subscription_id, input.guid
            ))
        })?;

        if !jobs.is_empty() {
            let item_id = row.id;
            for job in &jobs {
                sqlx::query(
                    r#"
                    INSERT INTO jobs (kind, item_id, payload, state, priority, max_attempts)
                    VALUES ($1, $2, $3, 'queued', $4, $5)
                    "#,
                )
                .bind(job.kind.as_str())
                // `None` (or a stale 0 placeholder) means the article row this
                // job was committed with.
                .bind(
                    job.item_id
                        .filter(|id| *id > 0)
                        .unwrap_or(item_id),
                )
                .bind(&job.payload)
                .bind(job.kind.priority())
                .bind(job.kind.max_attempts())
                .execute(&mut *tx)
                .await
                .map_err(AppError::Database)?;
            }
        }

        tx.commit().await.map_err(AppError::Database)?;
        Ok(row.into())
    }

    async fn find_by_id(&self, id: i64) -> Result<FeedItem> {
        let row = sqlx::query_as::<_, FeedItemRow>("SELECT * FROM feed_items WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("FeedItem with id {} not found", id)))?;

        Ok(row.into())
    }

    async fn find_dedup_keys(
        &self,
        subscription_id: i64,
    ) -> Result<(HashSet<String>, HashSet<String>)> {
        let rows: Vec<(Option<String>, Option<String>)> =
            sqlx::query_as("SELECT guid, link FROM feed_items WHERE subscription_id = $1")
                .bind(subscription_id)
                .fetch_all(&self.pool)
                .await?;

        let guids = rows.iter().filter_map(|r| r.0.clone()).collect();
        let links = rows.iter().filter_map(|r| r.1.clone()).collect();
        Ok((guids, links))
    }

    async fn find_ids_by_subscription(&self, subscription_id: i64) -> Result<Vec<i64>> {
        let rows: Vec<(i64,)> =
            sqlx::query_as("SELECT id FROM feed_items WHERE subscription_id = $1")
                .bind(subscription_id)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(|r| r.0).collect())
    }

    async fn find_index_page(&self, after_id: i64, limit: i64) -> Result<Vec<IndexRow>> {
        // substr(...) truncates in CHARACTERS (SQLite text semantics), which
        // bounds each row regardless of article size while still supplying
        // everything the 2000-unit document truncation can consume.
        // COALESCE(NULLIF(content_md, ''), content) prefers the cached
        // Markdown (full website text) over the raw RSS snippet — see the
        // IndexRow docs.
        let rows = sqlx::query_as::<_, IndexRowImpl>(
            r#"
            SELECT id, title, link, author, published_at,
                   substr(description, 1, $2) AS description,
                   substr(COALESCE(NULLIF(content_md, ''), content), 1, $2) AS content
            FROM feed_items
            WHERE id > $1
            ORDER BY id ASC
            LIMIT $3
            "#,
        )
        .bind(after_id)
        .bind(INDEX_TEXT_CHARS_SQL)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    async fn find_index_rows_by_ids(&self, ids: &[i64]) -> Result<Vec<IndexRow>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut qb = sqlx::QueryBuilder::new(
            r#"
            SELECT id, title, link, author, published_at,
                   substr(description, 1, "#,
        );
        qb.push_bind(INDEX_TEXT_CHARS_SQL)
            .push(r#") AS description, substr(COALESCE(NULLIF(content_md, ''), content), 1, "#)
            .push_bind(INDEX_TEXT_CHARS_SQL)
            .push(r#") AS content FROM feed_items WHERE id IN ("#);
        let mut separated = qb.separated(", ");
        for id in ids {
            separated.push_bind(id);
        }
        separated.push_unseparated(") ORDER BY id ASC");
        let rows = qb
            .build_query_as::<IndexRowImpl>()
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    async fn max_item_id(&self) -> Result<i64> {
        // MAX() over an empty table yields NULL → Option<i64> row value.
        let max: Option<i64> = sqlx::query_scalar("SELECT MAX(id) FROM feed_items")
            .fetch_optional(&self.pool)
            .await?;
        Ok(max.unwrap_or(0))
    }

    async fn database_id(&self) -> Result<String> {
        sqlx::query_scalar("SELECT value FROM app_metadata WHERE key = 'database_id'")
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| AppError::Internal("Database identity is missing".into()))
    }

    async fn find_website_backfill_candidates(&self, limit: i64) -> Result<Vec<(i64, String)>> {
        let rows: Vec<(i64, String)> = sqlx::query_as(
            r#"
            SELECT f.id, f.link
            FROM feed_items f
            JOIN subscriptions s ON s.id = f.subscription_id
            WHERE s.use_website = 1
              AND f.link IS NOT NULL AND f.link != ''
              AND (f.content_md IS NULL OR f.content_md = '' OR f.is_website_content = 0)
            ORDER BY f.id DESC
            LIMIT $1
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn update_content_md(
        &self,
        id: i64,
        content_md: &str,
        from_website: bool,
    ) -> Result<FeedItem> {
        // Only flip `is_website_content` when the markdown came from the
        // website. Lazy RSS→markdown conversions leave the flag untouched so
        // `is_website_content` keeps its original semantic meaning.
        let row = sqlx::query_as::<_, FeedItemRow>(
            r#"
            UPDATE feed_items
            SET content_md = $2,
                is_website_content = CASE WHEN $3 THEN 1 ELSE is_website_content END
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(content_md)
        .bind(from_website)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("FeedItem with id {} not found", id)))?;

        Ok(row.into())
    }

    async fn reset_content_md(&self, id: i64, content_md: &str) -> Result<FeedItem> {
        // Overwrite both content_md and the website marker — this always
        // reverts to the RSS source, never the website.
        let row = sqlx::query_as::<_, FeedItemRow>(
            r#"
            UPDATE feed_items
            SET content_md = $2, is_website_content = 0
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(content_md)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("FeedItem with id {} not found", id)))?;

        Ok(row.into())
    }

    async fn update_translation(
        &self,
        item_id: i64,
        translated_title: Option<&str>,
        translated_content: &str,
        source_hash: &str,
        model: &str,
        prompt_version: i64,
    ) -> Result<FeedItem> {
        let row = sqlx::query_as::<_, FeedItemRow>(
            r#"
            UPDATE feed_items
            -- An empty string clears the translation: NULLIF keeps the column
            -- NULL so the cache lookup sees "no translation" instead of a
            -- stale empty value.
            SET translated_content = NULLIF($2, ''),
                translated_title = COALESCE($3, translated_title),
                translated_source_hash = CASE WHEN $2 = '' THEN NULL ELSE $4 END,
                translated_model = CASE WHEN $2 = '' THEN NULL ELSE $5 END,
                translated_prompt_version = CASE WHEN $2 = '' THEN NULL ELSE $6 END,
                translated_at = CURRENT_TIMESTAMP
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(item_id)
        .bind(translated_content)
        .bind(translated_title)
        .bind(source_hash)
        .bind(model)
        .bind(prompt_version)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("FeedItem with id {} not found", item_id)))?;

        Ok(row.into())
    }

    async fn adopt_translation_provenance(&self, item_id: i64, source_hash: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE feed_items
               SET translated_source_hash = $2,
                   translated_model = COALESCE(translated_model, $3),
                   translated_prompt_version = COALESCE(translated_prompt_version, 0)
             WHERE id = $1
            "#,
        )
        .bind(item_id)
        .bind(source_hash)
        .bind(crate::ai::LEGACY_TRANSLATION_PROVENANCE)
        .execute(&self.pool)
        .await
        .map_err(|e| map_feed_item_sqlx_error(e, "adopting translation provenance"))?;
        Ok(())
    }

    async fn find_all(
        &self,
        subscription_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FeedItemSummary>> {
        let where_sql = if subscription_id.is_some() {
            "WHERE f.subscription_id = $3"
        } else {
            ""
        };
        self.fetch_summaries(where_sql, subscription_id, limit, offset)
            .await
    }

    async fn search(&self, query: &str, limit: i64) -> Result<Vec<FeedItemSummary>> {
        let pattern = format!("%{}%", escape_like(query));
        let sql = format!(
            r#"SELECT {} FROM {}
               WHERE (f.title LIKE $1 ESCAPE '\'
                  OR f.description LIKE $1 ESCAPE '\'
                  OR f.content LIKE $1 ESCAPE '\'
                  OR f.content_md LIKE $1 ESCAPE '\')
               ORDER BY f.published_at DESC LIMIT $2"#,
            SUMMARY_COLS, SUMMARY_FROM
        );
        let rows = sqlx::query_as::<_, FeedItemSummaryRow>(&sql)
            .bind(&pattern)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    async fn find_summaries_by_ids(&self, ids: &[i64]) -> Result<Vec<FeedItemSummary>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        // QueryBuilder is used (not format!) so the IN list is bound
        // parameters, keeping the query plan cacheable and injection-safe.
        let mut qb = sqlx::QueryBuilder::new(format!(
            "SELECT {} FROM {} WHERE f.id IN (",
            SUMMARY_COLS, SUMMARY_FROM
        ));
        let mut separated = qb.separated(", ");
        for id in ids {
            separated.push_bind(id);
        }
        separated.push_unseparated(")");
        let rows = qb
            .build_query_as::<FeedItemSummaryRow>()
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    async fn find_by_tag(
        &self,
        tag: &str,
        subscription_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FeedItemSummary>> {
        // Exact element match inside the tags JSON array. `json_valid` is
        // required, not defensive: `json_each` raises on a malformed value,
        // so one legacy row would abort the tag filter for the whole library.
        let base = format!(
            r#"SELECT {} FROM {}
               WHERE json_valid(f.tags)
                 AND EXISTS (SELECT 1 FROM json_each(f.tags) WHERE value = $1)"#,
            SUMMARY_COLS, SUMMARY_FROM
        );
        let rows = if let Some(sub_id) = subscription_id {
            sqlx::query_as::<_, FeedItemSummaryRow>(&format!(
                "{} AND f.subscription_id = $2 ORDER BY f.published_at DESC LIMIT $3 OFFSET $4",
                base
            ))
            .bind(tag)
            .bind(sub_id)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, FeedItemSummaryRow>(&format!(
                "{} ORDER BY f.published_at DESC LIMIT $2 OFFSET $3",
                base
            ))
            .bind(tag)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?
        };

        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    async fn find_all_tags(&self, subscription_id: Option<i64>) -> Result<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            r#"WITH src AS (
                   SELECT tags AS names
                     FROM feed_items
                    WHERE json_valid(tags)
                      AND ($1 IS NULL OR subscription_id = $1)
               ), used AS (
                   SELECT DISTINCT j.value AS name
                     FROM src, json_each(src.names) j
               )
               SELECT t.name
                 FROM tag_catalog t
                 JOIN used u ON u.name = t.name
                ORDER BY t.name"#,
        )
        .bind(subscription_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|(tag,)| tag).collect())
    }

    async fn find_tag_catalog(&self) -> Result<Vec<TagCatalogEntry>> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            r#"WITH src AS (
                   SELECT id, tags AS names
                     FROM feed_items
                    WHERE json_valid(tags)
               ), usage AS (
                   SELECT j.value AS name, COUNT(DISTINCT src.id) AS usage_count
                     FROM src, json_each(src.names) j
                    GROUP BY j.value
               )
               SELECT t.name, COALESCE(u.usage_count, 0) AS usage_count
                 FROM tag_catalog t
                 LEFT JOIN usage u ON u.name = t.name
                ORDER BY t.name"#,
        )
        .fetch_all(&self.pool)
        .await?;
        let aliases: Vec<(String, String)> = sqlx::query_as(
            "SELECT alias, canonical_name FROM tag_aliases ORDER BY canonical_name, alias",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut aliases_by_tag: HashMap<String, Vec<String>> = HashMap::new();
        for (alias, canonical) in aliases {
            aliases_by_tag.entry(canonical).or_default().push(alias);
        }

        Ok(rows
            .into_iter()
            .map(|(name, usage_count)| TagCatalogEntry {
                aliases: aliases_by_tag.remove(&name).unwrap_or_default(),
                name,
                usage_count,
            })
            .collect())
    }

    async fn find_tags_missing_explanation(&self, limit: i64) -> Result<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            r#"SELECT t.name
               FROM tag_catalog t
               LEFT JOIN tag_dictionary d ON d.name = t.name
               WHERE d.name IS NULL
               ORDER BY t.name
               LIMIT $1"#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(name,)| name).collect())
    }

    async fn tag_dictionary_status(&self) -> Result<(i64, i64, i64)> {
        let row: (i64, i64, i64) = sqlx::query_as(
            r#"SELECT
                 (SELECT COUNT(*) FROM tag_catalog),
                 (SELECT COUNT(*) FROM tag_catalog t
                    JOIN tag_dictionary d ON d.name = t.name AND d.explanation <> ''),
                 (SELECT COUNT(*) FROM tag_catalog t
                    JOIN tag_dictionary d ON d.name = t.name AND d.embedding IS NOT NULL)"#,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    async fn save_tag_explanations(
        &self,
        entries: &[(String, String)],
        prompt_version: i64,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for (name, explanation) in entries {
            // A new definition invalidates any vector computed from the old one.
            sqlx::query(
                r#"INSERT INTO tag_dictionary (name, explanation, prompt_version)
                   VALUES ($1, $2, $3)
                   ON CONFLICT(name) DO UPDATE SET
                     explanation = excluded.explanation,
                     prompt_version = excluded.prompt_version,
                     embedding = NULL,
                     embedding_key = NULL,
                     updated_at = CURRENT_TIMESTAMP"#,
            )
            .bind(name)
            .bind(explanation)
            .bind(prompt_version)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn find_tag_explanations(&self) -> Result<Vec<(String, String)>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT name, explanation FROM tag_dictionary WHERE explanation <> '' ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn save_tag_embeddings(
        &self,
        key: &str,
        entries: &[(String, Vec<f32>)],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for (name, vector) in entries {
            sqlx::query(
                "UPDATE tag_dictionary SET embedding = $2, embedding_key = $3, updated_at = CURRENT_TIMESTAMP WHERE name = $1",
            )
            .bind(name)
            .bind(encode_embedding(vector))
            .bind(key)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn find_tag_embeddings(&self, key: &str) -> Result<HashMap<String, Vec<f32>>> {
        let rows: Vec<(String, Vec<u8>)> = sqlx::query_as(
            "SELECT name, embedding FROM tag_dictionary WHERE embedding_key = $1 AND embedding IS NOT NULL",
        )
        .bind(key)
        .fetch_all(&self.pool)
        .await?;
        // A row whose BLOB is truncated is skipped rather than guessed at.
        Ok(rows
            .into_iter()
            .filter_map(|(name, bytes)| decode_embedding(&bytes).map(|vector| (name, vector)))
            .collect())
    }

    async fn find_topic_categories(&self) -> Result<Vec<TopicCategory>> {
        let rows: Vec<(i64, String, String, i64)> = sqlx::query_as(
            "SELECT id, label, definition, sort_order FROM topic_categories ORDER BY sort_order, id",
        ).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|(id,label,definition,sort_order)| TopicCategory { id,label,definition,sort_order }).collect())
    }

    async fn save_topic_suggestions(&self, entries: &[(String, String, String)]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for (name, json, key) in entries {
            sqlx::query(r#"INSERT INTO tag_dictionary (name, explanation, suggestion_json, suggestion_key)
                VALUES ($1, '', $2, $3)
                ON CONFLICT(name) DO UPDATE SET suggestion_json=excluded.suggestion_json,
                    suggestion_key=excluded.suggestion_key, updated_at=CURRENT_TIMESTAMP"#)
                .bind(name).bind(json).bind(key).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn find_topic_suggestions(&self, names: &[String]) -> Result<HashMap<String, (String, String)>> {
        if names.is_empty() { return Ok(HashMap::new()); }
        let json = serde_json::to_string(names).map_err(|e| AppError::Internal(e.to_string()))?;
        let rows: Vec<(String,String,String)> = sqlx::query_as(
            "SELECT name, suggestion_json, suggestion_key FROM tag_dictionary
             WHERE suggestion_json IS NOT NULL AND suggestion_key IS NOT NULL
               AND name IN (SELECT value FROM json_each($1))")
            .bind(json).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|(n,j,k)|(n,(j,k))).collect())
    }

    async fn find_topic_assignments(&self) -> Result<Vec<TopicAssignment>> {
        let rows: Vec<(String, Option<i64>, String, String)> = sqlx::query_as(
            "SELECT tag_name, category_id, state, source FROM tag_topic_assignments ORDER BY tag_name",
        ).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|(tag_name,category_id,state,source)| TopicAssignment { tag_name,category_id,state,source }).collect())
    }

    async fn replace_topic_state(&self, categories: &[TopicCategory], assignments: &[TopicAssignment]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for category in categories {
            sqlx::query(r#"INSERT INTO topic_categories (id,label,definition,sort_order) VALUES ($1,$2,$3,$4)
                ON CONFLICT(id) DO UPDATE SET label=excluded.label, definition=excluded.definition,
                sort_order=excluded.sort_order, updated_at=CURRENT_TIMESTAMP"#)
                .bind(category.id).bind(&category.label).bind(&category.definition).bind(category.sort_order)
                .execute(&mut *tx).await?;
        }
        sqlx::query("DELETE FROM tag_topic_assignments").execute(&mut *tx).await?;
        for assignment in assignments {
            sqlx::query("INSERT INTO tag_topic_assignments (tag_name,category_id,state,source) VALUES ($1,$2,$3,$4)")
                .bind(&assignment.tag_name).bind(assignment.category_id).bind(&assignment.state).bind(&assignment.source)
                .execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn find_tag_usage(&self, subscription_id: Option<i64>) -> Result<HashMap<String, i64>> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            r#"SELECT j.value, COUNT(DISTINCT f.id)
                 FROM feed_items f, json_each(f.tags) j
                WHERE ($1 IS NULL OR f.subscription_id = $1)
                  AND json_valid(f.tags)
                GROUP BY j.value"#,
        )
        .bind(subscription_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().collect())
    }

    async fn find_tag_cooccurrence(
        &self,
        subscription_id: Option<i64>,
    ) -> Result<Vec<(String, String, i64)>> {
        let rows: Vec<(String, String, i64)> = sqlx::query_as(
            r#"SELECT a.value, b.value, COUNT(DISTINCT f.id)
                 FROM feed_items f, json_each(f.tags) a, json_each(f.tags) b
                WHERE ($1 IS NULL OR f.subscription_id = $1)
                  AND json_valid(f.tags) AND a.value < b.value
                GROUP BY a.value, b.value"#,
        )
        .bind(subscription_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn find_tag_items(&self, subscription_id: Option<i64>) -> Result<Vec<(i64, String)>> {
        let rows: Vec<(i64, String)> = sqlx::query_as(
            r#"SELECT f.id, j.value
                 FROM feed_items f, json_each(f.tags) j
                WHERE ($1 IS NULL OR f.subscription_id = $1)
                  AND json_valid(f.tags)"#,
        )
        .bind(subscription_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn tag_overview_coverage(
        &self,
        subscription_id: Option<i64>,
    ) -> Result<TagOverviewCoverage> {
        // `json_valid` is NULL for a NULL column, so the two sums below count
        // only rows that really hold a canonical display array.
        let row: (i64, i64, i64) = sqlx::query_as(
            r#"SELECT COUNT(*),
                      SUM(CASE WHEN json_valid(tags) AND json_array_length(tags) > 0
                               THEN 1 ELSE 0 END),
                      SUM(CASE WHEN tags IS NOT NULL AND NOT json_valid(tags)
                               THEN 1 ELSE 0 END)
                 FROM feed_items
                WHERE ($1 IS NULL OR subscription_id = $1)"#,
        )
        .bind(subscription_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(TagOverviewCoverage {
            total_items: row.0,
            tagged_items: row.1,
            unreadable_items: row.2,
        })
    }

    async fn find_blocked_tags(&self) -> Result<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as("SELECT name FROM blocked_tags ORDER BY name")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|(name,)| name).collect())
    }

    async fn create_tag(&self, name: &str) -> Result<()> {
        let name = required_tag(name)?;
        let mut tx = self.pool.begin().await?;
        if sqlx::query_scalar::<_, String>("SELECT name FROM blocked_tags WHERE name = $1")
            .bind(&name)
            .fetch_optional(&mut *tx)
            .await?
            .is_some()
        {
            return Err(AppError::Duplicate(format!("Tag '{}' is blocked", name)));
        }
        if sqlx::query_scalar::<_, String>(
            "SELECT name FROM tag_catalog WHERE name = $1 OR EXISTS (SELECT 1 FROM tag_aliases WHERE alias = $1)",
        )
        .bind(&name)
        .fetch_optional(&mut *tx)
        .await?
        .is_some()
        {
            return Err(AppError::Duplicate(format!("Tag '{}' already exists", name)));
        }
        sqlx::query("INSERT INTO tag_catalog (name) VALUES ($1)")
            .bind(name)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn rename_tag(&self, old_name: &str, new_name: &str) -> Result<()> {
        let old_name = required_tag(old_name)?;
        let new_name = required_tag(new_name)?;
        if old_name == new_name {
            return Ok(());
        }

        let mut tx = self.pool.begin().await?;
        let exists: Option<String> =
            sqlx::query_scalar("SELECT name FROM tag_catalog WHERE name = $1")
                .bind(&old_name)
                .fetch_optional(&mut *tx)
                .await?;
        if exists.is_none() {
            return Err(AppError::NotFound(format!("Tag '{}' not found", old_name)));
        }
        let occupied: Option<String> = sqlx::query_scalar(
            "SELECT name FROM tag_catalog WHERE name = $1
             UNION ALL SELECT alias FROM tag_aliases WHERE alias = $1
             UNION ALL SELECT name FROM blocked_tags WHERE name = $1
             LIMIT 1",
        )
        .bind(&new_name)
        .fetch_optional(&mut *tx)
        .await?;
        if occupied.is_some() {
            return Err(AppError::Duplicate(format!(
                "Tag '{}' already exists",
                new_name
            )));
        }

        let mut replacements = HashMap::new();
        replacements.insert(old_name.clone(), new_name.clone());
        rewrite_feed_item_tags(&mut tx, &replacements, &HashSet::new()).await?;
        sqlx::query("UPDATE tag_aliases SET canonical_name = $2 WHERE canonical_name = $1")
            .bind(&old_name)
            .bind(&new_name)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE tag_catalog SET name = $2, updated_at = CURRENT_TIMESTAMP WHERE name = $1",
        )
        .bind(&old_name)
        .bind(&new_name)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO tag_aliases (alias, canonical_name) VALUES ($1, $2)")
            .bind(old_name)
            .bind(new_name)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn merge_tags(&self, canonical_name: &str, members: &[String]) -> Result<()> {
        let requested_head = required_tag(canonical_name)?;
        let mut tx = self.pool.begin().await?;
        let (aliases, blocked) = load_tag_maps(&mut tx).await?;
        let head = resolve_tag(requested_head, &aliases);
        if blocked.contains(&head) {
            return Err(AppError::Validation(format!("Tag '{}' is blocked", head)));
        }
        let head_exists: Option<String> =
            sqlx::query_scalar("SELECT name FROM tag_catalog WHERE name = $1")
                .bind(&head)
                .fetch_optional(&mut *tx)
                .await?;
        if head_exists.is_none() {
            return Err(AppError::NotFound(format!("Tag '{}' not found", head)));
        }

        let mut selected = Vec::new();
        for member in members {
            let member = resolve_tag(required_tag(member)?, &aliases);
            if member != head && !selected.contains(&member) {
                let exists: Option<String> =
                    sqlx::query_scalar("SELECT name FROM tag_catalog WHERE name = $1")
                        .bind(&member)
                        .fetch_optional(&mut *tx)
                        .await?;
                if exists.is_none() {
                    return Err(AppError::NotFound(format!("Tag '{}' not found", member)));
                }
                selected.push(member);
            }
        }

        let replacements: HashMap<String, String> = selected
            .iter()
            .cloned()
            .map(|member| (member, head.clone()))
            .collect();
        rewrite_feed_item_tags(&mut tx, &replacements, &HashSet::new()).await?;
        for member in &selected {
            sqlx::query("UPDATE tag_aliases SET canonical_name = $1 WHERE canonical_name = $2")
                .bind(&head)
                .bind(member)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT OR REPLACE INTO tag_aliases (alias, canonical_name) VALUES ($1, $2)",
            )
            .bind(member)
            .bind(&head)
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM tag_catalog WHERE name = $1")
                .bind(member)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn merge_tag_pairs(&self, pairs: &[(String, String)]) -> Result<usize> {
        if pairs.is_empty() { return Ok(0); }
        let mut tx = self.pool.begin().await?;
        let (aliases, blocked) = load_tag_maps(&mut tx).await?;
        let catalog: HashSet<String> = sqlx::query_as::<_, (String,)>("SELECT name FROM tag_catalog")
            .fetch_all(&mut *tx).await?.into_iter().map(|(name,)| name).collect();
        let mut replacements = HashMap::new();
        for (member, canonical) in pairs {
            let member = resolve_tag(required_tag(member)?, &aliases);
            let canonical = resolve_tag(required_tag(canonical)?, &aliases);
            if member == canonical || replacements.contains_key(&member) { continue; }
            if !catalog.contains(&member) || !catalog.contains(&canonical) {
                return Err(AppError::NotFound(format!("Cannot merge '{}' into '{}': tag not found", member, canonical)));
            }
            if blocked.contains(&canonical) {
                return Err(AppError::Validation(format!("Tag '{}' is blocked", canonical)));
            }
            replacements.insert(member, canonical);
        }
        if replacements.is_empty() { return Ok(0); }
        if replacements.values().any(|canonical| replacements.contains_key(canonical)) {
            return Err(AppError::Validation("Bulk tag merges must target tags that are not being merged".into()));
        }

        // One pass over feed_items for the whole cleanup, rather than one full
        // rewrite per singleton.
        rewrite_feed_item_tags(&mut tx, &replacements, &HashSet::new()).await?;
        for (member, canonical) in &replacements {
            sqlx::query("UPDATE tag_aliases SET canonical_name = $1 WHERE canonical_name = $2")
                .bind(canonical).bind(member).execute(&mut *tx).await?;
            sqlx::query("INSERT OR REPLACE INTO tag_aliases (alias, canonical_name) VALUES ($1, $2)")
                .bind(member).bind(canonical).execute(&mut *tx).await?;
            sqlx::query("DELETE FROM tag_catalog WHERE name = $1")
                .bind(member).execute(&mut *tx).await?;
            sqlx::query("DELETE FROM tag_topic_assignments WHERE tag_name = $1")
                .bind(member).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(replacements.len())
    }

    async fn delete_tag(&self, name: &str) -> Result<()> {
        let name = required_tag(name)?;
        let mut tx = self.pool.begin().await?;
        let exists: Option<String> =
            sqlx::query_scalar("SELECT name FROM tag_catalog WHERE name = $1")
                .bind(&name)
                .fetch_optional(&mut *tx)
                .await?;
        if exists.is_none() {
            return Err(AppError::NotFound(format!("Tag '{}' not found", name)));
        }
        let aliases: Vec<(String,)> =
            sqlx::query_as("SELECT alias FROM tag_aliases WHERE canonical_name = $1")
                .bind(&name)
                .fetch_all(&mut *tx)
                .await?;
        let removed: HashSet<String> = std::iter::once(name.clone())
            .chain(aliases.iter().map(|(alias,)| alias.clone()))
            .collect();
        rewrite_feed_item_tags(&mut tx, &HashMap::new(), &removed).await?;
        for blocked in &removed {
            sqlx::query("INSERT OR IGNORE INTO blocked_tags (name) VALUES ($1)")
                .bind(blocked)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("DELETE FROM tag_aliases WHERE canonical_name = $1")
            .bind(&name)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM tag_catalog WHERE name = $1")
            .bind(&name)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn restore_tag(&self, name: &str) -> Result<()> {
        let name = required_tag(name)?;
        let mut tx = self.pool.begin().await?;

        let was_blocked: Option<String> =
            sqlx::query_scalar("SELECT name FROM blocked_tags WHERE name = $1")
                .bind(&name)
                .fetch_optional(&mut *tx)
                .await?;
        if was_blocked.is_none() {
            return Err(AppError::NotFound(format!(
                "Blocked tag '{}' not found",
                name
            )));
        }

        let occupied: Option<String> = sqlx::query_scalar(
            "SELECT name FROM tag_catalog WHERE name = $1
             UNION ALL SELECT alias FROM tag_aliases WHERE alias = $1
             LIMIT 1",
        )
        .bind(&name)
        .fetch_optional(&mut *tx)
        .await?;
        if occupied.is_some() {
            return Err(AppError::Duplicate(format!(
                "Tag '{}' already exists",
                name
            )));
        }

        sqlx::query("DELETE FROM blocked_tags WHERE name = $1")
            .bind(&name)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO tag_catalog (name) VALUES ($1)")
            .bind(name)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn add_tag_alias(&self, alias: &str, canonical_name: &str) -> Result<()> {
        let alias = required_tag(alias)?;
        let canonical_name = required_tag(canonical_name)?;
        if alias == canonical_name {
            return Ok(());
        }

        let mut tx = self.pool.begin().await?;
        // The target of a synonym has to be a known name, or the alias would
        // point at nothing.
        let head_exists: Option<String> =
            sqlx::query_scalar("SELECT name FROM tag_catalog WHERE name = $1")
                .bind(&canonical_name)
                .fetch_optional(&mut *tx)
                .await?;
        if head_exists.is_none() {
            return Err(AppError::NotFound(format!(
                "Tag '{}' not found",
                canonical_name
            )));
        }
        // An active tag or a blocked name must never be shadowed by a mapping;
        // merging or restoring are the explicit operations for those cases.
        let occupied: Option<String> = sqlx::query_scalar(
            "SELECT name FROM tag_catalog WHERE name = $1
             UNION ALL SELECT name FROM blocked_tags WHERE name = $1
             LIMIT 1",
        )
        .bind(&alias)
        .fetch_optional(&mut *tx)
        .await?;
        if occupied.is_some() {
            return Err(AppError::Duplicate(format!(
                "Tag '{}' is an active or blocked name",
                alias
            )));
        }
        sqlx::query("INSERT OR IGNORE INTO tag_aliases (alias, canonical_name) VALUES ($1, $2)")
            .bind(alias)
            .bind(canonical_name)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn mark_read(&self, id: i64, is_read: bool) -> Result<FeedItem> {
        let row = sqlx::query_as::<_, FeedItemRow>(
            r#"
            UPDATE feed_items
            SET is_read = $2
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(is_read)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("FeedItem with id {} not found", id)))?;

        Ok(row.into())
    }

    async fn mark_all_read(&self, subscription_id: Option<i64>) -> Result<()> {
        match subscription_id {
            Some(id) => {
                sqlx::query(
                    "UPDATE feed_items SET is_read = 1 WHERE subscription_id = $1 AND is_read = 0",
                )
                .bind(id)
                .execute(&self.pool)
                .await?;
            }
            None => {
                sqlx::query("UPDATE feed_items SET is_read = 1 WHERE is_read = 0")
                    .execute(&self.pool)
                    .await?;
            }
        }
        Ok(())
    }

    async fn toggle_favorite(&self, id: i64) -> Result<bool> {
        self.toggle_flag(id, "is_favorite").await
    }

    async fn toggle_read_later(&self, id: i64) -> Result<bool> {
        self.toggle_flag(id, "is_read_later").await
    }

    async fn toggle_ignored(&self, id: i64) -> Result<bool> {
        self.toggle_flag(id, "is_ignored").await
    }

    async fn get_favorites(
        &self,
        subscription_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FeedItemSummary>> {
        let where_sql = if subscription_id.is_some() {
            "WHERE f.is_favorite = 1 AND f.subscription_id = $3"
        } else {
            "WHERE f.is_favorite = 1"
        };
        self.fetch_summaries(where_sql, subscription_id, limit, offset)
            .await
    }

    async fn get_read_later(
        &self,
        subscription_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FeedItemSummary>> {
        let where_sql = if subscription_id.is_some() {
            "WHERE f.is_read_later = 1 AND f.subscription_id = $3"
        } else {
            "WHERE f.is_read_later = 1"
        };
        self.fetch_summaries(where_sql, subscription_id, limit, offset)
            .await
    }

    async fn get_unread(
        &self,
        subscription_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FeedItemSummary>> {
        let where_sql = if subscription_id.is_some() {
            "WHERE f.subscription_id = $3 AND f.is_read = 0"
        } else {
            "WHERE f.is_read = 0"
        };
        self.fetch_summaries(where_sql, subscription_id, limit, offset)
            .await
    }

    async fn get_today_items(
        &self,
        subscription_id: Option<i64>,
        unread_only: bool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FeedItemSummary>> {
        // "Today" in the user's LOCAL timezone, expressed as a UTC range so
        // the published_at index can be used (no per-row DATE() function).
        let today = chrono::Local::now().date_naive();
        let start_local = today
            .and_hms_opt(0, 0, 0)
            .expect("00:00:00 is always valid");
        let start_utc: chrono::DateTime<chrono::Utc> = start_local
            .and_local_timezone(chrono::Local)
            .single()
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|| start_local.and_utc());
        let end_utc = start_utc + chrono::Duration::days(1);

        let mut where_sql = String::from("WHERE f.published_at >= $1 AND f.published_at < $2");
        if subscription_id.is_some() {
            where_sql.push_str(" AND f.subscription_id = $5");
        }
        if unread_only {
            where_sql.push_str(" AND f.is_read = 0");
        }

        let sql = format!(
            "SELECT {} FROM {} {} ORDER BY f.published_at DESC LIMIT $3 OFFSET $4",
            SUMMARY_COLS, SUMMARY_FROM, where_sql
        );
        let mut q = sqlx::query_as::<_, FeedItemSummaryRow>(&sql)
            .bind(start_utc)
            .bind(end_utc)
            .bind(limit)
            .bind(offset);
        if let Some(sub_id) = subscription_id {
            q = q.bind(sub_id);
        }
        let rows = q.fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    /// Persist the classifier's (or the user's) names for one article.
    ///
    /// `tags` is the ORIGINAL list, before synonym resolution. `raw_tags` keeps
    /// all of it — that is the point of the raw column: renaming a tag, folding
    /// in a synonym or filing the words under topics later must not need the
    /// model's answer again. The displayed `tags` column holds at most
    /// [`MAX_TAGS_PER_ITEM`] resolved names, so a fourth proposal is recorded
    /// but not shown.
    async fn save_tags(&self, item_id: i64, tags: &str) -> Result<FeedItem> {
        let proposed: Vec<String> = serde_json::from_str(tags)
            .map_err(|e| AppError::Validation(format!("Invalid tags JSON: {}", e)))?;
        let mut tx = self.pool.begin().await?;
        let (aliases, blocked) = load_tag_maps(&mut tx).await?;
        let mut raw_names: Vec<String> = Vec::new();
        let mut normalized = Vec::new();

        for raw in proposed {
            let Some(tag) = normalize_tag(&raw) else {
                continue;
            };
            if blocked.contains(&tag) {
                continue;
            }
            if !raw_names.contains(&tag) {
                raw_names.push(tag.clone());
            }
            // The display cap must not truncate the raw record, so exceeding
            // it skips the display path instead of leaving the loop.
            if normalized.len() == MAX_TAGS_PER_ITEM {
                continue;
            }
            let tag = resolve_tag(tag, &aliases);
            if blocked.contains(&tag) {
                continue;
            }
            if normalized.contains(&tag) {
                continue;
            }
            // Every name that reaches the article view joins the vocabulary,
            // so the classifier is offered it next time instead of inventing a
            // near-duplicate. Names past the display cap stay in `raw_tags`
            // only, which is why they are not added here.
            sqlx::query("INSERT OR IGNORE INTO tag_catalog (name) VALUES ($1)")
                .bind(&tag)
                .execute(&mut *tx)
                .await?;
            normalized.push(tag);
        }

        let normalized_json = serde_json::to_string(&normalized)
            .map_err(|e| AppError::Internal(format!("Failed to serialize tags: {}", e)))?;
        let raw_json = serde_json::to_string(&raw_names)
            .map_err(|e| AppError::Internal(format!("Failed to serialize raw tags: {}", e)))?;
        let row = sqlx::query_as::<_, FeedItemRow>(
            r#"
            UPDATE feed_items
            SET tags = $2, raw_tags = $3
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(item_id)
        .bind(normalized_json)
        .bind(raw_json)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("FeedItem with id {} not found", item_id)))?;

        tx.commit().await?;
        Ok(row.into())
    }
}

impl SqliteFeedItemRepository {
    /// Atomically flip a boolean column and return the new value.
    async fn toggle_flag(&self, id: i64, column: &str) -> Result<bool> {
        // `column` is only ever one of the three hardcoded literals above.
        let sql = format!(
            "UPDATE feed_items SET {col} = NOT {col} WHERE id = $1 RETURNING {col}",
            col = column
        );
        let value: Option<bool> = sqlx::query_scalar(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        value.ok_or_else(|| AppError::NotFound(format!("FeedItem with id {} not found", id)))
    }
}

fn map_feed_item_sqlx_error(e: sqlx::Error, context: &str) -> AppError {
    match &e {
        sqlx::Error::Database(db_err) => {
            let msg = db_err.message().to_lowercase();
            if msg.contains("unique") || msg.contains("duplicate") {
                AppError::Duplicate(format!("Duplicate entry {}", context))
            } else {
                AppError::Database(e)
            }
        }
        _ => AppError::Database(e),
    }
}

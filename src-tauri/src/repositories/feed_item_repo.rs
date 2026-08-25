use std::collections::HashSet;

use async_trait::async_trait;
use sqlx::SqlitePool;

use super::FeedItemRepository;
use super::IndexRow;
use crate::error::{AppError, Result};
use crate::models::{FeedItem, FeedItemSummary, NewFeedItem};

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
    f.is_ignored, f.tags, f.category, f.translated_title, \
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
    pub category: Option<String>,
    pub translated_title: Option<String>,
    pub translated_content: Option<String>,
    pub translated_at: Option<chrono::DateTime<chrono::Utc>>,
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
    pub category: Option<String>,
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
            category: r.category,
            translated_title: r.translated_title,
            translated_content: r.translated_content,
            translated_at: r.translated_at,
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
    pub category: Option<String>,
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
            category: r.category,
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
            category: r.category,
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

/// Production implementation backed by a real SQLite pool.
#[derive(Clone)]
pub struct SqliteFeedItemRepository {
    pool: SqlitePool,
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
        // ON CONFLICT DO NOTHING relies on the (subscription_id, guid) unique
        // index as a last-resort dedup guard; in-memory dedup during fetch is
        // the primary mechanism. A conflicting insert returns no row, which we
        // surface as a Duplicate error that callers treat as "already exists".
        let row = sqlx::query_as::<_, FeedItemRow>(
            r#"
            INSERT INTO feed_items (
                subscription_id, guid, title, link, content, content_md,
                description, author, published_at,
                is_website_content, is_read, is_favorite, is_read_later, is_ignored,
                tags, category,
                translated_title, translated_content, translated_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)
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
        .bind(&input.category)
        .bind(&input.translated_title)
        .bind(&input.translated_content)
        .bind(input.translated_at)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_feed_item_sqlx_error(e, "creating feed item"))?
        .ok_or_else(|| {
            AppError::Duplicate(format!(
                "feed item already exists (subscription {}, guid {:?})",
                input.subscription_id, input.guid
            ))
        })?;

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
        let rows: Vec<(Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT guid, link FROM feed_items WHERE subscription_id = $1",
        )
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
            SELECT id, title, link, author, published_at, category,
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
            SELECT id, title, link, author, published_at, category,
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

    async fn find_website_backfill_candidates(
        &self,
        limit: i64,
    ) -> Result<Vec<(i64, String)>> {
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

    async fn reset_content_md(
        &self,
        id: i64,
        content_md: &str,
    ) -> Result<FeedItem> {
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
    ) -> Result<FeedItem> {
        let row = sqlx::query_as::<_, FeedItemRow>(
            r#"
            UPDATE feed_items
            -- An empty string clears the translation (force re-translate path):
            -- NULLIF keeps the column NULL so the cache lookup sees "no
            -- translation" instead of a stale empty value.
            SET translated_content = NULLIF($2, ''),
                translated_title = COALESCE($3, translated_title),
                translated_at = CURRENT_TIMESTAMP
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(item_id)
        .bind(translated_content)
        .bind(translated_title)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("FeedItem with id {} not found", item_id)))?;

        Ok(row.into())
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
        self.fetch_summaries(where_sql, subscription_id, limit, offset).await
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
        // Exact element match inside the tags JSON array
        let base = format!(
            r#"SELECT {} FROM {}
               WHERE EXISTS (SELECT 1 FROM json_each(f.tags) WHERE value = $1)"#,
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
        let rows: Vec<(String,)> = if let Some(sub_id) = subscription_id {
            sqlx::query_as(
                r#"SELECT DISTINCT value FROM feed_items, json_each(feed_items.tags)
                   WHERE tags IS NOT NULL AND json_valid(tags) AND subscription_id = $1
                   ORDER BY value"#,
            )
            .bind(sub_id)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as(
                r#"SELECT DISTINCT value FROM feed_items, json_each(feed_items.tags)
                   WHERE tags IS NOT NULL AND json_valid(tags)
                   ORDER BY value"#,
            )
            .fetch_all(&self.pool)
            .await?
        };

        Ok(rows.into_iter().map(|(tag,)| tag).collect())
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
        self.fetch_summaries(where_sql, subscription_id, limit, offset).await
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
        self.fetch_summaries(where_sql, subscription_id, limit, offset).await
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
        self.fetch_summaries(where_sql, subscription_id, limit, offset).await
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

    async fn save_tags(&self, item_id: i64, tags: &str, category: &str) -> Result<FeedItem> {
        let row = sqlx::query_as::<_, FeedItemRow>(
            r#"
            UPDATE feed_items
            SET tags = $2, category = $3
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(item_id)
        .bind(tags)
        .bind(category)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("FeedItem with id {} not found", item_id)))?;

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

use async_trait::async_trait;
use sqlx::SqlitePool;

use super::FeedItemRepository;
use crate::error::{AppError, Result};
use crate::models::{FeedItem, NewFeedItem};

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

/// Production implementation backed by a real SQLite pool.
#[derive(Clone)]
pub struct SqliteFeedItemRepository {
    pool: SqlitePool,
}

impl SqliteFeedItemRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl FeedItemRepository for SqliteFeedItemRepository {
    async fn create(&self, input: NewFeedItem) -> Result<FeedItem> {
        let row = sqlx::query_as::<_, FeedItemRow>(
            r#"
            INSERT INTO feed_items (
                subscription_id, guid, title, link, content, content_md,
                description, author, published_at,
                is_website_content, is_read, is_favorite, is_read_later, is_ignored,
                tags, category,
                translated_title, translated_content, translated_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)
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
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_feed_item_sqlx_error(e, "creating feed item"))?;

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

    async fn find_by_subscription(&self, subscription_id: i64) -> Result<Vec<FeedItem>> {
        let rows = sqlx::query_as::<_, FeedItemRow>(
            "SELECT * FROM feed_items WHERE subscription_id = $1 ORDER BY published_at DESC",
        )
        .bind(subscription_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    async fn update_content_md(&self, id: i64, content_md: &str) -> Result<FeedItem> {
        let row = sqlx::query_as::<_, FeedItemRow>(
            r#"
            UPDATE feed_items
            SET content_md = $2
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

    #[allow(dead_code)]
    async fn delete(&self, id: i64) -> Result<()> {
        let result = sqlx::query("DELETE FROM feed_items WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;

        if result.rows_affected() == 0 {
            return Err(AppError::NotFound(format!(
                "FeedItem with id {} not found",
                id
            )));
        }

        Ok(())
    }

    async fn find_all(
        &self,
        subscription_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FeedItem>> {
        let rows = if let Some(sub_id) = subscription_id {
            sqlx::query_as::<_, FeedItemRow>(
                "SELECT * FROM feed_items WHERE subscription_id = $1 ORDER BY published_at DESC LIMIT $2 OFFSET $3",
            )
            .bind(sub_id)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, FeedItemRow>(
                "SELECT * FROM feed_items ORDER BY published_at DESC LIMIT $1 OFFSET $2",
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?
        };

        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    async fn search(&self, query: &str, limit: i64) -> Result<Vec<FeedItem>> {
        let pattern = format!("%{}%", query);
        let rows = sqlx::query_as::<_, FeedItemRow>(
            r#"
            SELECT * FROM feed_items
            WHERE title LIKE $1 OR description LIKE $1 OR content LIKE $1
            ORDER BY published_at DESC
            LIMIT $2
            "#,
        )
        .bind(&pattern)
        .bind(limit)
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
    ) -> Result<Vec<FeedItem>> {
        // Match the tag as a JSON string element: "tag"
        let pattern = format!("%\"{}\"%", tag);
        let rows = if let Some(sub_id) = subscription_id {
            sqlx::query_as::<_, FeedItemRow>(
                r#"
                SELECT * FROM feed_items
                WHERE tags LIKE $1 AND subscription_id = $2
                ORDER BY published_at DESC
                LIMIT $3 OFFSET $4
                "#,
            )
            .bind(&pattern)
            .bind(sub_id)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, FeedItemRow>(
                r#"
                SELECT * FROM feed_items
                WHERE tags LIKE $1
                ORDER BY published_at DESC
                LIMIT $2 OFFSET $3
                "#,
            )
            .bind(&pattern)
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
                "SELECT tags FROM feed_items WHERE tags IS NOT NULL AND subscription_id = $1",
            )
            .bind(sub_id)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as("SELECT tags FROM feed_items WHERE tags IS NOT NULL")
                .fetch_all(&self.pool)
                .await?
        };

        let mut all_tags: Vec<String> = Vec::new();
        for (tags_json,) in rows {
            if let Ok(tags) = serde_json::from_str::<Vec<String>>(&tags_json) {
                for tag in tags {
                    if !all_tags.contains(&tag) {
                        all_tags.push(tag);
                    }
                }
            }
        }
        all_tags.sort();
        Ok(all_tags)
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
                sqlx::query("UPDATE feed_items SET is_read = 1 WHERE subscription_id = $1")
                    .bind(id)
                    .execute(&self.pool)
                    .await?;
            }
            None => {
                sqlx::query("UPDATE feed_items SET is_read = 1")
                    .execute(&self.pool)
                    .await?;
            }
        }
        Ok(())
    }

    async fn toggle_favorite(&self, id: i64) -> Result<bool> {
        let row: FeedItemRow = self.find_by_id_internal(id).await?;
        let new_value = !row.is_favorite;
        sqlx::query("UPDATE feed_items SET is_favorite = $2 WHERE id = $1")
            .bind(id)
            .bind(new_value)
            .execute(&self.pool)
            .await?;
        Ok(new_value)
    }

    async fn toggle_read_later(&self, id: i64) -> Result<bool> {
        let row: FeedItemRow = self.find_by_id_internal(id).await?;
        let new_value = !row.is_read_later;
        sqlx::query("UPDATE feed_items SET is_read_later = $2 WHERE id = $1")
            .bind(id)
            .bind(new_value)
            .execute(&self.pool)
            .await?;
        Ok(new_value)
    }

    async fn toggle_ignored(&self, id: i64) -> Result<bool> {
        let row: FeedItemRow = self.find_by_id_internal(id).await?;
        let new_value = !row.is_ignored;
        sqlx::query("UPDATE feed_items SET is_ignored = $2 WHERE id = $1")
            .bind(id)
            .bind(new_value)
            .execute(&self.pool)
            .await?;
        Ok(new_value)
    }

    async fn get_favorites(&self, limit: i64, offset: i64) -> Result<Vec<FeedItem>> {
        let rows = sqlx::query_as::<_, FeedItemRow>(
            "SELECT * FROM feed_items WHERE is_favorite = 1 ORDER BY published_at DESC LIMIT $1 OFFSET $2",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    async fn get_read_later(&self, limit: i64, offset: i64) -> Result<Vec<FeedItem>> {
        let rows = sqlx::query_as::<_, FeedItemRow>(
            "SELECT * FROM feed_items WHERE is_read_later = 1 ORDER BY published_at DESC LIMIT $1 OFFSET $2",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    async fn get_unread(
        &self,
        subscription_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FeedItem>> {
        let rows = if let Some(sub_id) = subscription_id {
            sqlx::query_as::<_, FeedItemRow>(
                r#"
                SELECT * FROM feed_items
                WHERE subscription_id = $1 AND is_read = 0
                ORDER BY published_at DESC
                LIMIT $2 OFFSET $3
                "#,
            )
            .bind(sub_id)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, FeedItemRow>(
                r#"
                SELECT * FROM feed_items
                WHERE is_read = 0
                ORDER BY published_at DESC
                LIMIT $1 OFFSET $2
                "#,
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?
        };

        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    async fn get_today_items(
        &self,
        subscription_id: Option<i64>,
        unread_only: bool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FeedItem>> {
        let rows = match (subscription_id, unread_only) {
            (Some(sub_id), true) => {
                sqlx::query_as::<_, FeedItemRow>(
                    r#"
                    SELECT * FROM feed_items
                    WHERE subscription_id = $1
                      AND DATE(published_at) = DATE('now')
                      AND is_read = 0
                    ORDER BY published_at DESC
                    LIMIT $2 OFFSET $3
                    "#,
                )
                .bind(sub_id)
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await?
            }
            (Some(sub_id), false) => {
                sqlx::query_as::<_, FeedItemRow>(
                    r#"
                    SELECT * FROM feed_items
                    WHERE subscription_id = $1
                      AND DATE(published_at) = DATE('now')
                    ORDER BY published_at DESC
                    LIMIT $2 OFFSET $3
                    "#,
                )
                .bind(sub_id)
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await?
            }
            (None, true) => {
                sqlx::query_as::<_, FeedItemRow>(
                    r#"
                    SELECT * FROM feed_items
                    WHERE DATE(published_at) = DATE('now')
                      AND is_read = 0
                    ORDER BY published_at DESC
                    LIMIT $1 OFFSET $2
                    "#,
                )
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await?
            }
            (None, false) => {
                sqlx::query_as::<_, FeedItemRow>(
                    r#"
                    SELECT * FROM feed_items
                    WHERE DATE(published_at) = DATE('now')
                    ORDER BY published_at DESC
                    LIMIT $1 OFFSET $2
                    "#,
                )
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await?
            }
        };

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
    /// Internal helper: fetch a row by ID for toggle operations.
    async fn find_by_id_internal(&self, id: i64) -> Result<FeedItemRow> {
        sqlx::query_as::<_, FeedItemRow>("SELECT * FROM feed_items WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("FeedItem with id {} not found", id)))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::helpers::TestEnv;

    async fn seed_sub(env: &TestEnv) -> i64 {
        seed_sub_with_url(env, "https://example.com/rss").await
    }

    async fn seed_sub_with_url(env: &TestEnv, url: &str) -> i64 {
        env.service
            .add_subscription(crate::models::NewSubscription {
                url: url.into(),
                title: Some("Test Sub".into()),
                ..Default::default()
            })
            .await
            .expect("Failed to seed subscription")
            .id
    }

    async fn create_item(env: &TestEnv, sub_id: i64, title: &str) -> i64 {
        env.feed_service
            .create_item(NewFeedItem {
                subscription_id: sub_id,
                title: title.into(),
                content: Some("<p>Test content</p>".into()),
                ..Default::default()
            })
            .await
            .expect("Failed to create feed item")
            .id
    }

    #[tokio::test]
    async fn test_find_all_pagination() {
        let env = TestEnv::new().await;
        let sub_id = seed_sub(&env).await;
        for i in 0..5 {
            create_item(&env, sub_id, &format!("Item {}", i)).await;
        }

        let page1 = env.feed_repo.find_all(Some(sub_id), 2, 0).await.unwrap();
        assert_eq!(page1.len(), 2);

        let all = env.feed_repo.find_all(None, 10, 0).await.unwrap();
        assert_eq!(all.len(), 5);
    }

    #[tokio::test]
    async fn test_search_items() {
        let env = TestEnv::new().await;
        let sub_id = seed_sub(&env).await;
        create_item(&env, sub_id, "Rust Programming").await;
        create_item(&env, sub_id, "JavaScript Tips").await;
        create_item(&env, sub_id, "Cooking Recipes").await;

        let results = env.feed_repo.search("Rust", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Rust Programming");
    }

    #[tokio::test]
    async fn test_mark_read() {
        let env = TestEnv::new().await;
        let sub_id = seed_sub(&env).await;
        let item_id = create_item(&env, sub_id, "To Read").await;

        let updated = env.feed_repo.mark_read(item_id, true).await.unwrap();
        assert!(updated.is_read);

        let updated = env.feed_repo.mark_read(item_id, false).await.unwrap();
        assert!(!updated.is_read);
    }

    #[tokio::test]
    async fn test_mark_all_read() {
        let env = TestEnv::new().await;
        let sub_id = seed_sub(&env).await;
        create_item(&env, sub_id, "Item 1").await;
        create_item(&env, sub_id, "Item 2").await;

        env.feed_repo.mark_all_read(Some(sub_id)).await.unwrap();

        let items = env.feed_repo.find_by_subscription(sub_id).await.unwrap();
        assert!(items.iter().all(|i| i.is_read));
    }

    #[tokio::test]
    async fn test_mark_all_read_all_subscriptions() {
        let env = TestEnv::new().await;
        let sub_a = seed_sub_with_url(&env, "https://example.com/feed-a").await;
        let sub_b = seed_sub_with_url(&env, "https://example.com/feed-b").await;
        create_item(&env, sub_a, "A1").await;
        create_item(&env, sub_b, "B1").await;

        // Mark ALL items read across every subscription
        env.feed_repo.mark_all_read(None).await.unwrap();

        for sub_id in [sub_a, sub_b] {
            let items = env.feed_repo.find_by_subscription(sub_id).await.unwrap();
            assert!(items.iter().all(|i| i.is_read), "all items in sub {} should be read", sub_id);
        }
    }

    #[tokio::test]
    async fn test_toggle_favorite() {
        let env = TestEnv::new().await;
        let sub_id = seed_sub(&env).await;
        let item_id = create_item(&env, sub_id, "Fav Test").await;

        let state = env.feed_repo.toggle_favorite(item_id).await.unwrap();
        assert!(state);

        let state = env.feed_repo.toggle_favorite(item_id).await.unwrap();
        assert!(!state);
    }

    #[tokio::test]
    async fn test_toggle_read_later() {
        let env = TestEnv::new().await;
        let sub_id = seed_sub(&env).await;
        let item_id = create_item(&env, sub_id, "Later").await;

        let state = env.feed_repo.toggle_read_later(item_id).await.unwrap();
        assert!(state);

        let state = env.feed_repo.toggle_read_later(item_id).await.unwrap();
        assert!(!state);
    }

    #[tokio::test]
    async fn test_toggle_ignored() {
        let env = TestEnv::new().await;
        let sub_id = seed_sub(&env).await;
        let item_id = create_item(&env, sub_id, "Ignore").await;

        let state = env.feed_repo.toggle_ignored(item_id).await.unwrap();
        assert!(state);

        let state = env.feed_repo.toggle_ignored(item_id).await.unwrap();
        assert!(!state);
    }

    #[tokio::test]
    async fn test_get_favorites() {
        let env = TestEnv::new().await;
        let sub_id = seed_sub(&env).await;
        let id1 = create_item(&env, sub_id, "Fav 1").await;
        let id2 = create_item(&env, sub_id, "Fav 2").await;
        create_item(&env, sub_id, "Not Fav").await;

        env.feed_repo.toggle_favorite(id1).await.unwrap();
        env.feed_repo.toggle_favorite(id2).await.unwrap();

        let favs = env.feed_repo.get_favorites(10, 0).await.unwrap();
        assert_eq!(favs.len(), 2);
    }

    #[tokio::test]
    async fn test_get_unread() {
        let env = TestEnv::new().await;
        let sub_id = seed_sub(&env).await;
        let id1 = create_item(&env, sub_id, "Unread 1").await;
        create_item(&env, sub_id, "Unread 2").await;
        env.feed_repo.mark_read(id1, true).await.unwrap();

        let unread = env.feed_repo.get_unread(Some(sub_id), 10, 0).await.unwrap();
        assert_eq!(unread.len(), 1);
    }

    #[tokio::test]
    async fn test_save_tags() {
        let env = TestEnv::new().await;
        let sub_id = seed_sub(&env).await;
        let item_id = create_item(&env, sub_id, "Tagged").await;

        let updated = env
            .feed_repo
            .save_tags(item_id, r#"["rust","programming"]"#, "tech")
            .await
            .unwrap();
        assert_eq!(updated.tags, Some(r#"["rust","programming"]"#.into()));
        assert_eq!(updated.category, Some("tech".into()));
    }

    #[tokio::test]
    async fn test_find_by_tag() {
        let env = TestEnv::new().await;
        let sub_id = seed_sub(&env).await;
        let id1 = create_item(&env, sub_id, "Rust Article").await;
        let id2 = create_item(&env, sub_id, "Cooking Article").await;

        env.feed_repo
            .save_tags(id1, r#"["rust","programming"]"#, "tech")
            .await
            .unwrap();
        env.feed_repo
            .save_tags(id2, r#"["cooking"]"#, "lifestyle")
            .await
            .unwrap();

        let results = env
            .feed_repo
            .find_by_tag("rust", None, 10, 0)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Rust Article");
    }

    #[tokio::test]
    async fn test_find_all_tags() {
        let env = TestEnv::new().await;
        let sub_id = seed_sub(&env).await;

        let id1 = create_item(&env, sub_id, "Article 1").await;
        let id2 = create_item(&env, sub_id, "Article 2").await;

        env.feed_repo
            .save_tags(id1, r#"["rust","programming"]"#, "tech")
            .await
            .unwrap();
        env.feed_repo
            .save_tags(id2, r#"["rust","web"]"#, "tech")
            .await
            .unwrap();

        let all_tags = env.feed_repo.find_all_tags(None).await.unwrap();
        assert!(all_tags.contains(&"rust".to_string()));
        assert!(all_tags.contains(&"programming".to_string()));
        assert!(all_tags.contains(&"web".to_string()));
    }
}

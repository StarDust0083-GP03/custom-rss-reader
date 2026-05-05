use async_trait::async_trait;
use sqlx::SqlitePool;

use super::SubscriptionRepository;
use crate::error::{AppError, Result};
use crate::models::{NewSubscription, Subscription, UpdateSubscription};

/// Private database row type that maps directly to the `subscriptions` table.
/// This is NOT exposed outside the repository — callers interact with the domain `Subscription` model.
#[derive(sqlx::FromRow)]
struct SubscriptionRow {
    pub id: i64,
    pub url: String,
    pub title: Option<String>,
    pub website_url: Option<String>,
    pub rsshub_url: Option<String>,
    pub use_website: bool,
    pub auto_classify: bool,
    pub opml_attributes: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<SubscriptionRow> for Subscription {
    fn from(r: SubscriptionRow) -> Self {
        Subscription {
            id: r.id,
            url: r.url,
            title: r.title,
            website_url: r.website_url,
            rsshub_url: r.rsshub_url,
            use_website: r.use_website,
            auto_classify: r.auto_classify,
            opml_attributes: r.opml_attributes,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// Production implementation backed by a real SQLite pool.
#[derive(Clone)]
pub struct SqliteSubscriptionRepository {
    pool: SqlitePool,
}

impl SqliteSubscriptionRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SubscriptionRepository for SqliteSubscriptionRepository {
    async fn create(&self, input: NewSubscription) -> Result<Subscription> {
        // Validate at the repository boundary before touching the database.
        input.validate()?;

        let row = sqlx::query_as::<_, SubscriptionRow>(
            r#"
            INSERT INTO subscriptions (url, title, website_url, rsshub_url, use_website, auto_classify, opml_attributes)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING *
            "#,
        )
        .bind(&input.url)
        .bind(&input.title)
        .bind(&input.website_url)
        .bind(&input.rsshub_url)
        .bind(input.use_website)
        .bind(input.auto_classify)
        .bind(&input.opml_attributes)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(e, &format!("creating subscription '{}'", input.url)))?;

        Ok(row.into())
    }

    async fn find_by_id(&self, id: i64) -> Result<Subscription> {
        let row = sqlx::query_as::<_, SubscriptionRow>(
            "SELECT * FROM subscriptions WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Subscription with id {} not found", id)))?;

        Ok(row.into())
    }

    async fn find_all(&self) -> Result<Vec<Subscription>> {
        let rows = sqlx::query_as::<_, SubscriptionRow>(
            "SELECT * FROM subscriptions ORDER BY title",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    async fn update(&self, id: i64, input: UpdateSubscription) -> Result<Subscription> {
        let row = sqlx::query_as::<_, SubscriptionRow>(
            r#"
            UPDATE subscriptions
            SET
                title = COALESCE($2, title),
                website_url = COALESCE($3, website_url),
                use_website = COALESCE($4, use_website),
                rsshub_url = COALESCE($5, rsshub_url),
                updated_at = CURRENT_TIMESTAMP
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(&input.title)
        .bind(&input.website_url)
        .bind(input.use_website)
        .bind(&input.rsshub_url)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Subscription with id {} not found", id)))?;

        Ok(row.into())
    }

    async fn delete(&self, id: i64) -> Result<()> {
        let result = sqlx::query("DELETE FROM subscriptions WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;

        if result.rows_affected() == 0 {
            return Err(AppError::NotFound(format!(
                "Subscription with id {} not found",
                id
            )));
        }

        Ok(())
    }

    async fn exists_by_url(&self, url: &str) -> Result<bool> {
        let exists: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM subscriptions WHERE url = $1",
        )
        .bind(url)
        .fetch_optional(&self.pool)
        .await?;

        Ok(exists.is_some())
    }

    async fn toggle_use_website(&self, id: i64) -> Result<Subscription> {
        let row = sqlx::query_as::<_, SubscriptionRow>(
            r#"
            UPDATE subscriptions
            SET use_website = NOT use_website,
                updated_at = CURRENT_TIMESTAMP
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Subscription with id {} not found", id)))?;

        Ok(row.into())
    }

    async fn toggle_auto_classify(&self, id: i64) -> Result<Subscription> {
        let row = sqlx::query_as::<_, SubscriptionRow>(
            r#"
            UPDATE subscriptions
            SET auto_classify = NOT auto_classify,
                updated_at = CURRENT_TIMESTAMP
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Subscription with id {} not found", id)))?;

        Ok(row.into())
    }
}

/// Map sqlx errors to typed AppError variants.
fn map_sqlx_error(e: sqlx::Error, context: &str) -> AppError {
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

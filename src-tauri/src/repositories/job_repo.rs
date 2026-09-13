//! Durable background-job queue.
//!
//! Enrichment (classification, website Markdown, semantic indexing) used to
//! run inline while a feed refresh was still being awaited, and a failure was
//! an `eprintln!` that nothing retried. Jobs now live in SQLite:
//!
//! * `enqueue` is committed with the article insert (see
//!   `FeedItemRepository::create_with_jobs`), so a crash after commit still
//!   leaves recoverable work.
//! * `claim_batch` leases work in one short transaction — the DB lock is
//!   released before any network call.
//! * `complete`/`fail` only acknowledge the exact attempt that was claimed,
//!   so a worker whose lease expired cannot overwrite a newer result.
//! * `fail` applies bounded exponential backoff and gives up after
//!   `max_attempts`. Previously failing work was retried on every sync and
//!   the newest failures could starve older ones forever; now the queue
//!   orders by `(priority, id)` and a failed job leaves the queue.

use async_trait::async_trait;
use sqlx::{Row, SqlitePool};

use crate::error::{AppError, Result};
use crate::models::job::{Job, JobStats, NewJob};

/// How long a claimed job stays invisible to other workers.
const LEASE_SECONDS: i64 = 300;

/// Base delay for the first retry; doubles per attempt, capped.
const BACKOFF_BASE_SECONDS: i64 = 15;
const BACKOFF_MAX_SECONDS: i64 = 600;

#[async_trait]
pub trait JobRepository: Send + Sync {
    /// Insert jobs, returning the number of rows created.
    async fn enqueue(&self, jobs: &[NewJob]) -> Result<usize>;

    /// Claim up to `limit` due jobs, oldest/highest-priority first. Claimed
    /// rows move to `running` with an incremented attempt count and a lease.
    async fn claim_batch(&self, limit: i64, kinds: &[&str]) -> Result<Vec<Job>>;

    /// Acknowledge a successful attempt. `attempt` must match the claim, so a
    /// worker that overran its lease cannot mark newer work as done.
    async fn complete(&self, id: i64, attempt: i64) -> Result<bool>;

    /// Record a failure: retry with backoff, or mark `failed` when attempts
    /// are exhausted.
    async fn fail(&self, id: i64, attempt: i64, error: &str) -> Result<bool>;

    /// Queue depth / failure summary for the UI.
    async fn stats(&self) -> Result<JobStats>;

    /// Requeue jobs whose lease expired (crash or forced shutdown).
    async fn requeue_expired(&self) -> Result<usize>;

    /// Record a failure that retrying cannot fix and park the job now.
    ///
    /// A 404 article URL and a JavaScript-only page both fail identically on
    /// every attempt; retrying them five times only delays the rest of the
    /// queue and buries real errors under identical messages.
    async fn fail_permanently(&self, id: i64, attempt: i64, error: &str) -> Result<bool>;

    /// Put failed jobs back in the queue (explicit user retry).
    async fn retry_failed(&self) -> Result<usize>;

    /// Drop the oldest terminal rows once more than `keep` are retained.
    async fn prune_finished(&self, keep: i64) -> Result<usize>;
}

pub struct SqliteJobRepository {
    pool: SqlitePool,
}

impl SqliteJobRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

fn row_to_job(row: &sqlx::sqlite::SqliteRow) -> Result<Job> {
    Ok(Job {
        id: row.try_get("id").map_err(AppError::Database)?,
        kind: row.try_get("kind").map_err(AppError::Database)?,
        item_id: row.try_get("item_id").map_err(AppError::Database)?,
        payload: row.try_get("payload").map_err(AppError::Database)?,
        state: row.try_get("state").map_err(AppError::Database)?,
        priority: row.try_get("priority").map_err(AppError::Database)?,
        attempts: row.try_get("attempts").map_err(AppError::Database)?,
        max_attempts: row.try_get("max_attempts").map_err(AppError::Database)?,
        next_attempt_at: row.try_get("next_attempt_at").map_err(AppError::Database)?,
        last_error: row.try_get("last_error").map_err(AppError::Database)?,
        created_at: row.try_get("created_at").map_err(AppError::Database)?,
    })
}

#[async_trait]
impl JobRepository for SqliteJobRepository {
    async fn enqueue(&self, jobs: &[NewJob]) -> Result<usize> {
        if jobs.is_empty() {
            return Ok(0);
        }
        let mut tx = self.pool.begin().await.map_err(AppError::Database)?;
        let mut created = 0usize;
        for job in jobs {
            let result = sqlx::query(
                r#"
                INSERT INTO jobs (kind, item_id, payload, state, priority, max_attempts)
                VALUES ($1, $2, $3, 'queued', $4, $5)
                "#,
            )
            .bind(job.kind.as_str())
            .bind(job.item_id)
            .bind(&job.payload)
            .bind(job.kind.priority())
            .bind(job.kind.max_attempts())
            .execute(&mut *tx)
            .await
            .map_err(AppError::Database)?;
            created += result.rows_affected() as usize;
        }
        tx.commit().await.map_err(AppError::Database)?;
        Ok(created)
    }

    async fn claim_batch(&self, limit: i64, kinds: &[&str]) -> Result<Vec<Job>> {
        // One statement: select due/expired jobs and move them to `running`.
        // RETURNING gives the worker exactly the rows it owns.
        let kind_filter = if kinds.is_empty() {
            String::new()
        } else {
            let list = kinds
                .iter()
                .map(|k| format!("'{}'", k))
                .collect::<Vec<_>>()
                .join(", ");
            format!(" AND kind IN ({list})")
        };
        let sql = format!(
            r#"
            UPDATE jobs
               SET state = 'running',
                   attempts = attempts + 1,
                   lease_until = datetime('now', '+{LEASE_SECONDS} seconds'),
                   updated_at = CURRENT_TIMESTAMP
             WHERE id IN (
                   SELECT id FROM jobs
                    WHERE (
                            (state = 'queued' AND next_attempt_at <= CURRENT_TIMESTAMP)
                         OR (state = 'running' AND lease_until IS NOT NULL
                             AND lease_until < CURRENT_TIMESTAMP)
                          ){kind_filter}
                    ORDER BY priority DESC, id ASC
                    LIMIT $1
             )
            RETURNING *
            "#
        );
        let rows = sqlx::query(&sql)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(AppError::Database)?;
        rows.iter().map(row_to_job).collect()
    }

    async fn complete(&self, id: i64, attempt: i64) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE jobs
               SET state = 'succeeded', lease_until = NULL, updated_at = CURRENT_TIMESTAMP
             WHERE id = $1 AND state = 'running' AND attempts = $2
            "#,
        )
        .bind(id)
        .bind(attempt)
        .execute(&self.pool)
        .await
        .map_err(AppError::Database)?;
        Ok(result.rows_affected() == 1)
    }

    async fn fail(&self, id: i64, attempt: i64, error: &str) -> Result<bool> {
        let backoff = backoff_seconds(attempt);
        let result = sqlx::query(
            r#"
            UPDATE jobs
               SET state = CASE WHEN attempts >= max_attempts THEN 'failed' ELSE 'queued' END,
                   next_attempt_at = datetime('now', $3),
                   last_error = $4,
                   lease_until = NULL,
                   updated_at = CURRENT_TIMESTAMP
             WHERE id = $1 AND state = 'running' AND attempts = $2
            "#,
        )
        .bind(id)
        .bind(attempt)
        .bind(format!("+{backoff} seconds"))
        .bind(truncate_error(error))
        .execute(&self.pool)
        .await
        .map_err(AppError::Database)?;
        Ok(result.rows_affected() == 1)
    }

    async fn fail_permanently(&self, id: i64, attempt: i64, error: &str) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE jobs
               SET state = 'failed',
                   last_error = $3,
                   lease_until = NULL,
                   updated_at = CURRENT_TIMESTAMP
             WHERE id = $1 AND state = 'running' AND attempts = $2
            "#,
        )
        .bind(id)
        .bind(attempt)
        .bind(truncate_error(error))
        .execute(&self.pool)
        .await
        .map_err(AppError::Database)?;
        Ok(result.rows_affected() == 1)
    }

    async fn stats(&self) -> Result<JobStats> {
        let counts: Vec<(String, i64)> =
            sqlx::query_as("SELECT state, COUNT(*) FROM jobs GROUP BY state")
                .fetch_all(&self.pool)
                .await
                .map_err(AppError::Database)?;
        let mut stats = JobStats::default();
        for (state, count) in counts {
            match state.as_str() {
                "queued" => stats.queued = count,
                "running" => stats.running = count,
                "failed" => stats.failed = count,
                "succeeded" => stats.succeeded = count,
                _ => {}
            }
        }
        let errors: Vec<(String,)> = sqlx::query_as(
            "SELECT last_error FROM jobs WHERE last_error IS NOT NULL ORDER BY id DESC LIMIT 5",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(AppError::Database)?;
        stats.recent_errors = errors.into_iter().map(|(e,)| e).collect();

        let queued: Vec<(String, i64)> = sqlx::query_as(
            "SELECT kind, COUNT(*) FROM jobs WHERE state = 'queued' GROUP BY kind",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(AppError::Database)?;
        stats.queued_by_kind = queued.into_iter().collect();
        Ok(stats)
    }

    async fn requeue_expired(&self) -> Result<usize> {
        let result = sqlx::query(
            r#"
            UPDATE jobs
               SET state = 'queued', lease_until = NULL, updated_at = CURRENT_TIMESTAMP
             WHERE state = 'running'
               AND lease_until IS NOT NULL
               AND lease_until < CURRENT_TIMESTAMP
               AND attempts < max_attempts
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(AppError::Database)?;
        Ok(result.rows_affected() as usize)
    }

    async fn retry_failed(&self) -> Result<usize> {
        let result = sqlx::query(
            r#"
            UPDATE jobs
               SET state = 'queued',
                   attempts = 0,
                   next_attempt_at = CURRENT_TIMESTAMP,
                   lease_until = NULL,
                   last_error = NULL,
                   updated_at = CURRENT_TIMESTAMP
             WHERE state = 'failed'
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(AppError::Database)?;
        Ok(result.rows_affected() as usize)
    }

    async fn prune_finished(&self, keep: i64) -> Result<usize> {
        let result = sqlx::query(
            r#"
            DELETE FROM jobs
             WHERE state IN ('succeeded', 'failed')
               AND id NOT IN (
                   SELECT id FROM jobs WHERE state IN ('succeeded', 'failed')
                   ORDER BY id DESC LIMIT $1
               )
            "#,
        )
        .bind(keep)
        .execute(&self.pool)
        .await
        .map_err(AppError::Database)?;
        Ok(result.rows_affected() as usize)
    }
}

/// Exponential backoff with a cap. `attempt` is the 1-based attempt that just
/// failed.
fn backoff_seconds(attempt: i64) -> i64 {
    let shift = attempt.clamp(0, 6) as u32;
    (BACKOFF_BASE_SECONDS.saturating_mul(1 << shift)).min(BACKOFF_MAX_SECONDS)
}

/// Keep error text bounded — a provider can return an entire HTML page.
fn truncate_error(error: &str) -> String {
    const MAX: usize = 500;
    let mut out: String = error.chars().take(MAX).collect();
    if error.chars().count() > MAX {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::migrations::run_migrations;

    async fn pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open in-memory database");
        run_migrations(&pool).await.expect("migrate");
        pool
    }

    #[tokio::test]
    async fn claim_is_exclusive_and_acknowledges_only_the_claimed_attempt() {
        let pool = pool().await;
        let repo = SqliteJobRepository::new(pool.clone());
        repo.enqueue(&[NewJob::classify(Some(1)), NewJob::classify(Some(2))])
            .await
            .expect("enqueue");

        let first = repo.claim_batch(10, &["classify"]).await.expect("claim");
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].attempts, 1);

        // A second worker gets nothing: the rows are leased.
        let second = repo.claim_batch(10, &["classify"]).await.expect("claim");
        assert!(second.is_empty());

        // Stale acknowledgement (attempt 0) is refused.
        assert!(!repo.complete(first[0].id, 0).await.expect("stale ack"));
        assert!(repo.complete(first[0].id, 1).await.expect("ack"));

        // The acknowledged job is gone; the other one is still leased, so
        // only it comes back once the lease expires.
        sqlx::query("UPDATE jobs SET lease_until = datetime('now', '-1 minute') WHERE id = $1")
            .bind(first[1].id)
            .execute(&pool)
            .await
            .expect("expire lease");
        let third = repo.claim_batch(10, &["classify"]).await.expect("claim");
        assert_eq!(third.len(), 1);
        assert_eq!(third[0].id, first[1].id);
    }

    #[tokio::test]
    async fn failure_retries_with_backoff_then_gives_up() {
        let pool = pool().await;
        let repo = SqliteJobRepository::new(pool.clone());
        repo.enqueue(&[NewJob::classify(Some(1))]).await.expect("enqueue");

        let claimed = repo.claim_batch(1, &["classify"]).await.expect("claim");
        let job = &claimed[0];
        repo.fail(job.id, job.attempts, "model unavailable")
            .await
            .expect("fail");

        // Backoff keeps it out of the next claim...
        assert!(repo
            .claim_batch(10, &["classify"])
            .await
            .expect("claim")
            .is_empty());

        // ...until its next_attempt_at passes. Keep failing until the attempt
        // budget (Classify allows 3) is spent.
        advance(&pool).await;
        let claimed = repo.claim_batch(1, &["classify"]).await.expect("claim");
        assert_eq!(claimed[0].attempts, 2);
        repo.fail(claimed[0].id, claimed[0].attempts, "still broken")
            .await
            .expect("fail");

        advance(&pool).await;
        let claimed = repo.claim_batch(1, &["classify"]).await.expect("claim");
        assert_eq!(claimed[0].attempts, 3);
        repo.fail(claimed[0].id, claimed[0].attempts, "still broken")
            .await
            .expect("fail");

        // Exhausted: it is parked as failed and leaves the queue.
        let stats = repo.stats().await.expect("stats");
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.queued, 0);
        assert!(repo
            .claim_batch(10, &["classify"])
            .await
            .expect("claim")
            .is_empty());

        assert_eq!(repo.retry_failed().await.expect("retry"), 1);
        assert_eq!(repo.stats().await.expect("stats").queued, 1);
    }

    async fn advance(pool: &SqlitePool) {
        sqlx::query("UPDATE jobs SET next_attempt_at = datetime('now', '-1 second')")
            .execute(pool)
            .await
            .expect("advance clock");
    }

    #[tokio::test]
    async fn permanent_failures_park_immediately() {
        let pool = pool().await;
        let repo = SqliteJobRepository::new(pool.clone());
        repo.enqueue(&[NewJob::website_markdown(None, "https://x/404".into())])
            .await
            .expect("enqueue");
        let claimed = repo.claim_batch(1, &["website_markdown"]).await.expect("claim");

        assert!(
            repo.fail_permanently(claimed[0].id, claimed[0].attempts, "HTTP status: 404")
                .await
                .expect("park")
        );

        let stats = repo.stats().await.expect("stats");
        assert_eq!(stats.failed, 1, "parked on the first attempt: {stats:?}");
        assert_eq!(stats.queued, 0);
        assert!(
            repo.claim_batch(10, &["website_markdown"])
                .await
                .expect("claim")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn expired_leases_are_recovered() {
        let pool = pool().await;
        let repo = SqliteJobRepository::new(pool.clone());
        repo.enqueue(&[NewJob::chroma_upsert(Some(7))])
            .await
            .expect("enqueue");
        repo.claim_batch(1, &["chroma_upsert"])
            .await
            .expect("claim");

        sqlx::query("UPDATE jobs SET lease_until = datetime('now', '-10 minutes')")
            .execute(&pool)
            .await
            .expect("expire lease");

        assert_eq!(repo.requeue_expired().await.expect("requeue"), 1);
        let stats = repo.stats().await.expect("stats");
        assert_eq!(stats.queued, 1);
        assert_eq!(stats.running, 0);
    }

    #[test]
    fn backoff_is_bounded() {
        assert_eq!(backoff_seconds(0), 15);
        assert_eq!(backoff_seconds(1), 30);
        assert_eq!(backoff_seconds(4), 240);
        assert_eq!(backoff_seconds(20), BACKOFF_MAX_SECONDS);
    }
}

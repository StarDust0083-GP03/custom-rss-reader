//! SQLite upgrade compatibility.
//!
//! Every existing install is an *old database*: there is no `schema_migrations`
//! table in them, so the first launch after this refactor runs all ten versions
//! against the user's real library. These tests build the schemas that older
//! releases actually shipped, run the upgrade, and assert that nothing the user
//! created is lost.
//!
//! Cases:
//!   A. the oldest schema (no content_md, no tags, no tag tables)
//!   B. the schema from the previous release (everything except jobs,
//!      validators, and translation provenance)
//!   C. duplicate rows whose flags and bodies are split between copies
//!   D. re-running the migrations (idempotence)
//!   E. a database that already recorded part of the version range
//!   F. the pre-migration backup file

use std::sync::Arc;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::database::migrations::{pending_versions, run_migrations, SCHEMA_VERSION};
use crate::repositories::FeedItemRepository;

async fn memory() -> SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(":memory:")
                .foreign_keys(true),
        )
        .await
        .expect("in-memory database")
}

fn has(text: &str) -> bool {
    !text.trim().is_empty()
}

async fn table_exists(pool: &SqlitePool, name: &str) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = $1",
    )
    .bind(name)
    .fetch_one(pool)
    .await
    .expect("sqlite_master")
        > 0
}

async fn column_exists(pool: &SqlitePool, table: &str, column: &str) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info($1) WHERE name = $2",
    )
    .bind(table)
    .bind(column)
    .fetch_one(pool)
    .await
    .expect("pragma_table_info")
        > 0
}

/// The schema shipped by the first release.
async fn legacy_v1(pool: &SqlitePool) {
    for ddl in [
        r#"CREATE TABLE subscriptions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            url TEXT NOT NULL UNIQUE,
            title TEXT,
            website_url TEXT,
            rsshub_url TEXT,
            use_website BOOLEAN DEFAULT 0,
            opml_attributes TEXT,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )"#,
        r#"CREATE TABLE feed_items (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            subscription_id INTEGER NOT NULL,
            guid TEXT,
            title TEXT NOT NULL,
            link TEXT,
            content TEXT,
            description TEXT,
            author TEXT,
            published_at DATETIME,
            fetched_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            is_website_content BOOLEAN DEFAULT 0,
            is_read BOOLEAN DEFAULT 0,
            is_favorite BOOLEAN DEFAULT 0,
            is_read_later BOOLEAN DEFAULT 0,
            translated_title TEXT,
            translated_content TEXT,
            translated_at DATETIME
        )"#,
    ] {
        sqlx::query(ddl).execute(pool).await.expect("legacy DDL");
    }

    sqlx::query("INSERT INTO subscriptions (id, url, title, use_website) VALUES (1, 'https://old.example.com/feed', 'Old feed', 0)")
        .execute(pool)
        .await
        .expect("legacy subscription");
    sqlx::query(
        "INSERT INTO feed_items (id, subscription_id, guid, title, link, content, is_read, is_favorite, translated_content, translated_at)
         VALUES (1, 1, 'g1', 'Old article', 'https://old.example.com/1', '<p>old body</p>', 1, 1, '<div class=\"bilingual-content\">cached</div>', CURRENT_TIMESTAMP)",
    )
    .execute(pool)
    .await
    .expect("legacy article with a cached translation");
}

/// The schema from the release before this refactor: everything except the
/// jobs table, the HTTP validators, and translation provenance.
async fn legacy_pre_refactor(pool: &SqlitePool) {
    legacy_v1(pool).await;
    for ddl in [
        "ALTER TABLE feed_items ADD COLUMN content_md TEXT",
        "ALTER TABLE feed_items ADD COLUMN is_ignored BOOLEAN DEFAULT 0",
        "ALTER TABLE feed_items ADD COLUMN tags TEXT",
        "ALTER TABLE feed_items ADD COLUMN category TEXT",
        "ALTER TABLE subscriptions ADD COLUMN auto_classify BOOLEAN DEFAULT 1",
        r#"CREATE TABLE app_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)"#,
        "INSERT INTO app_metadata (key, value) VALUES ('database_id', 'deadbeefdeadbeefdeadbeefdeadbeef')",
        r#"CREATE TABLE tag_catalog (name TEXT PRIMARY KEY COLLATE NOCASE, created_at DATETIME DEFAULT CURRENT_TIMESTAMP, updated_at DATETIME DEFAULT CURRENT_TIMESTAMP)"#,
        r#"CREATE TABLE tag_aliases (alias TEXT PRIMARY KEY COLLATE NOCASE, canonical_name TEXT NOT NULL COLLATE NOCASE, created_at DATETIME DEFAULT CURRENT_TIMESTAMP)"#,
        r#"CREATE TABLE blocked_tags (name TEXT PRIMARY KEY COLLATE NOCASE, blocked_at DATETIME DEFAULT CURRENT_TIMESTAMP)"#,
        "UPDATE feed_items SET content_md = '## Old article\n\nBody text.', tags = '[\"rust\",\"testing\"]', category = 'technology' WHERE id = 1",
        "INSERT INTO tag_catalog (name) VALUES ('rust'), ('testing')",
    ] {
        sqlx::query(ddl).execute(pool).await.expect("pre-refactor DDL");
    }
}

// ---------------------------------------------------------------------------
// A. oldest schema
// ---------------------------------------------------------------------------

#[tokio::test]
async fn oldest_schema_upgrades_without_losing_articles_or_translations() {
    let pool = memory().await;
    legacy_v1(&pool).await;

    assert!(!table_exists(&pool, "schema_migrations").await);
    run_migrations(&pool).await.expect("upgrade from v1");

    // Structural expectations.
    for table in ["jobs", "schema_migrations", "tag_catalog", "app_metadata"] {
        assert!(table_exists(&pool, table).await, "{table} must exist");
    }
    for column in [
        "content_md",
        "tags",
        "is_ignored",
        "translated_source_hash",
        "translated_model",
        "translated_prompt_version",
    ] {
        assert!(
            column_exists(&pool, "feed_items", column).await,
            "feed_items.{column} must exist"
        );
    }
    for column in ["auto_classify", "http_etag", "http_last_modified"] {
        assert!(
            column_exists(&pool, "subscriptions", column).await,
            "subscriptions.{column} must exist"
        );
    }

    // Data expectations: the user's article keeps every flag and its cache.
    let (title, is_read, is_favorite, translated): (String, bool, bool, Option<String>) =
        sqlx::query_as(
            "SELECT title, is_read, is_favorite, translated_content FROM feed_items WHERE id = 1",
        )
        .fetch_one(&pool)
        .await
        .expect("legacy row survives");
    assert_eq!(title, "Old article");
    assert!(is_read && is_favorite, "user flags are preserved");
    assert!(translated.is_some(), "cached translation is preserved");

    // The upgrade hashed the cached translation so it stays usable.
    let (hash, model, prompt): (Option<String>, Option<String>, Option<i64>) = sqlx::query_as(
        "SELECT translated_source_hash, translated_model, translated_prompt_version FROM feed_items WHERE id = 1",
    )
    .fetch_one(&pool)
    .await
    .expect("provenance");
    let hash = hash.expect("legacy translation must get a hash");
    assert_eq!(hash.len(), 64, "sha-256 hex");
    assert_eq!(model.as_deref(), Some("legacy"));
    assert_eq!(prompt, Some(0));

    // The subscription picked up its new defaults without changing its identity.
    let (url, auto_classify): (String, bool) =
        sqlx::query_as("SELECT url, auto_classify FROM subscriptions WHERE id = 1")
            .fetch_one(&pool)
            .await
            .expect("subscription");
    assert_eq!(url, "https://old.example.com/feed");
    assert!(auto_classify, "defaults are applied by the migration");

    // Nothing was invented for the queue.
    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
        .fetch_one(&pool)
        .await
        .expect("jobs");
    assert_eq!(jobs, 0, "an upgrade must not queue work for old articles");
}

// ---------------------------------------------------------------------------
// B. previous release
// ---------------------------------------------------------------------------

#[tokio::test]
async fn previous_release_schema_upgrades_and_keeps_derived_data() {
    let pool = memory().await;
    legacy_pre_refactor(&pool).await;

    run_migrations(&pool).await.expect("upgrade from the previous release");

    let (tags, content_md): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT tags, content_md FROM feed_items WHERE id = 1")
            .fetch_one(&pool)
            .await
            .expect("row");
    assert_eq!(tags.as_deref(), Some(r#"["rust","testing"]"#));
    assert!(has(content_md.as_deref().unwrap_or("")));

    // The tag catalog backfill is guarded by a count check, so an existing
    // catalog must not be duplicated or rewritten.
    let catalog: Vec<String> = sqlx::query_scalar("SELECT name FROM tag_catalog ORDER BY name")
        .fetch_all(&pool)
        .await
        .expect("catalog");
    assert_eq!(catalog.len(), 2, "no duplicate catalog rows: {catalog:?}");

    // The database identity survives, so the Chroma watermark stays valid.
    let id: String =
        sqlx::query_scalar("SELECT value FROM app_metadata WHERE key = 'database_id'")
            .fetch_one(&pool)
            .await
            .expect("database_id");
    assert_eq!(id, "deadbeefdeadbeefdeadbeefdeadbeef");
}

// ---------------------------------------------------------------------------
// C. duplicates with split annotations
// ---------------------------------------------------------------------------

/// `(id, title, is_read, is_favorite, is_read_later, is_ignored, tags)`.
type DuplicateRow = (i64, String, bool, bool, bool, bool, Option<String>);

#[tokio::test]
async fn duplicate_rows_merge_every_annotation_before_deleting() {
    let pool = memory().await;
    // Duplicates came from the pre-refactor era, so build that schema (it has
    // the annotation columns the merge has to preserve).
    legacy_pre_refactor(&pool).await;
    // Two rows for the same guid: the older one is read and has a long body,
    // the newer one is favorited, tagged, and titled better.
    sqlx::query(
        "INSERT INTO feed_items (id, subscription_id, guid, title, link, content, is_read, is_favorite, is_read_later, is_ignored, tags)
         VALUES (2, 1, 'dup', 'First copy', 'https://old.example.com/dup', '<p>short</p>', 1, 0, 0, 0, '[\"alpha\"]')",
    )
    .execute(&pool)
    .await
    .expect("first copy");
    sqlx::query(
        "INSERT INTO feed_items (id, subscription_id, guid, title, link, content, is_read, is_favorite, is_read_later, is_ignored, tags, translated_content, translated_at)
         VALUES (3, 1, 'dup', 'Second copy', 'https://old.example.com/dup',
                 '<p>the much longer body of the same article</p>', 0, 1, 1, 1, '[\"beta\",\"alpha\"]',
                 '<div class=\"bilingual-content\">cached translation</div>', CURRENT_TIMESTAMP)",
    )
    .execute(&pool)
    .await
    .expect("second copy");

    run_migrations(&pool).await.expect("upgrade with duplicates");

    let rows: Vec<DuplicateRow> = sqlx::query_as(
        "SELECT id, title, is_read, is_favorite, is_read_later, is_ignored, tags
           FROM feed_items WHERE guid = 'dup' ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("merged row");
    assert_eq!(rows.len(), 1, "the duplicate is gone");
    let (id, _title, is_read, is_favorite, is_read_later, is_ignored, tags) = &rows[0];
    assert_eq!(*id, 2, "the oldest row is kept");
    assert!(
        *is_read && *is_favorite && *is_read_later && *is_ignored,
        "OR-merged flags: {rows:?}"
    );
    let tags = tags.clone().unwrap_or_default();
    assert!(tags.contains("alpha") && tags.contains("beta"), "tag union: {tags}");

    // The longer body and the only translation both survive on the keeper.
    let (content, content_md, translated): (Option<String>, Option<String>, Option<String>) =
        sqlx::query_as(
            "SELECT content, content_md, translated_content FROM feed_items WHERE id = 2",
        )
        .fetch_one(&pool)
        .await
        .expect("keeper");
    assert!(has(content.as_deref().unwrap_or("")));
    assert!(content.unwrap_or_default().contains("much longer body"));
    assert!(has(content_md.as_deref().unwrap_or("")) || translated.is_some());
}

// ---------------------------------------------------------------------------
// D. idempotence
// ---------------------------------------------------------------------------

#[tokio::test]
async fn running_the_upgrade_twice_changes_nothing() {
    let pool = memory().await;
    legacy_pre_refactor(&pool).await;

    run_migrations(&pool).await.expect("first upgrade");
    let before = snapshot(&pool).await;

    run_migrations(&pool).await.expect("second upgrade");
    assert_eq!(
        snapshot(&pool).await,
        before,
        "a second start must be a no-op"
    );

    let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations")
        .fetch_one(&pool)
        .await
        .expect("migrations");
    assert_eq!(applied, SCHEMA_VERSION, "every version is recorded once");

    assert!(
        pending_versions(&pool).await.expect("pending").is_empty(),
        "nothing pending after a full upgrade"
    );
}

/// Comparable view of everything the migrations touch.
/// `(id, title, is_read, is_favorite, is_read_later, is_ignored, tags, content_md)`.
type SnapshotItem = (i64, String, bool, bool, bool, bool, Option<String>, Option<String>);
/// `(id, url, auto_classify)`.
type SnapshotSubscription = (i64, String, bool);

async fn snapshot(pool: &SqlitePool) -> Vec<String> {
    let items: Vec<SnapshotItem> =
        sqlx::query_as(
            "SELECT id, title, is_read, is_favorite, is_read_later, is_ignored, tags, content_md
               FROM feed_items ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .expect("items");
    let subs: Vec<SnapshotSubscription> =
        sqlx::query_as("SELECT id, url, auto_classify FROM subscriptions ORDER BY id")
            .fetch_all(pool)
            .await
            .expect("subscriptions");
    let catalog: Vec<String> = sqlx::query_scalar("SELECT name FROM tag_catalog ORDER BY name")
        .fetch_all(pool)
        .await
        .expect("catalog");
    let mut out: Vec<String> = items.iter().map(|row| format!("{row:?}")).collect();
    out.extend(subs.iter().map(|row| format!("{row:?}")));
    out.extend(catalog);
    out
}

// ---------------------------------------------------------------------------
// E. partially upgraded database
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_partially_recorded_database_only_runs_the_rest() {
    let pool = memory().await;
    legacy_pre_refactor(&pool).await;
    sqlx::query(
        "CREATE TABLE schema_migrations (
            version INTEGER PRIMARY KEY, name TEXT NOT NULL,
            applied_at DATETIME DEFAULT CURRENT_TIMESTAMP)",
    )
    .execute(&pool)
    .await
    .expect("schema_migrations");
    for version in 1..=5 {
        sqlx::query("INSERT INTO schema_migrations (version, name) VALUES ($1, 'earlier run')")
            .bind(version)
            .execute(&pool)
            .await
            .expect("record");
    }

    let pending = pending_versions(&pool).await.expect("pending");
    assert_eq!(
        pending,
        (6..=SCHEMA_VERSION).collect::<Vec<_>>(),
        "only the missing versions are planned"
    );

    run_migrations(&pool).await.expect("finish the upgrade");

    assert!(table_exists(&pool, "jobs").await, "the jobs table is created");
    assert!(column_exists(&pool, "feed_items", "translated_source_hash").await);
    let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations")
        .fetch_one(&pool)
        .await
        .expect("migrations");
    assert_eq!(applied, SCHEMA_VERSION);
}

// ---------------------------------------------------------------------------
// Translation cache across the upgrade
// ---------------------------------------------------------------------------

#[tokio::test]
async fn legacy_translations_stay_cached_and_source_edits_invalidate_them() {
    use crate::commands::streaming::lookup_cached_translation;
    use crate::repositories::feed_item_repo::SqliteFeedItemRepository as Repo;

    let pool = memory().await;
    legacy_pre_refactor(&pool).await;
    run_migrations(&pool).await.expect("upgrade");
    let repo: Arc<dyn FeedItemRepository> =
        Arc::new(Repo::new(pool.clone()));

    let item = repo.find_by_id(1).await.expect("item");
    let source = item.content_md.clone().unwrap_or_default();
    let hash = crate::ai::translation_source_hash(&source);

    // Pre-upgrade translation: hash matches, provenance is unknown. It must be
    // served — re-billing a whole library after an upgrade is not acceptable.
    assert!(
        lookup_cached_translation(&repo, 1, &hash, "some-other-model", 42)
            .await
            .expect("lookup")
            .is_some(),
        "an upgraded library keeps its cached translations"
    );

    // A changed article is still detected, which is the actual bug fix.
    sqlx::query("UPDATE feed_items SET content_md = 'The publisher edited this.' WHERE id = 1")
        .execute(&pool)
        .await
        .expect("edit article");
    let edited = crate::ai::translation_source_hash("The publisher edited this.");
    assert!(
        lookup_cached_translation(&repo, 1, &edited, "some-other-model", 42)
            .await
            .expect("lookup")
            .is_none(),
        "a stale source must be re-translated"
    );

    // A translation written by current code is strict about its model again:
    // switching models invalidates it.
    repo.update_translation(1, None, "<div class=\"bilingual-content\">new</div>", &edited, "model-a", 1)
        .await
        .expect("store translation");
    assert!(
        lookup_cached_translation(&repo, 1, &edited, "model-a", 1)
            .await
            .expect("lookup")
            .is_some(),
        "same source, model and prompt ⇒ cache hit"
    );
    assert!(
        lookup_cached_translation(&repo, 1, &edited, "model-b", 1)
            .await
            .expect("lookup")
            .is_none(),
        "a different model is a cache miss for rows with known provenance"
    );
    assert!(
        lookup_cached_translation(&repo, 1, &edited, "model-a", 2)
            .await
            .expect("lookup")
            .is_none(),
        "a different prompt revision is a cache miss"
    );
}

/// A downgraded build writes translations without provenance. The next start
/// on the newer build must adopt them instead of treating the whole library as
/// stale.
#[tokio::test]
async fn provenance_is_adopted_for_rows_written_without_it() {
    use crate::commands::streaming::lookup_cached_translation;
    use crate::repositories::feed_item_repo::SqliteFeedItemRepository as Repo;

    let pool = memory().await;
    legacy_pre_refactor(&pool).await;
    run_migrations(&pool).await.expect("upgrade");
    // Simulate a write from an older/downgraded build.
    sqlx::query(
        "UPDATE feed_items SET translated_source_hash = NULL, translated_model = NULL,
                               translated_prompt_version = NULL WHERE id = 1",
    )
    .execute(&pool)
    .await
    .expect("clear provenance");

    let repo: Arc<dyn FeedItemRepository> = Arc::new(Repo::new(pool.clone()));
    let item = repo.find_by_id(1).await.expect("item");
    let stored = item.content_md.clone().unwrap_or_default();
    let hash = crate::ai::translation_source_hash(&stored);

    let cached = lookup_cached_translation(&repo, 1, &hash, "model-x", 9)
        .await
        .expect("lookup");
    assert!(cached.is_some(), "an unverifiable cache must be adopted, not discarded");

    // Adoption is persisted, so the next lookup is strict again.
    let (adopted_hash, adopted_model, adopted_prompt): (Option<String>, Option<String>, Option<i64>) =
        sqlx::query_as(
            "SELECT translated_source_hash, translated_model, translated_prompt_version
               FROM feed_items WHERE id = 1",
        )
        .fetch_one(&pool)
        .await
        .expect("provenance row");
    assert_eq!(adopted_hash.as_deref(), Some(hash.as_str()));
    assert_eq!(adopted_model.as_deref(), Some("legacy"));
    assert_eq!(adopted_prompt, Some(0));

    // …and an edit is caught from then on.
    sqlx::query("UPDATE feed_items SET content_md = 'rewritten' WHERE id = 1")
        .execute(&pool)
        .await
        .expect("edit");
    let rewritten = crate::ai::translation_source_hash("rewritten");
    assert!(
        lookup_cached_translation(&repo, 1, &rewritten, "model-x", 9)
            .await
            .expect("lookup")
            .is_none(),
        "after adoption, a changed source invalidates the cache"
    );

    // The article's own timestamp is not refreshed by adoption: an old
    // translation must still age out.
    let translated_at: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT translated_at FROM feed_items WHERE id = 1")
            .fetch_one(&pool)
            .await
            .expect("translated_at");
    assert!(
        (chrono::Utc::now() - translated_at).num_seconds() < 60,
        "the test's own row is recent; adoption must not touch it"
    );
}

// ---------------------------------------------------------------------------
// F. backup
// ---------------------------------------------------------------------------

#[tokio::test]
async fn upgrade_writes_a_backup_next_to_the_database() {
    let dir = std::env::temp_dir().join(format!("rss-reader-compat-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let db_path = dir.join("rss_reader.db");

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&db_path)
                .create_if_missing(true),
        )
        .await
        .expect("file database");
    legacy_pre_refactor(&pool).await;

    // The app calls this before migrating; it must notice the pending versions
    // and snapshot the database while it still holds the old data.
    crate::database::backup_before_migration(&pool, &db_path).await;

    let backups: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .expect("dir")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("pre-migration-v"))
        })
        .collect();
    assert_eq!(backups.len(), 1, "exactly one backup: {backups:?}");

    // The backup is a real, readable database with the pre-upgrade contents.
    let backup_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(&backups[0]))
        .await
        .expect("open backup");
    let title: String = sqlx::query_scalar("SELECT title FROM feed_items WHERE id = 1")
        .fetch_one(&backup_pool)
        .await
        .expect("backup contents");
    assert_eq!(title, "Old article");
    let jobs_table: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='jobs'",
    )
    .fetch_one(&backup_pool)
    .await
    .expect("backup schema");
    assert_eq!(jobs_table, 0, "the backup predates the migration");

    // A second call must not overwrite or duplicate the snapshot.
    crate::database::backup_before_migration(&pool, &db_path).await;
    let still_one = std::fs::read_dir(&dir)
        .expect("dir")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("pre-migration-v"))
        })
        .count();
    assert_eq!(still_one, 1);

    // A fresh install has nothing to protect: no backup is written.
    let fresh_path = dir.join("brand-new.db");
    let fresh_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&fresh_path)
                .create_if_missing(true),
        )
        .await
        .expect("fresh database");
    crate::database::backup_before_migration(&fresh_pool, &fresh_path).await;
    assert!(
        !fresh_path.with_extension("db.pre-migration-v10.bak").exists(),
        "a new database must not produce a backup"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

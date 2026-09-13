use sqlx::{Executor, SqliteConnection, SqlitePool};

use crate::error::{AppError, Result};
use crate::models::tag::{normalize_tag, MAX_TAGS_PER_ITEM};

/// The newest schema version this build knows how to produce.
pub const SCHEMA_VERSION: i64 = 14;

/// A migration step future that borrows the connection for its own lifetime.
///
/// Steps are idempotent and recorded in `schema_migrations`, so each runs at
/// most once and the applied set is auditable. Adding a step means appending
/// a version here — never editing an already-shipped one.
type MigrationFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>>;

fn step(version: i64, name: &'static str) -> (i64, &'static str) {
    (version, name)
}

/// Run all pending database migrations.
///
/// Creates tables from scratch if they don't exist, otherwise adds any
/// missing columns incrementally. Each numbered step is recorded once, so a
/// later release can add versions without re-running (or re-deduplicating)
/// older ones.
pub async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    let mut tx = pool.begin().await.map_err(AppError::Database)?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(&mut *tx)
    .await
    .map_err(AppError::Database)?;

    // v1 — base tables (legacy databases already have them).
    migrate_once(&mut tx, step(1, "initial schema"), |conn| {
        Box::pin(async move {
            let tables_exist = sqlx::query(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='feed_items'",
            )
            .fetch_optional(&mut *conn)
            .await
            .map_err(AppError::Database)?
            .is_some();
            if !tables_exist {
                create_tables(conn).await?;
            }
            Ok(())
        })
    })
    .await?;

    // v2 — columns added after the first release.
    migrate_once(&mut tx, step(2, "annotation and website columns"), |conn| {
        Box::pin(async move { add_missing_columns(conn).await })
    })
    .await?;

    // v3 — merge duplicate (subscription_id, guid) rows WITHOUT losing user
    // annotations, then let the unique index enforce dedup going forward.
    migrate_once(&mut tx, step(3, "merge duplicate feed items"), |conn| {
        Box::pin(async move { merge_duplicate_feed_items(conn).await })
    })
    .await?;

    // v4..v8 — tag tables, app metadata, catalog backfill, indexes.
    migrate_once(&mut tx, step(4, "tag tables"), |conn| {
        Box::pin(async move { create_tag_tables(conn).await })
    })
    .await?;
    migrate_once(&mut tx, step(5, "app metadata"), |conn| {
        Box::pin(async move { create_app_metadata(conn).await })
    })
    .await?;
    migrate_once(&mut tx, step(6, "tag catalog backfill"), |conn| {
        Box::pin(async move { backfill_tag_catalog(conn).await })
    })
    .await?;
    migrate_once(&mut tx, step(7, "indexes"), |conn| {
        Box::pin(async move { create_indexes(conn).await })
    })
    .await?;

    // v8 — durable background jobs.
    migrate_once(&mut tx, step(8, "background job queue"), |conn| {
        Box::pin(async move { create_jobs_table(conn).await })
    })
    .await?;

    // v9 — translation validity (which source/model produced a cached result)
    // and HTTP conditional-GET validators.
    migrate_once(
        &mut tx,
        step(9, "translation validity and HTTP validators"),
        |conn| Box::pin(async move { add_validity_columns(conn).await }),
    )
    .await?;

    // v10 — give the translations that predate validity tracking a source hash
    // so they stay usable. Without this every cached translation in an
    // upgraded library would look stale and be re-billed on the next click.
    migrate_once(
        &mut tx,
        step(10, "backfill translation source hashes"),
        |conn| Box::pin(async move { backfill_translation_hashes(conn).await }),
    )
    .await?;

    // v11 — tag adoption and raw tag retention. A vocabulary entry can stay
    // known without being a displayed category, and every article keeps the
    // names classification produced so changing adoption re-resolves instead
    // of destroying data.
    migrate_once(&mut tx, step(11, "tag adoption and raw tags"), |conn| {
        Box::pin(async move { add_tag_adoption(conn).await })
    })
    .await?;

    // v12 — tag dictionary: an LLM-written definition per tag plus the local
    // encoder vector of that definition, so grouping and matching work from
    // real semantics instead of bare names.
    migrate_once(&mut tx, step(12, "tag dictionary"), |conn| {
        Box::pin(async move { create_tag_dictionary(conn).await })
    })
    .await?;

    // v13 — the topic navigation layer. Tags stay fine-grained; a stable
    // catalog of named topics carries the ≤50 entry points, and each tag maps
    // to at most one topic. Community detection stays a read-only view over
    // the raw tags, so it never writes here.
    migrate_once(&mut tx, step(13, "topic categories"), |conn| {
        Box::pin(async move {
            create_topic_tables(conn).await?;
            add_dictionary_suggestion_columns(conn).await
        })
    })
    .await?;

    // v14 — the category concept is gone. A tag is a tag; what a reader browses
    // by is a topic (`topic_categories`), and the vocabulary no longer needs a
    // "display this as an article category" flag or a free-text category per
    // article from the classifier. Dropping the columns rather than ignoring
    // them keeps one meaning per name in the schema.
    migrate_once(&mut tx, step(14, "remove the category concept"), |conn| {
        Box::pin(async move { remove_category_columns(conn).await })
    })
    .await?;

    tx.commit().await.map_err(AppError::Database)?;

    Ok(())
}

/// Versions that this build would apply to the given database. Used by
/// `init_database` to decide whether a pre-migration backup is warranted.
pub async fn pending_versions(pool: &SqlitePool) -> Result<Vec<i64>> {
    let applied: Vec<i64> = sqlx::query_scalar("SELECT version FROM schema_migrations")
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    Ok((1..=SCHEMA_VERSION)
        .filter(|v| !applied.contains(v))
        .collect())
}

/// Apply one numbered step unless it has already been recorded.
async fn migrate_once<F>(conn: &mut SqliteConnection, step: (i64, &'static str), f: F) -> Result<()>
where
    F: for<'a> FnOnce(&'a mut SqliteConnection) -> MigrationFuture<'a>,
{
    let (version, name) = step;
    let applied: Option<i64> =
        sqlx::query_scalar("SELECT version FROM schema_migrations WHERE version = $1")
            .bind(version)
            .fetch_optional(&mut *conn)
            .await
            .map_err(AppError::Database)?;
    if applied.is_some() {
        return Ok(());
    }

    f(conn).await?;

    sqlx::query("INSERT INTO schema_migrations (version, name) VALUES ($1, $2)")
        .bind(version)
        .bind(name)
        .execute(&mut *conn)
        .await
        .map_err(AppError::Database)?;
    Ok(())
}

async fn create_tables(conn: &mut SqliteConnection) -> Result<()> {
    conn.execute(
        r#"
        CREATE TABLE subscriptions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            url TEXT NOT NULL UNIQUE,
            title TEXT,
            website_url TEXT,
            rsshub_url TEXT,
            use_website BOOLEAN DEFAULT 0,
            auto_classify BOOLEAN DEFAULT 1,
            opml_attributes TEXT,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .await
    .map_err(AppError::Database)?;

    conn.execute(
        r#"
        CREATE TABLE feed_items (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            subscription_id INTEGER NOT NULL,
            guid TEXT,
            title TEXT NOT NULL,
            link TEXT,
            content TEXT,
            content_md TEXT,
            description TEXT,
            author TEXT,
            published_at DATETIME,
            fetched_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            is_website_content BOOLEAN DEFAULT 0,
            is_read BOOLEAN DEFAULT 0,
            is_favorite BOOLEAN DEFAULT 0,
            is_read_later BOOLEAN DEFAULT 0,
            is_ignored BOOLEAN DEFAULT 0,
            tags TEXT,
            category TEXT,
            translated_title TEXT,
            translated_content TEXT,
            translated_at DATETIME,
            FOREIGN KEY (subscription_id) REFERENCES subscriptions(id) ON DELETE CASCADE
        )
        "#,
    )
    .await
    .map_err(AppError::Database)?;

    Ok(())
}

async fn create_app_metadata(conn: &mut SqliteConnection) -> Result<()> {
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS app_metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )
        "#,
    )
    .await
    .map_err(AppError::Database)?;

    // The id survives normal migrations but changes when the SQLite database
    // is replaced. Chroma sync uses it to invalidate an old watermark.
    conn.execute(
        "INSERT OR IGNORE INTO app_metadata (key, value) VALUES ('database_id', lower(hex(randomblob(16))))",
    )
    .await
    .map_err(AppError::Database)?;

    Ok(())
}

async fn create_tag_tables(conn: &mut SqliteConnection) -> Result<()> {
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS tag_catalog (
            name TEXT PRIMARY KEY COLLATE NOCASE,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .await
    .map_err(AppError::Database)?;

    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS tag_aliases (
            alias TEXT PRIMARY KEY COLLATE NOCASE,
            canonical_name TEXT NOT NULL COLLATE NOCASE,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .await
    .map_err(AppError::Database)?;

    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS blocked_tags (
            name TEXT PRIMARY KEY COLLATE NOCASE,
            blocked_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .await
    .map_err(AppError::Database)?;

    Ok(())
}

/// Seed the catalog when upgrading a database that predates controlled tags.
/// The catalog count guard keeps subsequent startups cheap.
async fn backfill_tag_catalog(conn: &mut SqliteConnection) -> Result<()> {
    let catalog_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tag_catalog")
        .fetch_one(&mut *conn)
        .await
        .map_err(AppError::Database)?;
    if catalog_count > 0 {
        return Ok(());
    }

    let rows: Vec<(i64, Option<String>)> = sqlx::query_as(
        "SELECT id, tags FROM feed_items WHERE tags IS NOT NULL AND json_valid(tags)",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(AppError::Database)?;

    for (item_id, raw_tags) in rows {
        let Some(raw_tags) = raw_tags else { continue };
        let Ok(tags) = serde_json::from_str::<Vec<String>>(&raw_tags) else {
            continue;
        };
        let mut normalized = Vec::new();
        for raw in tags {
            let Some(tag) = normalize_tag(&raw) else {
                continue;
            };
            if normalized.len() >= MAX_TAGS_PER_ITEM {
                break;
            }
            if !normalized.contains(&tag) {
                sqlx::query("INSERT OR IGNORE INTO tag_catalog (name) VALUES ($1)")
                    .bind(&tag)
                    .execute(&mut *conn)
                    .await
                    .map_err(AppError::Database)?;
                normalized.push(tag);
            }
        }

        let normalized_json = serde_json::to_string(&normalized)
            .map_err(|e| AppError::Internal(format!("Failed to serialize tags: {}", e)))?;
        if normalized_json != raw_tags {
            sqlx::query("UPDATE feed_items SET tags = $2 WHERE id = $1")
                .bind(item_id)
                .bind(normalized_json)
                .execute(&mut *conn)
                .await
                .map_err(AppError::Database)?;
        }
    }

    Ok(())
}

/// Add a column if it does not exist yet. Migration errors are returned to
/// the caller: starting with a partially migrated schema is less safe than
/// stopping and asking for the database error to be repaired.
async fn ensure_column(
    conn: &mut SqliteConnection,
    table: &str,
    column: &str,
    ddl: &str,
) -> Result<()> {
    let exists: Option<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info($1) WHERE name = $2 LIMIT 1")
            .bind(table)
            .bind(column)
            .fetch_optional(&mut *conn)
            .await
            .map_err(AppError::Database)?;

    if exists.is_none() {
        sqlx::query(ddl)
            .execute(&mut *conn)
            .await
            .map_err(AppError::Database)
            .map(|_| ())?;
    }
    Ok(())
}

async fn add_tag_adoption(conn: &mut SqliteConnection) -> Result<()> {
    ensure_column(
        conn,
        "tag_catalog",
        "adopted",
        "ALTER TABLE tag_catalog ADD COLUMN adopted INTEGER NOT NULL DEFAULT 1",
    )
    .await?;
    ensure_column(
        conn,
        "feed_items",
        "raw_tags",
        "ALTER TABLE feed_items ADD COLUMN raw_tags TEXT",
    )
    .await?;

    // The display column already held canonical names, so it is the best
    // available approximation of the original classifier output.
    conn.execute(
        "UPDATE feed_items SET raw_tags = tags WHERE raw_tags IS NULL AND tags IS NOT NULL",
    )
    .await
    .map_err(AppError::Database)?;

    conn.execute("CREATE INDEX IF NOT EXISTS idx_tag_catalog_adopted ON tag_catalog(adopted, name)")
        .await
        .map_err(AppError::Database)?;

    Ok(())
}

async fn create_tag_dictionary(conn: &mut SqliteConnection) -> Result<()> {
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS tag_dictionary (
            name TEXT PRIMARY KEY COLLATE NOCASE,
            explanation TEXT NOT NULL,
            embedding BLOB,
            embedding_key TEXT,
            prompt_version INTEGER NOT NULL DEFAULT 1,
            suggestion_json TEXT,
            suggestion_key TEXT,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .await
    .map_err(AppError::Database)?;
    Ok(())
}

/// Seed vocabulary for the topic layer.
///
/// `(id, label, definition)`. IDs are the stable identity: renaming a topic
/// keeps its slot and colour, and a retired ID is never reused. The list is
/// deliberately below the 49-slot ceiling so later topics have room, and the
/// definitions carry the include/exclude rule that keeps neighbouring topics
/// distinguishable.
const SEED_TOPICS: [(i64, &str, &str); 40] = [
    (1, "AI models & training", "Model architecture, training and inference; not product features built on top."),
    (2, "AI applications & agents", "Agent and assistant design, prompt and tool orchestration; not model internals."),
    (3, "AI evaluation & reliability", "Benchmarks, evals, failure analysis; not product or policy news."),
    (4, "AI policy & industry", "Regulation, funding, market moves around AI; not technique."),
    (5, "Programming languages", "Language design, semantics, idioms; not libraries or frameworks for one domain."),
    (6, "Software engineering & architecture", "Design, testing, refactoring, maintainability across stacks."),
    (7, "Web development", "Browsers, frontend frameworks, HTTP app layers."),
    (8, "Mobile & desktop development", "Native app platforms, distribution and platform APIs."),
    (9, "Databases", "Storage engines, query languages, schema design."),
    (10, "Data engineering & analysis", "Pipelines, formats, quality; not model training."),
    (11, "Cloud infrastructure & operations", "Provisioning, scaling, observability, cost."),
    (12, "Containers & deployment", "Images, orchestration, release and rollout; a container tool is not a synonym for a cloud provider."),
    (13, "Operating systems", "Kernels, system tools, platform behaviour."),
    (14, "Networking & protocols", "Transport, DNS, proxies, protocol design; not application security."),
    (15, "Developer tools & workflow", "Editors, build systems, version control, CLI ergonomics."),
    (16, "Security engineering & offense", "Vulnerabilities, exploits, authentication; not privacy law."),
    (17, "Privacy & digital rights", "Data protection, tracking, civil liberties."),
    (18, "Open source & self-hosting", "Licensing practice, project governance, running your own services."),
    (19, "Hardware & silicon", "Devices, chips, embedded and manufacturing."),
    (20, "Graphics, imaging & visualization", "Rendering, image processing, charts and diagrams."),
    (21, "Games & game development", "Game design, engines, play culture."),
    (22, "Design & user experience", "Interaction, typography, accessibility, visual craft."),
    (23, "Product & user research", "Discovery, requirements, experiment design."),
    (24, "Startups & business", "Company building, operations, business models."),
    (25, "Investing & funding", "Capital markets, venture deals, valuations."),
    (26, "Economics & business policy", "Macro and industry policy; not company-building advice."),
    (27, "Management & workplace", "Teams, hiring, leadership, labour."),
    (28, "Productivity & knowledge management", "Personal workflow, notes, focus, time."),
    (29, "Writing & publishing", "Prose craft, editing, publishing platforms."),
    (30, "Education & learning", "Teaching, curricula, study practice."),
    (31, "Media & internet culture", "Journalism, platforms, online communities."),
    (32, "Politics & public policy", "Elections, governance, public administration."),
    (33, "Law & intellectual property", "Legal analysis, patents, compliance."),
    (34, "Philosophy & ethics", "Logic, metaphysics, moral questions."),
    (35, "Psychology & relationships", "Mind, behaviour, interpersonal life."),
    (36, "Health & living", "Medicine, wellbeing, everyday life."),
    (37, "History & culture", "Past events, traditions, cultural commentary."),
    (38, "Arts & entertainment", "Music, film, books, creative work."),
    (39, "Mathematics & natural sciences", "Formal and physical sciences."),
    (40, "Space & robotics", "Astronomy, launch, autonomy and machines."),
];

/// Largest topic ID the navigation layer accepts. The ceiling is the product
/// constraint (≤50 entries including the virtual “Unsorted” entry), so a new
/// topic may take a free slot but never grow the count without a decision.
pub const MAX_TOPIC_ID: i64 = 49;

/// The topic catalog, its per-tag assignment, and the suggestion cache.
async fn create_topic_tables(conn: &mut SqliteConnection) -> Result<()> {
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS topic_categories (
            id INTEGER PRIMARY KEY,
            label TEXT NOT NULL,
            definition TEXT NOT NULL DEFAULT '',
            sort_order INTEGER NOT NULL DEFAULT 0,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            CHECK (id BETWEEN 1 AND 49),
            CHECK (TRIM(label) <> '')
        )
        "#,
    )
    .await
    .map_err(AppError::Database)?;

    // One row per known name: `assigned` needs a topic, `context_only` and
    // `review` must not have one, so a half-decided word can never leak into
    // navigation as if a human had placed it.
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS tag_topic_assignments (
            tag_name TEXT PRIMARY KEY COLLATE NOCASE,
            category_id INTEGER REFERENCES topic_categories(id),
            state TEXT NOT NULL,
            source TEXT NOT NULL DEFAULT 'manual',
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            CHECK (state IN ('assigned', 'context_only', 'review')),
            CHECK (source IN ('manual', 'ai')),
            CHECK ((state = 'assigned') = (category_id IS NOT NULL))
        )
        "#,
    )
    .await
    .map_err(AppError::Database)?;

    conn.execute("CREATE INDEX IF NOT EXISTS idx_tag_topic_assignments_category ON tag_topic_assignments(category_id)")
        .await
        .map_err(AppError::Database)?;

    for (id, label, definition) in SEED_TOPICS {
        sqlx::query(
            r#"INSERT INTO topic_categories (id, label, definition, sort_order)
               VALUES ($1, $2, $3, $1)
               ON CONFLICT (id) DO NOTHING"#,
        )
        .bind(id)
        .bind(label)
        .bind(definition)
        .execute(&mut *conn)
        .await
        .map_err(AppError::Database)?;
    }

    Ok(())
}

/// Drop the two columns that only existed for the category model.
///
/// `tag_catalog.adopted` said "show this name as an article category"; topics
/// say where a tag belongs now, so the flag has no reader. `feed_items.category`
/// held the classifier's own free-text category, which 73% of articles never
/// got and only one UI line ever showed.
async fn remove_category_columns(conn: &mut SqliteConnection) -> Result<()> {
    // The index would block the column drop on older SQLite builds and is
    // useless without the column it covers.
    conn.execute("DROP INDEX IF EXISTS idx_tag_catalog_adopted")
        .await
        .map_err(AppError::Database)?;
    for ddl in [
        "ALTER TABLE tag_catalog DROP COLUMN adopted",
        "ALTER TABLE feed_items DROP COLUMN category",
    ] {
        conn.execute(ddl).await.map_err(AppError::Database)?;
    }
    Ok(())
}

async fn add_missing_columns(conn: &mut SqliteConnection) -> Result<()> {
    ensure_column(
        conn,
        "feed_items",
        "content_md",
        "ALTER TABLE feed_items ADD COLUMN content_md TEXT",
    )
    .await?;
    ensure_column(
        conn,
        "feed_items",
        "tags",
        "ALTER TABLE feed_items ADD COLUMN tags TEXT",
    )
    .await?;
    ensure_column(
        conn,
        "feed_items",
        "category",
        "ALTER TABLE feed_items ADD COLUMN category TEXT",
    )
    .await?;
    ensure_column(
        conn,
        "feed_items",
        "translated_title",
        "ALTER TABLE feed_items ADD COLUMN translated_title TEXT",
    )
    .await?;
    ensure_column(
        conn,
        "feed_items",
        "translated_content",
        "ALTER TABLE feed_items ADD COLUMN translated_content TEXT",
    )
    .await?;
    ensure_column(
        conn,
        "feed_items",
        "translated_at",
        "ALTER TABLE feed_items ADD COLUMN translated_at DATETIME",
    )
    .await?;
    ensure_column(
        conn,
        "feed_items",
        "is_ignored",
        "ALTER TABLE feed_items ADD COLUMN is_ignored BOOLEAN DEFAULT 0",
    )
    .await?;
    ensure_column(
        conn,
        "subscriptions",
        "auto_classify",
        "ALTER TABLE subscriptions ADD COLUMN auto_classify BOOLEAN DEFAULT 1",
    )
    .await?;

    Ok(())
}

/// The suggestion cache columns are added in the same step as the topic
/// tables: they only ever hold unapproved candidates for the topic layer, so
/// an older database gains them exactly when that layer arrives.
async fn add_dictionary_suggestion_columns(conn: &mut SqliteConnection) -> Result<()> {
    ensure_column(
        conn,
        "tag_dictionary",
        "suggestion_json",
        "ALTER TABLE tag_dictionary ADD COLUMN suggestion_json TEXT",
    )
    .await?;
    ensure_column(
        conn,
        "tag_dictionary",
        "suggestion_key",
        "ALTER TABLE tag_dictionary ADD COLUMN suggestion_key TEXT",
    )
    .await?;
    Ok(())
}

/// Row shape used by the duplicate merge.
#[derive(sqlx::FromRow, Clone)]
struct DuplicateRow {
    id: i64,
    subscription_id: i64,
    guid: String,
    link: Option<String>,
    content: Option<String>,
    content_md: Option<String>,
    description: Option<String>,
    author: Option<String>,
    published_at: Option<chrono::DateTime<chrono::Utc>>,
    is_website_content: bool,
    is_read: bool,
    is_favorite: bool,
    is_read_later: bool,
    is_ignored: bool,
    tags: Option<String>,
    category: Option<String>,
    translated_title: Option<String>,
    translated_content: Option<String>,
    translated_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Remove duplicate (subscription_id, guid) rows **without losing user
/// annotations**.
///
/// The old implementation deleted every row except `MIN(id)`. A duplicate is
/// usually a re-published entry: the newer row may carry the reader's
/// favorite/read/saved flags, a longer body, or a translation, so deleting it
/// threw away exactly the data the user created. Flags are merged with OR,
/// text columns keep the richest non-empty value, and tags are unioned.
/// Rows with NULL guid are left alone (SQLite unique indexes treat NULLs as
/// distinct).
async fn merge_duplicate_feed_items(conn: &mut SqliteConnection) -> Result<()> {
    let rows: Vec<DuplicateRow> = sqlx::query_as(
        r#"
        SELECT id, subscription_id, guid, link, content, content_md, description, author,
               published_at, is_website_content, is_read, is_favorite, is_read_later,
               is_ignored, tags, category, translated_title, translated_content, translated_at
          FROM feed_items
         WHERE guid IS NOT NULL
           AND (subscription_id, guid) IN (
               SELECT subscription_id, guid FROM feed_items WHERE guid IS NOT NULL
                GROUP BY subscription_id, guid HAVING COUNT(*) > 1
           )
         ORDER BY subscription_id, guid, id
        "#,
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(AppError::Database)?;

    if rows.is_empty() {
        return Ok(());
    }

    let mut merged: Vec<DuplicateRow> = Vec::new();
    let mut removed = 0usize;
    let mut index = 0usize;
    while index < rows.len() {
        let start = index;
        while index < rows.len()
            && rows[index].subscription_id == rows[start].subscription_id
            && rows[index].guid == rows[start].guid
        {
            index += 1;
        }
        let group = &rows[start..index];
        if group.len() < 2 {
            continue;
        }

        // Keeper is the oldest row; every other row contributes annotations
        // and is then deleted.
        let mut keep = group[0].clone();
        for other in &group[1..] {
            keep.is_read |= other.is_read;
            keep.is_favorite |= other.is_favorite;
            keep.is_read_later |= other.is_read_later;
            keep.is_ignored |= other.is_ignored;
            keep.link = keep.link.clone().or_else(|| other.link.clone());
            keep.description = keep
                .description
                .clone()
                .or_else(|| other.description.clone());
            keep.author = keep.author.clone().or_else(|| other.author.clone());
            keep.published_at = keep.published_at.or(other.published_at);
            keep.category = keep.category.clone().or_else(|| other.category.clone());
            keep.tags = merge_tag_json(keep.tags.as_deref(), other.tags.as_deref());
            keep = prefer_richer(keep, other);
        }
        removed += group.len() - 1;
        merged.push(keep);
    }

    for keep in &merged {
        sqlx::query(
            r#"
            UPDATE feed_items
               SET link = $2, content = $3, content_md = $4, description = $5, author = $6,
                   published_at = $7, is_website_content = $8, is_read = $9, is_favorite = $10,
                   is_read_later = $11, is_ignored = $12, tags = $13, category = $14,
                   translated_title = $15, translated_content = $16, translated_at = $17
             WHERE id = $1
            "#,
        )
        .bind(keep.id)
        .bind(&keep.link)
        .bind(&keep.content)
        .bind(&keep.content_md)
        .bind(&keep.description)
        .bind(&keep.author)
        .bind(keep.published_at)
        .bind(keep.is_website_content)
        .bind(keep.is_read)
        .bind(keep.is_favorite)
        .bind(keep.is_read_later)
        .bind(keep.is_ignored)
        .bind(&keep.tags)
        .bind(&keep.category)
        .bind(&keep.translated_title)
        .bind(&keep.translated_content)
        .bind(keep.translated_at)
        .execute(&mut *conn)
        .await
        .map_err(AppError::Database)?;
    }

    if removed > 0 {
        sqlx::query(
            r#"
            DELETE FROM feed_items
             WHERE guid IS NOT NULL AND id NOT IN (
                 SELECT MIN(id) FROM feed_items WHERE guid IS NOT NULL
                 GROUP BY subscription_id, guid
             )
            "#,
        )
        .execute(&mut *conn)
        .await
        .map_err(AppError::Database)?;
        println!(
            "[migration] merged annotations from {} duplicate feed_items row(s)",
            removed
        );
    }

    Ok(())
}

/// The richer of two optional text values: non-empty, then longest.
fn richer<'a>(a: Option<&'a String>, b: Option<&'a String>) -> Option<&'a String> {
    match (a, b) {
        (None, None) => None,
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (Some(x), Some(y)) => {
            if y.trim().len() > x.trim().len() {
                Some(y)
            } else {
                Some(x)
            }
        }
    }
}

/// Keep the richer copy of the content/translation columns.
///
/// `is_website_content` follows whichever `content_md` won, so the flag can
/// never claim website text while holding the RSS snippet.
fn prefer_richer(mut keep: DuplicateRow, other: &DuplicateRow) -> DuplicateRow {
    keep.content = richer(keep.content.as_ref(), other.content.as_ref()).cloned();

    let previous_md = keep.content_md.clone();
    let best_md = richer(previous_md.as_ref(), other.content_md.as_ref()).cloned();
    if best_md != previous_md {
        keep.is_website_content = other.is_website_content;
    }
    keep.content_md = best_md;

    let previous_translation = keep.translated_content.clone();
    let best_translation = richer(
        previous_translation.as_ref(),
        other.translated_content.as_ref(),
    )
    .cloned();
    if best_translation != previous_translation {
        keep.translated_at = other.translated_at.or(keep.translated_at);
        keep.translated_title = other.translated_title.clone().or(keep.translated_title);
    }
    keep.translated_content = best_translation;
    if keep.translated_title.is_none() {
        keep.translated_title = other.translated_title.clone();
    }
    if keep.translated_at.is_none() {
        keep.translated_at = other.translated_at;
    }
    keep
}

/// Union two JSON tag arrays, preserving order and dropping duplicates.
fn merge_tag_json(a: Option<&str>, b: Option<&str>) -> Option<String> {
    let parse = |raw: Option<&str>| -> Vec<String> {
        raw.and_then(|r| serde_json::from_str::<Vec<String>>(r).ok())
            .unwrap_or_default()
    };
    let mut out = parse(a);
    for tag in parse(b) {
        if !out.contains(&tag) {
            out.push(tag);
        }
    }
    if out.is_empty() {
        None
    } else {
        serde_json::to_string(&out).ok()
    }
}

/// The durable job queue and the validity/validator columns.
async fn create_jobs_table(conn: &mut SqliteConnection) -> Result<()> {
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS jobs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            kind TEXT NOT NULL,
            item_id INTEGER,
            payload TEXT,
            state TEXT NOT NULL DEFAULT 'queued',
            priority INTEGER NOT NULL DEFAULT 0,
            attempts INTEGER NOT NULL DEFAULT 0,
            max_attempts INTEGER NOT NULL DEFAULT 3,
            next_attempt_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
            lease_until DATETIME,
            last_error TEXT,
            created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .await
    .map_err(AppError::Database)?;

    for ddl in [
        "CREATE INDEX IF NOT EXISTS idx_jobs_claim ON jobs(state, next_attempt_at)",
        "CREATE INDEX IF NOT EXISTS idx_jobs_item ON jobs(item_id)",
    ] {
        conn.execute(ddl).await.map_err(AppError::Database)?;
    }
    Ok(())
}

/// Hash the source text of every pre-existing translation.
///
/// The hash is computed from exactly the same column selection the translation
/// pipeline uses (`content_md` → `content` → `description`), so an unchanged
/// article keeps its cached translation while an edited one is correctly
/// detected as stale. Rows translated from a live web page (the webview
/// streaming path) cannot be reproduced from the database; they are hashed
/// against the stored text and will simply be re-translated once.
/// `(id, content_md, content, description)` for one legacy translation.
type LegacyTranslationRow = (i64, Option<String>, Option<String>, Option<String>);

async fn backfill_translation_hashes(conn: &mut SqliteConnection) -> Result<()> {
    let rows: Vec<LegacyTranslationRow> = sqlx::query_as(
        r#"
        SELECT id, content_md, content, description
          FROM feed_items
         WHERE translated_content IS NOT NULL
           AND translated_content <> ''
           AND translated_source_hash IS NULL
        "#,
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(AppError::Database)?;

    if rows.is_empty() {
        return Ok(());
    }

    let mut updated = 0usize;
    for (id, content_md, content, description) in rows {
        let source = [content_md, content, description]
            .into_iter()
            .flatten()
            .find(|text| !text.trim().is_empty())
            .unwrap_or_default();
        let hash = crate::ai::translation_source_hash(&source);
        sqlx::query(
            r#"
            UPDATE feed_items
               SET translated_source_hash = $2,
                   translated_model = COALESCE(translated_model, $3),
                   translated_prompt_version = COALESCE(translated_prompt_version, 0)
             WHERE id = $1
            "#,
        )
        .bind(id)
        .bind(hash)
        .bind(crate::ai::LEGACY_TRANSLATION_PROVENANCE)
        .execute(&mut *conn)
        .await
        .map_err(AppError::Database)?;
        updated += 1;
    }

    println!("[migration] hashed {updated} pre-existing translation(s)");
    Ok(())
}

async fn add_validity_columns(conn: &mut SqliteConnection) -> Result<()> {
    ensure_column(
        conn,
        "feed_items",
        "translated_source_hash",
        "ALTER TABLE feed_items ADD COLUMN translated_source_hash TEXT",
    )
    .await?;
    ensure_column(
        conn,
        "feed_items",
        "translated_model",
        "ALTER TABLE feed_items ADD COLUMN translated_model TEXT",
    )
    .await?;
    ensure_column(
        conn,
        "feed_items",
        "translated_prompt_version",
        "ALTER TABLE feed_items ADD COLUMN translated_prompt_version INTEGER",
    )
    .await?;
    ensure_column(
        conn,
        "subscriptions",
        "http_etag",
        "ALTER TABLE subscriptions ADD COLUMN http_etag TEXT",
    )
    .await?;
    ensure_column(
        conn,
        "subscriptions",
        "http_last_modified",
        "ALTER TABLE subscriptions ADD COLUMN http_last_modified TEXT",
    )
    .await?;
    Ok(())
}

async fn create_indexes(conn: &mut SqliteConnection) -> Result<()> {
    // Drop legacy low-selectivity indexes. The boolean single-column indexes
    // are essentially never used by the query planner, and the subscription
    // index is covered by the (subscription_id, published_at) composite.
    let drops = [
        "DROP INDEX IF EXISTS idx_feed_items_is_read",
        "DROP INDEX IF EXISTS idx_feed_items_is_favorite",
        "DROP INDEX IF EXISTS idx_feed_items_is_read_later",
        "DROP INDEX IF EXISTS idx_feed_items_subscription",
    ];
    for ddl in drops {
        sqlx::query(ddl)
            .execute(&mut *conn)
            .await
            .map_err(AppError::Database)?;
    }

    let indexes = [
        // Covers the hot path: WHERE subscription_id = ? ORDER BY published_at DESC
        "CREATE INDEX IF NOT EXISTS idx_feed_items_sub_pub ON feed_items(subscription_id, published_at DESC)",
        "CREATE INDEX IF NOT EXISTS idx_feed_items_published ON feed_items(published_at DESC)",
        "CREATE INDEX IF NOT EXISTS idx_feed_items_fav_pub ON feed_items(is_favorite, published_at DESC)",
        "CREATE INDEX IF NOT EXISTS idx_feed_items_rl_pub ON feed_items(is_read_later, published_at DESC)",
    ];
    for ddl in indexes {
        sqlx::query(ddl)
            .execute(&mut *conn)
            .await
            .map_err(AppError::Database)?;
    }

    // The unique dedup index must not fail silently: without it the fetch
    // pipeline loses its last line of defense against duplicate rows.
    let unique = "CREATE UNIQUE INDEX IF NOT EXISTS idx_feed_items_guid ON feed_items(subscription_id, guid)";
    sqlx::query(unique)
        .execute(&mut *conn)
        .await
        .map_err(AppError::Database)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::run_migrations;

    #[tokio::test]
    async fn legacy_schema_gets_missing_columns_and_indexes() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("create test database");

        sqlx::query(
            "CREATE TABLE subscriptions (id INTEGER PRIMARY KEY, url TEXT NOT NULL UNIQUE)",
        )
        .execute(&pool)
        .await
        .expect("create legacy subscriptions table");
        sqlx::query(
            "CREATE TABLE feed_items (
                id INTEGER PRIMARY KEY,
                subscription_id INTEGER NOT NULL,
                guid TEXT,
                title TEXT NOT NULL,
                link TEXT,
                content TEXT,
                description TEXT,
                author TEXT,
                published_at DATETIME,
                fetched_at DATETIME,
                is_website_content BOOLEAN DEFAULT 0,
                is_read BOOLEAN DEFAULT 0,
                is_favorite BOOLEAN DEFAULT 0,
                is_read_later BOOLEAN DEFAULT 0,
                tags TEXT
            )",
        )
        .execute(&pool)
        .await
        .expect("create legacy feed_items table");
        sqlx::query(
            "INSERT INTO feed_items (subscription_id, title, tags) VALUES (1, 'legacy', ?)",
        )
        .bind(r#"["Machine Learning","machine-learning","AI"]"#)
        .execute(&pool)
        .await
        .expect("seed legacy tagged item");

        run_migrations(&pool)
            .await
            .expect("legacy migration should complete");

        let content_md: Option<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info('feed_items') WHERE name = 'content_md'",
        )
        .fetch_optional(&pool)
        .await
        .expect("inspect content_md");
        let auto_classify: Option<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info('subscriptions') WHERE name = 'auto_classify'",
        )
        .fetch_optional(&pool)
        .await
        .expect("inspect auto_classify");
        let index: Option<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND name = 'idx_feed_items_guid'",
        )
        .fetch_optional(&pool)
        .await
        .expect("inspect unique index");

        assert_eq!(content_md.as_deref(), Some("content_md"));
        assert_eq!(auto_classify.as_deref(), Some("auto_classify"));
        assert_eq!(index.as_deref(), Some("idx_feed_items_guid"));

        let normalized_tags: (String,) = sqlx::query_as("SELECT tags FROM feed_items WHERE id = 1")
            .fetch_one(&pool)
            .await
            .expect("inspect migrated tags");
        assert_eq!(normalized_tags.0, r#"["machine_learning","ai"]"#);
        let catalog_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tag_catalog")
            .fetch_one(&pool)
            .await
            .expect("inspect tag catalog");
        assert_eq!(catalog_count.0, 2);

        let database_id: String =
            sqlx::query_scalar("SELECT value FROM app_metadata WHERE key = 'database_id'")
                .fetch_one(&pool)
                .await
                .expect("inspect database identity");
        assert_eq!(database_id.len(), 32);
    }
}

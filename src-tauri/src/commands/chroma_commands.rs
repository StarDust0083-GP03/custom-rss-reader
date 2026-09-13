use std::sync::Arc;

use tauri::State;

use crate::chroma::service::ChromaService;
use crate::chroma::ChromaConfig;
use crate::error::Result;

use super::AppState;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChromaConfigResponse {
    pub host: String,
    pub port: u16,
    pub collection_name: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ChromaInitializationResponse {
    pub config: ChromaConfigResponse,
    pub sync: crate::chroma::sync::SyncReport,
}

#[tauri::command]
pub async fn set_chroma_config(
    state: State<'_, AppState>,
    host: Option<String>,
    port: Option<u16>,
    collection_name: Option<String>,
    enabled: Option<bool>,
) -> Result<()> {
    let mut config = ChromaConfig::load();
    if let Some(host) = host {
        config.host = host.trim().trim_end_matches('/').to_string();
    }
    if let Some(port) = port {
        config.port = port;
    }
    if let Some(collection_name) = collection_name {
        config.collection_name = collection_name.trim().to_string();
    }
    if let Some(enabled) = enabled {
        config.enabled = enabled;
    }
    config.validate()?;
    // The worker and the feed pipeline read the switch from the holder, so a
    // saved change must be visible to them before the next queue poll.
    state.chroma_service.refresh_from_config().await;
    config.save()?;
    state.chroma_service.invalidate().await;

    // An enabled host/collection change gets a new collection identity. Run
    // the identity-aware catch-up now instead of leaving semantic search empty
    // until the next app restart or feed refresh.
    if config.enabled {
        let repo = state.feed_repo.clone();
        let chroma = state.chroma_service.clone();
        tauri::async_runtime::spawn(async move {
            crate::chroma::sync::run_background_sync(repo, chroma).await;
        });
    }
    Ok(())
}

/// Enable ChromaDB, ensure its collection exists, and index all existing
/// articles in one user-facing operation. The configuration is saved only
/// after the server and collection have been validated.
#[tauri::command]
pub async fn enable_chroma_and_index(
    state: State<'_, AppState>,
    host: String,
    port: u16,
    collection_name: String,
) -> Result<ChromaInitializationResponse> {
    let config = ChromaConfig {
        host: host.trim().trim_end_matches('/').to_string(),
        port,
        collection_name: collection_name.trim().to_string(),
        enabled: true,
    };
    config.validate()?;

    // ChromaService::new performs the v2 identity lookup and get-or-create
    // collection call. Do this before persisting enabled=true, so an
    // unreachable server cannot leave the app looking successfully enabled.
    let chroma = ChromaService::new(&config).await?;

    // Full resync is intentional for first-use: the persisted watermark may
    // belong to a previous collection, while every existing article must be
    // searchable after this one-click setup. Persist enabled=true only after
    // indexing succeeds, so a model/download failure cannot leave a partially
    // initialized configuration looking ready.
    let sync = crate::chroma::sync::full_resync(&state.feed_repo, &chroma).await?;
    config.save()?;
    state.chroma_service.invalidate().await;
    Ok(ChromaInitializationResponse {
        config: ChromaConfigResponse {
            host: config.host,
            port: config.port,
            collection_name: config.collection_name,
            enabled: config.enabled,
        },
        sync,
    })
}

#[tauri::command]
pub async fn get_chroma_config() -> Result<ChromaConfigResponse> {
    let config = ChromaConfig::load();
    Ok(ChromaConfigResponse {
        host: config.host,
        port: config.port,
        collection_name: config.collection_name,
        enabled: config.enabled,
    })
}

/// Semantic search over the article index.
///
/// Returns real [`crate::models::FeedItemSummary`] rows, not metadata-only
/// hits: SQLite stays authoritative for titles, read flags, favorites, and
/// subscription identity, and the ranking from the vector search is
/// preserved. The frontend used to synthesize these rows itself, which meant
/// semantic results claimed `subscription_id: 0`, `is_read: false`, and no
/// source for articles that were read, favorited, and belonged to a feed.
#[tauri::command]
pub async fn semantic_search(
    state: State<'_, AppState>,
    query: String,
    limit: Option<i64>,
) -> Result<Vec<crate::models::FeedItemSummary>> {
    let chroma = get_chroma_service(&state).await?;
    let hits = chroma.search(&query, clamp_limit(limit, 10, 100)).await?;
    hydrate_hits(&state.feed_repo, hits).await
}

/// Join ranked vector hits with their SQLite rows, dropping hits whose
/// article no longer exists (the index can lag a delete) and restoring rank
/// order.
pub(crate) async fn hydrate_hits(
    repo: &Arc<dyn crate::repositories::FeedItemRepository>,
    hits: Vec<crate::chroma::service::SemanticSearchResult>,
) -> Result<Vec<crate::models::FeedItemSummary>> {
    if hits.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = hits.iter().map(|hit| hit.item_id).collect();
    let mut summaries = repo.find_summaries_by_ids(&ids).await?;
    let rank: std::collections::HashMap<i64, usize> =
        ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    summaries.sort_by_key(|s| rank.get(&s.id).copied().unwrap_or(usize::MAX));
    Ok(summaries)
}

/// Find articles similar to the given feed item.
///
/// Returns real [`FeedItemSummary`] rows (not synthesized hits) so the
/// frontend can navigate into the detail view of each result, unlike
/// `semantic_search` whose hits are metadata-only.
#[tauri::command]
pub async fn find_similar_items(
    state: State<'_, AppState>,
    item_id: i64,
    limit: Option<i64>,
) -> Result<Vec<crate::models::FeedItemSummary>> {
    let chroma = get_chroma_service(&state).await?;
    let limit = clamp_limit(limit, 10, 50);

    let item = state.feed_repo.find_by_id(item_id).await?;
    let hits = chroma.find_similar(&item, limit).await?;
    if hits.is_empty() {
        return Ok(Vec::new());
    }

    // find_summaries_by_ids returns DB order — hydrate_hits restores the
    // similarity ranking so the most similar article shows first.
    hydrate_hits(&state.feed_repo, hits).await
}

/// Index health for the settings panel.
///
/// The reader asked "is semantic search working?" and the only answer was the
/// terminal log. This assembles everything the panel shows in one call: the
/// watermark (what is known to be indexed), the library size, the queue depth,
/// and the live sync phase.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexStatus {
    /// Semantic search switched on in the config.
    pub enabled: bool,
    /// Whether a sync/reindex walk is running right now.
    pub running: bool,
    /// Stage of the running walk (`""` when idle).
    pub phase: &'static str,
    /// Items with `id <= indexed` are known to be in the index.
    pub indexed: i64,
    /// Highest item id in the library (an approximation of the item count).
    pub total: i64,
    /// Index jobs waiting in the durable queue.
    pub queued_jobs: i64,
    /// Item ids queued for (re)indexing by the sync state file.
    pub pending_upserts: i64,
    /// Item ids queued for deletion from the index.
    pub pending_deletes: i64,
    pub collection_name: String,
    /// Identity of the active collection, when one has been created.
    pub collection_id: Option<String>,
    /// Rows processed so far in the running walk.
    pub done: i64,
    /// Rows the running walk expects to scan.
    pub scan_total: i64,
    pub elapsed_ms: u128,
}

#[tauri::command]
pub async fn chroma_index_status(state: State<'_, AppState>) -> Result<IndexStatus> {
    let config = crate::chroma::ChromaConfig::load();
    let sync = crate::chroma::sync::SyncState::load();
    let progress = crate::chroma::sync::current_progress();
    let queued_jobs = state
        .jobs
        .stats()
        .await
        .map(|stats| {
            stats
                .queued_by_kind
                .get(crate::models::job::JobKind::ChromaUpsert.as_str())
                .copied()
                .unwrap_or(0)
        })
        .unwrap_or(0);

    Ok(IndexStatus {
        enabled: config.enabled,
        running: progress.running,
        phase: progress.phase,
        indexed: sync.last_indexed_id,
        total: state.feed_repo.max_item_id().await.unwrap_or(0),
        queued_jobs,
        pending_upserts: sync.pending_upserts.len() as i64,
        pending_deletes: sync.pending_deletes.len() as i64,
        collection_name: config.collection_name,
        collection_id: sync.collection_id,
        done: progress.done,
        scan_total: progress.total,
        elapsed_ms: progress.elapsed_ms,
    })
}

#[tauri::command]
pub async fn reindex_chromadb(state: State<'_, AppState>) -> Result<String> {
    let chroma = get_chroma_service(&state).await?;

    // Full rebuild via the sync mechanism: reset the watermark and replay.
    // Rows stream through the lightweight IndexRow projection (keyset
    // pagination, truncated text) instead of full FeedItem pages.
    let report = crate::chroma::sync::full_resync(&state.feed_repo, &chroma).await?;
    Ok(format!(
        "Re-indexed {} items in {}ms",
        report.indexed, report.duration_ms
    ))
}

/// Incremental ChromaDB sync (the same pass that runs automatically at
/// startup and after each refresh). Exposed as a command so the UI can
/// force a catch-up without a full rebuild.
#[tauri::command]
pub async fn chroma_sync(state: State<'_, AppState>) -> Result<String> {
    let chroma = get_chroma_service(&state).await?;
    let report = crate::chroma::sync::incremental_sync(&state.feed_repo, &chroma).await?;
    Ok(format!(
        "Synced: {} indexed, {} deleted ({}ms)",
        report.indexed, report.deleted, report.duration_ms
    ))
}

/// Live progress of the current reindex/sync run, polled by the UI while
/// `reindex_chromadb` is in flight so it can show a running status.
#[tauri::command]
pub async fn chroma_sync_progress(
    _state: State<'_, AppState>,
) -> Result<crate::chroma::sync::SyncProgress> {
    Ok(crate::chroma::sync::current_progress())
}

/// Refresh website Markdown for articles that were imported from a feed's
/// history without it (website mode came later, or the fetch-time pre-cache
/// failed) and queue them for ChromaDB re-indexing, so they become findable
/// by semantic search.
///
/// The pass is strictly rate-limited (QPS cap, small batch, no retries,
/// one host-failure cutoff) — see the contract in `chroma::backfill`.
/// Intended to be fired (unawaited) by the frontend when the search view
/// loads; the report describes what was done once the pass finishes.
#[tauri::command]
pub async fn chroma_backfill_markdown(
    state: State<'_, AppState>,
) -> Result<crate::chroma::backfill::BackfillReport> {
    crate::chroma::backfill::backfill_website_markdown(
        &state.feed_repo,
        &state.fetcher,
        &state.chroma_service,
    )
    .await
}

#[tauri::command]
pub async fn chroma_health_check(state: State<'_, AppState>) -> Result<bool> {
    match state.chroma_service.get().await {
        Some(service) => {
            let ok = service.health_check().await?;
            if !ok {
                // Server went away — drop the stale handle so the next
                // check/search reconnects instead of failing forever.
                state.chroma_service.invalidate().await;
            }
            Ok(ok)
        }
        None => Ok(false),
    }
}

// ---- Helpers ----

/// Clamp `limit` to a sane upper bound, same contract as the item commands —
/// IPC callers must not be able to pull an unbounded result set.
fn clamp_limit(limit: Option<i64>, default: i64, max: i64) -> i64 {
    limit.unwrap_or(default).clamp(1, max)
}

async fn get_chroma_service(state: &AppState) -> Result<Arc<ChromaService>> {
    state.chroma_service.get().await.ok_or_else(|| {
        crate::error::AppError::OperationFailed(
            "ChromaDB is not reachable. Check the server and the Semantic DB settings.".into(),
        )
    })
}

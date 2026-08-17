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

#[tauri::command]
pub async fn set_chroma_config(
    host: Option<String>,
    port: Option<u16>,
    collection_name: Option<String>,
    enabled: Option<bool>,
) -> Result<()> {
    let mut config = ChromaConfig::load();
    if let Some(host) = host {
        config.host = host;
    }
    if let Some(port) = port {
        config.port = port;
    }
    if let Some(collection_name) = collection_name {
        config.collection_name = collection_name;
    }
    if let Some(enabled) = enabled {
        config.enabled = enabled;
    }
    config.save()?;
    Ok(())
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

#[tauri::command]
pub async fn semantic_search(
    state: State<'_, AppState>,
    query: String,
    limit: Option<i64>,
) -> Result<Vec<crate::chroma::service::SemanticSearchResult>> {
    let chroma = get_chroma_service(&state).await?;
    chroma.search(&query, clamp_limit(limit, 10, 100)).await
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

    let ids: Vec<i64> = hits.iter().map(|h| h.item_id).collect();
    let mut summaries = state.feed_repo.find_summaries_by_ids(&ids).await?;
    // find_summaries_by_ids returns DB order — restore the similarity
    // ranking so the most similar article shows first.
    let rank: std::collections::HashMap<i64, usize> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, i))
        .collect();
    summaries.sort_by_key(|s| rank.get(&s.id).copied().unwrap_or(usize::MAX));
    Ok(summaries)
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
    state
        .chroma_service
        .get()
        .await
        .ok_or_else(|| crate::error::AppError::OperationFailed(
            "ChromaDB is not reachable. Check the server and the Semantic DB settings.".into(),
        ))
}

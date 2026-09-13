//! Background-job visibility and recovery commands.
//!
//! Enrichment failures used to be invisible: a classification error printed to
//! stderr and the article simply never got tags. The queue is now queryable so
//! the UI can show pending work and offer an explicit retry.

use tauri::State;

use crate::error::Result;
use crate::models::JobStats;

use super::AppState;

/// Queue depth, the most recent errors, and why work is waiting.
#[tauri::command]
pub async fn get_job_stats(state: State<'_, AppState>) -> Result<JobStats> {
    let mut stats = state.jobs.stats().await?;

    // The queue itself cannot know that a dependency is missing, so the
    // command layer explains it. Without this, a fresh library shows "7,230
    // tasks queued" and gives the user nothing to do about it.
    if stats
        .queued_by_kind
        .get(crate::models::job::JobKind::Classify.as_str())
        .is_some_and(|count| *count > 0)
        && !ai_ready(&state).await
    {
        stats
            .blocked_reasons
            .push("ai_not_configured".into());
    }
    if stats
        .queued_by_kind
        .get(crate::models::job::JobKind::ChromaUpsert.as_str())
        .is_some_and(|count| *count > 0)
        && !state.chroma_service.is_configured()
    {
        stats
            .blocked_reasons
            .push("semantic_search_disabled".into());
    }
    Ok(stats)
}

/// Whether an AI service is available: either already built (settings saved in
/// this session) or present on disk (configured before launch). Checking only
/// the in-memory slot reported "AI is not configured" while the worker was
/// happily loading the file.
async fn ai_ready(state: &AppState) -> bool {
    if state.ai_service.read().await.is_some() {
        return true;
    }
    crate::ai::load_config()
        .map(|config| {
            !config.api_key.trim().is_empty()
                && !config.base_url.trim().is_empty()
                && !config.model.trim().is_empty()
        })
        .unwrap_or(false)
}

/// Put every failed job back in the queue with a fresh attempt budget.
#[tauri::command]
pub async fn retry_failed_jobs(state: State<'_, AppState>) -> Result<usize> {
    state.jobs.retry_failed().await
}

/// Recover jobs stuck in `running` because a previous process died holding
/// their lease.
#[tauri::command]
pub async fn requeue_expired_jobs(state: State<'_, AppState>) -> Result<usize> {
    state.jobs.requeue_expired().await
}

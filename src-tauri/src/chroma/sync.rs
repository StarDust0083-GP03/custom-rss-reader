//! Incremental ChromaDB synchronization.
//!
//! ## The problem
//!
//! Previously, items were indexed only at fetch time. If ChromaDB was down
//! or slow during a refresh, the item was silently lost from the index
//! forever (a console eprintln at best), items fetched before ChromaDB was
//! enabled were never indexed without a manual full reindex, and failed
//! deletes leaked orphaned vectors.
//!
//! ## The mechanism
//!
//! A **watermark + pending queues** state file (`~/.rss-reader/chroma_sync.json`)
//! drives an idempotent, crash-safe sync:
//!
//! 1. `last_indexed_id` — high-water mark. All items with `id > watermark`
//!    are pending. Advances only AFTER a page is successfully upserted and
//!    the state is persisted, so a crash mid-sync just replays the page
//!    (upserts are idempotent — same id overwrites).
//! 2. `pending_deletes` — item ids whose vector deletion failed (or that
//!    vanished before Chroma was enabled). Drained on each sync.
//! 3. `pending_upserts` — item ids whose indexing failed at fetch time.
//!    Drained on each sync.
//!
//! The watermark is validated against `max(feed_items.id)` at sync start;
//! if the database was reset (watermark above max), it restarts from 0.
//!
//! Triggers: app startup (background task), after every bulk fetch/refresh,
//! and the manual `chroma_sync` command. The "Re-Index All" button resets
//! the state and runs the same loop — one mechanism, no special cases.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::repositories::FeedItemRepository;

use super::service::ChromaService;

/// Rows per upsert page during sync.
pub const SYNC_PAGE_SIZE: i64 = 500;

/// Result of one sync run, surfaced to the UI/commands.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SyncReport {
    pub indexed: usize,
    pub deleted: usize,
    pub pages: usize,
    pub duration_ms: u128,
}

/// Live progress of the current sync/reindex run, polled by the UI so a
/// long re-index shows a running status instead of just a final toast.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct SyncProgress {
    /// True while a sync/reindex is in flight.
    pub running: bool,
    /// Current stage: `""` (idle), `"deletes"`, `"upserts"`, `"walk"`.
    pub phase: &'static str,
    /// Approximate total items to scan (`max(feed_items.id)` at run start).
    pub total: i64,
    /// Rows processed so far (deleted + upserted + walked).
    pub done: i64,
    pub pages: usize,
    pub elapsed_ms: u128,
}

/// Process-global progress tracker. Counters only — a plain `Mutex` is
/// enough: updated synchronously in the sync loop, snapshotted by a command.
static PROGRESS: Mutex<SyncProgress> = Mutex::new(SyncProgress {
    running: false,
    phase: "",
    total: 0,
    done: 0,
    pages: 0,
    elapsed_ms: 0,
});

/// Snapshot for the UI polling command.
pub fn current_progress() -> SyncProgress {
    *PROGRESS.lock().unwrap()
}

/// Mutate the shared progress state.
fn update_progress(f: impl FnOnce(&mut SyncProgress)) {
    let mut p = PROGRESS.lock().unwrap();
    f(&mut p);
}

/// Persisted sync state. Stored as JSON next to the other app config files.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncState {
    /// All items with id <= this value are known to be indexed (or were
    /// intentionally skipped because ChromaDB was disabled at the time).
    pub last_indexed_id: i64,
    /// Item ids awaiting deletion from the index.
    pub pending_deletes: Vec<i64>,
    /// Item ids awaiting (re)indexing.
    pub pending_upserts: Vec<i64>,
}

impl SyncState {
    /// Path of the state file: `~/.rss-reader/chroma_sync.json`.
    pub fn path() -> Result<PathBuf> {
        let home = dirs::home_dir()
            .ok_or_else(|| AppError::Internal("HOME directory not found".into()))?;
        Ok(home.join(".rss-reader").join("chroma_sync.json"))
    }

    /// Load from disk; missing/corrupt file yields a fresh (empty) state.
    pub fn load() -> Self {
        Self::path()
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Atomically persist (tmp file + rename) so a crash never leaves a
    /// half-written state behind.
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AppError::Internal(format!("Failed to create config dir: {}", e)))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| AppError::Internal(format!("Failed to serialize sync state: {}", e)))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)
            .map_err(|e| AppError::Internal(format!("Failed to write sync state: {}", e)))?;
        std::fs::rename(&tmp, &path)
            .map_err(|e| AppError::Internal(format!("Failed to rename sync state: {}", e)))?;
        Ok(())
    }

    /// Queue an item for deletion on the next sync (called when the live
    /// delete fails, e.g. ChromaDB briefly unreachable).
    pub fn queue_delete(id: i64) {
        Self::mutate(|s| {
            if !s.pending_deletes.contains(&id) {
                s.pending_deletes.push(id);
            }
            s.pending_upserts.retain(|&x| x != id);
        });
    }

    /// Queue an item for (re)indexing on the next sync.
    pub fn queue_upsert(id: i64) {
        Self::mutate(|s| {
            if !s.pending_upserts.contains(&id) {
                s.pending_upserts.push(id);
            }
            s.pending_deletes.retain(|&x| x != id);
        });
    }

    /// Load-modify-save with the modification applied to a fresh load. Best
    /// effort: a write failure is logged, never propagated to callers that
    /// run in fire-and-forget contexts.
    fn mutate(f: impl FnOnce(&mut SyncState)) {
        let mut state = Self::load();
        f(&mut state);
        if let Err(e) = state.save() {
            eprintln!("[chroma-sync] failed to persist sync state: {}", e);
        }
    }
}

/// Run one incremental sync pass.
///
/// Steps:
/// 1. Validate/reset the watermark against the current DB maximum.
/// 2. Drain `pending_deletes` (batched).
/// 3. Drain `pending_upserts`.
/// 4. Keyset-walk items above the watermark, page by page; after each
///    successful page the watermark is advanced and persisted.
///
/// Any failure aborts the run and returns the error — the state on disk
/// still reflects the last completed page, so the next run resumes there.
pub async fn incremental_sync(
    repo: &Arc<dyn FeedItemRepository>,
    chroma: &ChromaService,
) -> Result<SyncReport> {
    let started = std::time::Instant::now();
    let mut state = SyncState::load();
    let mut report = SyncReport::default();

    update_progress(|p| {
        p.running = true;
        p.phase = "";
        p.total = 0;
        p.done = 0;
        p.pages = 0;
        p.elapsed_ms = 0;
    });

    // 1. Watermark validation — a database reset (ids restarting at 1) must
    //    not be treated as "everything already indexed".
    let max_id = repo.max_item_id().await?;
    if state.last_indexed_id > max_id {
        state.last_indexed_id = 0;
        state.save()?;
    }
    // `total` approximates the number of items to scan (walk runs 0 → max id).
    update_progress(|p| p.total = max_id);

    let mut processed: i64 = 0;

    // 2. Pending deletes — deleting an id that isn't in the index is a
    //    no-op server-side, so blind batched deletes are safe.
    if !state.pending_deletes.is_empty() {
        update_progress(|p| p.phase = "deletes");
        chroma.delete_items(&state.pending_deletes).await?;
        report.deleted = state.pending_deletes.len();
        processed += report.deleted as i64;
        state.pending_deletes.clear();
        state.save()?;
    }

    // 3. Pending upserts (items whose fetch-time indexing failed).
    if !state.pending_upserts.is_empty() {
        update_progress(|p| p.phase = "upserts");
        let rows = repo.find_index_rows_by_ids(&state.pending_upserts).await?;
        chroma.upsert_index_rows(&rows).await?;
        report.indexed += rows.len();
        processed += rows.len() as i64;
        state.pending_upserts.clear();
        state.save()?;
    }

    // 4. Keyset walk above the watermark — the long haul (embedding page by
    //    page), so this is where per-page progress is reported.
    let pages_total = (max_id as usize + SYNC_PAGE_SIZE as usize - 1) / SYNC_PAGE_SIZE as usize;
    if max_id > state.last_indexed_id {
        update_progress(|p| p.phase = "walk");
    }
    loop {
        let rows = repo.find_index_page(state.last_indexed_id, SYNC_PAGE_SIZE).await?;
        if rows.is_empty() {
            break;
        }
        let last_id = rows.last().map(|r| r.id).unwrap_or(state.last_indexed_id);
        chroma.upsert_index_rows(&rows).await?;
        report.indexed += rows.len();
        report.pages += 1;
        processed += rows.len() as i64;
        // Advance + persist ONLY after a successful upsert — crash safety.
        state.last_indexed_id = last_id;
        state.save()?;

        update_progress(|p| {
            p.done = processed;
            p.pages = report.pages;
            p.elapsed_ms = started.elapsed().as_millis();
        });
        println!(
            "[chroma-sync] page {}/~{}: +{} indexed ({} total), {:?} elapsed",
            report.pages,
            pages_total.max(1),
            rows.len(),
            report.indexed,
            started.elapsed(),
        );

        if (rows.len() as i64) < SYNC_PAGE_SIZE {
            break;
        }
    }

    // Final snapshot: done, not running — the UI's last poll shows totals.
    update_progress(|p| {
        p.done = processed;
        p.pages = report.pages;
        p.running = false;
        p.phase = "";
        p.elapsed_ms = started.elapsed().as_millis();
    });

    report.duration_ms = started.elapsed().as_millis();
    Ok(report)
}

/// Full rebuild: reset the state (watermark back to 0, queues kept) and run
/// the incremental loop to completion. Upsert semantics make the re-write
/// of already-indexed items safe; this also reconciles any drift between
/// the DB and the index for ids <= the old watermark.
pub async fn full_resync(
    repo: &Arc<dyn FeedItemRepository>,
    chroma: &ChromaService,
) -> Result<SyncReport> {
    let mut state = SyncState::load();
    state.last_indexed_id = 0;
    state.save()?;
    incremental_sync(repo, chroma).await
}

/// Convenience wrapper used by fire-and-forget call sites (startup, post-
/// fetch). Lazy-connects through the holder; failures are logged, never
/// surfaced as command errors.
pub async fn run_background_sync(
    repo: Arc<dyn FeedItemRepository>,
    holder: crate::chroma::ChromaHolder,
) {
    let Some(chroma) = holder.get().await else { return };
    match incremental_sync(&repo, &chroma).await {
        Ok(report) if report.indexed > 0 || report.deleted > 0 => {
            println!(
                "[chroma-sync] indexed {} deleted {} in {}ms",
                report.indexed, report.deleted, report.duration_ms
            );
        }
        Ok(_) => {}
        Err(e) => eprintln!("[chroma-sync] incremental sync failed: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_state_roundtrip() {
        let state = SyncState {
            last_indexed_id: 42,
            pending_deletes: vec![1, 2],
            pending_upserts: vec![7],
        };
        let json = serde_json::to_string(&state).unwrap();
        let back: SyncState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.last_indexed_id, 42);
        assert_eq!(back.pending_deletes, vec![1, 2]);
        assert_eq!(back.pending_upserts, vec![7]);
    }

    #[test]
    fn test_sync_state_default() {
        let s = SyncState::default();
        assert_eq!(s.last_indexed_id, 0);
        assert!(s.pending_deletes.is_empty());
        assert!(s.pending_upserts.is_empty());
    }

    // NOTE: queue_delete/queue_upsert/save touch the REAL ~/.rss-reader
    // state file and are exercised implicitly by the sync flow; unit-testing
    // them in CI would mutate a developer's app data, so their dedup/mutex
    // logic is kept trivial instead.
}

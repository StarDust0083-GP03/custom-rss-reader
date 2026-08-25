use tauri::State;

use crate::error::Result;
use crate::models::{NewSubscription, Subscription, UpdateSubscription};

use super::AppState;

// ---- Thin command functions ----
// Each command extracts parameters, delegates to the service, and returns the result.
// No SQL, no business logic — just parameter plumbing.

#[tauri::command]
pub async fn add_subscription(
    state: State<'_, AppState>,
    url: String,
    title: Option<String>,
    website_url: Option<String>,
    rsshub_url: Option<String>,
    use_website: Option<bool>,
) -> Result<Subscription> {
    let input = NewSubscription {
        url,
        title,
        website_url,
        rsshub_url,
        use_website: use_website.unwrap_or(false),
        auto_classify: true,
        opml_attributes: None,
    };

    state.subscription_service.add_subscription(input).await
}

#[tauri::command]
pub async fn list_subscriptions(
    state: State<'_, AppState>,
) -> Result<Vec<Subscription>> {
    state.subscription_service.list_subscriptions().await
}

#[tauri::command]
pub async fn get_subscription(
    state: State<'_, AppState>,
    id: i64,
) -> Result<Subscription> {
    state.subscription_service.get_subscription(id).await
}

#[tauri::command]
pub async fn remove_subscription(
    state: State<'_, AppState>,
    id: i64,
) -> Result<()> {
    // Collect item ids BEFORE the cascade delete so the ChromaDB index can
    // be cleaned up too; otherwise deleted articles stay searchable forever.
    let item_ids = state.feed_repo.find_ids_by_subscription(id).await?;

    // Persist tombstones BEFORE the cascade delete. If the process dies after
    // SQLite commits but before Chroma cleanup, the next sync still knows what
    // to remove. The sync pass also checks that a tombstoned row is really
    // gone, so a crash before the SQLite delete cannot delete a live vector.
    crate::chroma::sync::SyncState::queue_deletes_durable(&item_ids).await?;

    if let Err(error) = state.subscription_service.remove_subscription(id).await {
        // The source row is still present; retaining its tombstone would make
        // a later sync delete a live vector, so best-effort rollback the queue.
        let _ = crate::chroma::sync::SyncState::clear_pending_deletes(&item_ids).await;
        return Err(error);
    }

    // Best-effort: a Chroma outage must not fail the subscription removal.
    // Failed or disabled cleanup leaves the durable tombstones for the next
    // incremental sync; a successful direct delete clears them.
    if let Some(chroma) = state.chroma_service.get().await {
        if let Err(e) = chroma.delete_items(&item_ids).await {
            eprintln!("ChromaDB cleanup for subscription {} failed: {}", id, e);
        } else if let Err(e) = crate::chroma::sync::SyncState::clear_pending_deletes(&item_ids).await {
            eprintln!("Failed to clear ChromaDB tombstones for subscription {}: {}", id, e);
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn update_subscription(
    state: State<'_, AppState>,
    id: i64,
    title: Option<String>,
    // `Some(Some(""))` clears the column, `None` leaves it unchanged.
    // (Tauri serializes JS `null` / absent as `Option::None`.)
    website_url: Option<Option<String>>,
    use_website: Option<bool>,
    rsshub_url: Option<Option<String>>,
) -> Result<Subscription> {
    let input = UpdateSubscription {
        title,
        website_url,
        use_website,
        rsshub_url,
    };

    state.subscription_service.update_subscription(id, input).await
}

#[tauri::command]
pub async fn toggle_use_website(
    state: State<'_, AppState>,
    id: i64,
) -> Result<Subscription> {
    state.subscription_service.toggle_use_website(id).await
}

#[tauri::command]
pub async fn toggle_auto_classify(
    state: State<'_, AppState>,
    id: i64,
) -> Result<Subscription> {
    state.subscription_service.toggle_auto_classify(id).await
}

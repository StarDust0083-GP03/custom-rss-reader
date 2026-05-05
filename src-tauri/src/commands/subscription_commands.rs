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
    state.subscription_service.remove_subscription(id).await
}

#[tauri::command]
pub async fn update_subscription(
    state: State<'_, AppState>,
    id: i64,
    title: Option<String>,
    website_url: Option<String>,
    use_website: Option<bool>,
    rsshub_url: Option<String>,
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

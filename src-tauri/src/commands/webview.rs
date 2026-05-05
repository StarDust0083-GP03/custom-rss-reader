use tauri_plugin_opener::OpenerExt;

use crate::error::{AppError, Result};

#[tauri::command]
pub async fn open_url_in_browser(app_handle: tauri::AppHandle, url: String) -> Result<()> {
    app_handle
        .opener()
        .open_url(url, None::<&str>)
        .map_err(|e| AppError::OperationFailed(format!("Failed to open URL: {}", e)))?;
    Ok(())
}

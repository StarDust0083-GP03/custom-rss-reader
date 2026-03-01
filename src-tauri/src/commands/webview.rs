use tauri::{WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

#[tauri::command]
pub async fn open_url_in_webview(app_handle: tauri::AppHandle, url: String) -> Result<(), String> {
    // Open URL in system default browser
    app_handle.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| format!("Failed to open URL: {}", e))?;

    Ok(())
}

use tauri::{WebviewUrl, WebviewWindowBuilder};

#[tauri::command]
pub async fn open_url_in_webview(app_handle: tauri::AppHandle, url: String) -> Result<(), String> {
    // Create a new webview window to display the URL inside the app
    let window_label = format!("webview_{}", uuid::Uuid::new_v4().to_string().replace("-", ""));
    
    WebviewWindowBuilder::new(&app_handle, &window_label, WebviewUrl::External(url.parse().map_err(|e| format!("Invalid URL: {}", e))?))
        .title("WebView")
        .inner_size(900.0, 700.0)
        .center()
        .build()
        .map_err(|e| format!("Failed to create webview window: {}", e))?;

    Ok(())
}

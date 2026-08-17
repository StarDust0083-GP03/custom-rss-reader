use tauri_plugin_opener::OpenerExt;

use crate::error::{AppError, Result};

/// Open `url` in the user's default browser.
///
/// Only `http://` and `https://` schemes are accepted — anything else is
/// rejected so a JS-injected `javascript:` href can't reach the OS shell.
#[tauri::command]
pub async fn open_url_in_browser(app_handle: tauri::AppHandle, url: String) -> Result<()> {
    if !is_safe_external_url(&url) {
        return Err(AppError::Validation(format!(
            "Refusing to open URL with unsafe scheme: {}",
            url
        )));
    }
    app_handle
        .opener()
        .open_url(url, None::<&str>)
        .map_err(|e| AppError::OperationFailed(format!("Failed to open URL: {}", e)))?;
    Ok(())
}

fn is_safe_external_url(url: &str) -> bool {
    // Case-insensitive (JS can hand us `HTTPS://...`) and tolerant of
    // surrounding whitespace. Still only ever http(s).
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_url() {
        assert!(is_safe_external_url("https://example.com"));
        assert!(is_safe_external_url("http://example.com"));
    }

    #[test]
    fn test_unsafe_url() {
        assert!(!is_safe_external_url("javascript:alert(1)"));
        assert!(!is_safe_external_url("file:///etc/passwd"));
        assert!(!is_safe_external_url("data:text/html,xxx"));
        assert!(!is_safe_external_url(""));
        assert!(!is_safe_external_url("JAVASCRIPT:alert(1)"));
    }
}
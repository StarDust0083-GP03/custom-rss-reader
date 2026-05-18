use crate::error::Result;

#[tauri::command]
pub async fn test_html2md(html: String) -> Result<String> {
    crate::content_processor::html_to_markdown_pipeline(&html)
}

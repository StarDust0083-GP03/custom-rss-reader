use crate::opml::{importer::parse_opml, exporter::export_opml as export_opml_impl};
use crate::database::schema::Subscription;

#[tauri::command]
pub async fn import_opml(
    pool: tauri::State<'_, sqlx::SqlitePool>,
    file_path: String,
) -> Result<ImportResult, String> {
    println!("[DEBUG] Import OPML from: {}", file_path);

    // Read OPML file
    let content = std::fs::read_to_string(&file_path)
        .map_err(|e| format!("Failed to read OPML file: {}", e))?;

    println!("[DEBUG] OPML content length: {} bytes", content.len());
    println!("[DEBUG] First 500 chars:\n{}", &content[..content.len().min(500)]);

    // Parse OPML
    let import_result = parse_opml(&content)
        .map_err(|e| format!("Failed to parse OPML: {}", e))?;

    println!("[DEBUG] Parsed {} feeds from OPML", import_result.feeds.len());

    let mut imported = 0;
    let mut skipped = 0;
    let mut errors = import_result.errors.clone();

    // Import feeds to database
    for feed in &import_result.feeds {
        println!("[DEBUG] Processing feed: url={}, title={:?}", feed.url, feed.title);

        // Check if subscription already exists
        let exists: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM subscriptions WHERE url = $1"
        )
        .bind(&feed.url)
        .fetch_optional(pool.inner())
        .await
        .map_err(|e| format!("Failed to check subscription existence: {}", e))?;

        if exists.is_none() {
            // Insert new subscription
            sqlx::query(
                r#"
                INSERT INTO subscriptions (url, title, website_url, opml_attributes)
                VALUES ($1, $2, $3, $4)
                "#
            )
            .bind(&feed.url)
            .bind(&feed.title)
            .bind(&feed.website_url)
            .bind(&feed.opml_attributes)
            .execute(pool.inner())
            .await
            .map_err(|e| {
                let err_msg = format!("Failed to import {}: {}", feed.url, e);
                errors.push(err_msg.clone());
                err_msg
            })?;

            println!("[DEBUG] Imported feed: {}", feed.url);
            imported += 1;
        } else {
            println!("[DEBUG] Skipped existing feed: {}", feed.url);
            skipped += 1;
        }
    }

    println!("[DEBUG] Import complete: imported={}, skipped={}", imported, skipped);

    Ok(ImportResult {
        imported,
        skipped,
        errors,
    })
}

#[tauri::command]
pub async fn export_opml(
    pool: tauri::State<'_, sqlx::SqlitePool>,
    file_path: String,
) -> Result<(), String> {
    // Get all subscriptions
    let subscriptions = sqlx::query_as::<_, Subscription>("SELECT * FROM subscriptions")
        .fetch_all(pool.inner())
        .await
        .map_err(|e| format!("Failed to fetch subscriptions: {}", e))?;

    // Export to OPML
    let opml_content = export_opml_impl(&subscriptions)
        .map_err(|e| format!("Failed to export OPML: {}", e))?;

    // Write to file
    std::fs::write(&file_path, opml_content)
        .map_err(|e| format!("Failed to write OPML file: {}", e))?;

    Ok(())
}

#[derive(serde::Serialize)]
pub struct ImportResult {
    pub imported: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

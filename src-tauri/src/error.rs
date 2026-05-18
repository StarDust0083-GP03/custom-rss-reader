use thiserror::Error;

/// Unified error type for all backend layers.
/// Replaces `Result<T, String>` throughout the codebase.
#[derive(Error, Debug)]
pub enum AppError {
    // --- Data layer errors ---
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Entity not found: {0}")]
    NotFound(String),

    #[error("Duplicate entry: {0}")]
    Duplicate(String),

    #[error("Validation error: {0}")]
    Validation(String),

    // --- Service / operation errors ---
    #[error("Operation failed: {0}")]
    OperationFailed(String),

    // --- External service errors ---
    #[error("Network error: {0}")]
    Network(String),

    #[error("Parse error: {0}")]
    Parse(String),

    // --- Internal / unexpected errors ---
    #[error("Internal error: {0}")]
    Internal(String),
}

/// Convenience type alias.
pub type Result<T> = std::result::Result<T, AppError>;

/// Serialize AppError as a human-readable string for Tauri IPC.
/// The frontend receives a plain string (matching the current `Result<T, String>` pattern),
/// so existing frontend error handling continues to work without changes.
impl serde::Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

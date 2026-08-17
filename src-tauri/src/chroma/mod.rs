pub mod service;
pub mod sync;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

/// Default ChromaDB server URL.
const DEFAULT_CHROMA_HOST: &str = "http://localhost";
const DEFAULT_CHROMA_PORT: u16 = 8000;
const DEFAULT_COLLECTION_NAME: &str = "rss_articles";

/// ChromaDB configuration, stored in ~/.rss-reader/chroma_config.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChromaConfig {
    pub host: String,
    pub port: u16,
    pub collection_name: String,
    pub enabled: bool,
}

impl Default for ChromaConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_CHROMA_HOST.to_string(),
            port: DEFAULT_CHROMA_PORT,
            collection_name: DEFAULT_COLLECTION_NAME.to_string(),
            enabled: false,
        }
    }
}

impl ChromaConfig {
    /// Path to the chroma config file.
    ///
    /// Returns an error if the user's home directory is unavailable — the
    /// startup path uses this and the result is then logged and the app
    /// continues without ChromaDB rather than panicking.
    pub fn config_path() -> Result<PathBuf> {
        let home_dir = dirs::home_dir()
            .ok_or_else(|| AppError::Internal("HOME directory not found".into()))?;
        Ok(home_dir.join(".rss-reader").join("chroma_config.json"))
    }

    /// Load config from disk, or return defaults if file doesn't exist.
    pub fn load() -> Self {
        Self::config_path()
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Save config to disk.
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AppError::Internal(format!("Failed to create config dir: {}", e)))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| AppError::Internal(format!("Failed to serialize chroma config: {}", e)))?;
        std::fs::write(&path, json)
            .map_err(|e| AppError::Internal(format!("Failed to write chroma config: {}", e)))?;
        Ok(())
    }

    /// Construct the full ChromaDB server URL.
    pub fn url(&self) -> String {
        let host = self.host.trim_end_matches('/');
        format!("{}:{}", host, self.port)
    }
}

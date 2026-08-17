pub mod service;
pub mod sync;

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

/// Default ChromaDB server URL.
const DEFAULT_CHROMA_HOST: &str = "http://localhost";
const DEFAULT_CHROMA_PORT: u16 = 8000;
const DEFAULT_COLLECTION_NAME: &str = "rss_articles";

/// Lazily-created, auto-reconnecting ChromaDB service holder.
///
/// The previous design connected ONCE at app startup: if the server wasn't
/// up at that moment, every later health check / search returned false
/// forever ("ChromaDB is not reachable") until the app was restarted —
/// even after the server came back. This holder creates the connection on
/// first use, caches it, and drops it when the server dies so the next
/// call reconnects.
#[derive(Clone, Default)]
pub struct ChromaHolder {
    inner: Arc<tokio::sync::Mutex<Option<Arc<service::ChromaService>>>>,
}

impl ChromaHolder {
    /// Return the cached service, or create one on demand when enabled and
    /// reachable. Never fails the caller — returns `None` when disabled or
    /// unreachable (callers decide how to surface that).
    pub async fn get(&self) -> Option<Arc<service::ChromaService>> {
        let mut guard = self.inner.lock().await;
        if let Some(svc) = guard.as_ref() {
            return Some(svc.clone());
        }
        let config = ChromaConfig::load();
        if !config.enabled {
            return None;
        }
        match service::ChromaService::new(&config).await {
            Ok(svc) => {
                let arc = Arc::new(svc);
                *guard = Some(arc.clone());
                Some(arc)
            }
            Err(e) => {
                eprintln!("[chroma] lazy connect failed: {}", e);
                None
            }
        }
    }

    /// Drop the cached connection (e.g. after a failed health check) so the
    /// next `get()` reconnects instead of staying stuck on a dead handle.
    pub async fn invalidate(&self) {
        *self.inner.lock().await = None;
    }
}

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

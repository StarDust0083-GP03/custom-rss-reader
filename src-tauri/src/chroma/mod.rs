pub mod backfill;
pub mod embeddings;
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
#[derive(Clone)]
pub struct ChromaHolder {
    inner: Arc<tokio::sync::Mutex<Option<Arc<service::ChromaService>>>>,
    /// Explicit configuration. `None` means "read the config file", which is
    /// what production does; tests inject their own so they never connect to a
    /// developer's running server or download a model.
    config: Option<Arc<ChromaConfig>>,
    /// Cached "semantic search is switched on" flag.
    ///
    /// Cached rather than read per call for two reasons: the worker asks on
    /// every queue poll, and — more importantly — a decision that changes the
    /// shape of the queue should be an explicit state change, not a side effect
    /// of whatever happens to be in the config file at that instant.
    enabled: Arc<std::sync::atomic::AtomicBool>,
}

impl Default for ChromaHolder {
    fn default() -> Self {
        Self::with_enabled(ChromaConfig::load().enabled)
    }
}

impl ChromaHolder {
    /// Holder with an explicit switch, for tests and for callers that already
    /// know the configuration.
    pub fn with_enabled(enabled: bool) -> Self {
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(None)),
            config: None,
            enabled: Arc::new(std::sync::atomic::AtomicBool::new(enabled)),
        }
    }

    /// Holder pinned to an explicit configuration.
    ///
    /// Used by tests so they never read a developer's `chroma_config.json` or
    /// connect to a running server; production uses [`Self::default`] and
    /// `refresh_from_config`.
    #[cfg(test)]
    pub fn with_config(config: ChromaConfig) -> Self {
        let enabled = config.enabled;
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(None)),
            config: Some(Arc::new(config)),
            enabled: Arc::new(std::sync::atomic::AtomicBool::new(enabled)),
        }
    }

    /// The configuration to connect with: the injected one, or the file.
    fn effective_config(&self) -> ChromaConfig {
        match &self.config {
            Some(config) => (**config).clone(),
            None => ChromaConfig::load(),
        }
    }

    /// Whether semantic search is configured, without connecting.
    pub fn is_configured(&self) -> bool {
        self.enabled.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Re-read the config file and adopt its `enabled` flag, dropping any
    /// cached connection when the feature was switched off.
    pub async fn refresh_from_config(&self) {
        let enabled = ChromaConfig::load().enabled;
        self.enabled
            .store(enabled, std::sync::atomic::Ordering::SeqCst);
        if !enabled {
            *self.inner.lock().await = None;
        }
    }

    /// Return the cached service, or create one on demand when enabled and
    /// reachable. Never fails the caller — returns `None` when disabled or
    /// unreachable (callers decide how to surface that).
    pub async fn get(&self) -> Option<Arc<service::ChromaService>> {
        if let Some(svc) = self.inner.lock().await.as_ref() {
            return Some(svc.clone());
        }
        if !self.is_configured() {
            return None;
        }
        let config = self.effective_config();
        // Connect OUTSIDE the lock: ChromaService::new performs network I/O
        // (identity lookup + get-or-create), and holding the holder's mutex
        // across it would stall every concurrent health check or search.
        // A double connect after a concurrent miss is harmless — the cached
        // handle is identical and upserts are idempotent.
        match service::ChromaService::new(&config).await {
            Ok(svc) => {
                let arc = Arc::new(svc);
                *self.inner.lock().await = Some(arc.clone());
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
    /// Validate configuration before it is persisted or used to connect.
    pub fn validate(&self) -> Result<()> {
        if self.host.trim().is_empty() {
            return Err(AppError::Validation("ChromaDB host cannot be empty".into()));
        }
        if self.port == 0 {
            return Err(AppError::Validation(
                "ChromaDB port must be greater than zero".into(),
            ));
        }
        if self.collection_name.trim().is_empty() {
            return Err(AppError::Validation(
                "ChromaDB collection name cannot be empty".into(),
            ));
        }
        let url = reqwest::Url::parse(&self.url()).map_err(|_| {
            AppError::Validation("ChromaDB host must be a valid http(s) URL".into())
        })?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(AppError::Validation(
                "ChromaDB host must be a valid http(s) URL".into(),
            ));
        }
        Ok(())
    }

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

#[cfg(test)]
mod tests {
    use super::ChromaConfig;

    #[test]
    fn valid_config_accepts_http_url() {
        let config = ChromaConfig {
            host: "http://localhost".into(),
            port: 8000,
            collection_name: "rss_articles".into(),
            enabled: true,
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn invalid_config_rejects_empty_fields_and_non_http_urls() {
        for config in [
            ChromaConfig {
                host: "".into(),
                ..ChromaConfig::default()
            },
            ChromaConfig {
                port: 0,
                ..ChromaConfig::default()
            },
            ChromaConfig {
                collection_name: "".into(),
                ..ChromaConfig::default()
            },
            ChromaConfig {
                host: "ftp://localhost".into(),
                ..ChromaConfig::default()
            },
        ] {
            assert!(config.validate().is_err());
        }
    }
}

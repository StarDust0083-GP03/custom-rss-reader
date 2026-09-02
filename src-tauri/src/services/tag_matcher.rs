//! Semantic matching of LLM-generated tag names onto the existing catalog.
//!
//! The classifier is asked to reuse catalog names, but models still invent
//! near-duplicates (`ml` next to `machine_learning`, `llms` next to
//! `large_language_models`). This service embeds any generated name that is
//! not already an exact catalog name or alias, compares it against the active
//! catalog with the local ONNX sentence-embedding model, and silently rewrites
//! it to the closest catalog entry when the cosine similarity is at or above
//! the user-configured threshold. Every applied match is persisted as an
//! alias so the next occurrence resolves exactly, without an embedding call.
//!
//! No LLM or ChromaDB server is involved — this is the same in-process
//! embedder that semantic search uses, so it works even when Chroma is off.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use chromadb::embeddings::EmbeddingFunction;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};

use crate::chroma::embeddings::OnnxEmbeddingFunction;
use crate::error::{AppError, Result};
use crate::models::tag::normalize_tag;
use crate::repositories::FeedItemRepository;

/// Default threshold for silently snapping a generated name onto an existing
/// catalog tag. Deliberately conservative: below this the name is kept as a
/// new tag and only surfaces in the review-only cluster suggestions.
pub const DEFAULT_MATCH_THRESHOLD: f32 = 0.85;
const MIN_MATCH_THRESHOLD: f32 = 0.5;
const MAX_MATCH_THRESHOLD: f32 = 1.0;

/// User settings for automatic tag matching, stored in
/// `~/.rss-reader/tag_config.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TagMatchConfig {
    /// When false, generated names are only normalized and alias-resolved.
    pub enabled: bool,
    /// Cosine similarity at or above which a generated name is rewritten to
    /// the closest catalog tag. Range `[0.5, 1.0]`.
    pub similarity_threshold: f32,
}

impl Default for TagMatchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            similarity_threshold: DEFAULT_MATCH_THRESHOLD,
        }
    }
}

impl TagMatchConfig {
    pub fn validate(&self) -> Result<()> {
        if !self.similarity_threshold.is_finite()
            || !(MIN_MATCH_THRESHOLD..=MAX_MATCH_THRESHOLD).contains(&self.similarity_threshold)
        {
            return Err(AppError::Validation(format!(
                "Similarity threshold must be between {} and {}",
                MIN_MATCH_THRESHOLD, MAX_MATCH_THRESHOLD
            )));
        }
        Ok(())
    }

    pub fn config_path() -> Result<PathBuf> {
        let home_dir = dirs::home_dir()
            .ok_or_else(|| AppError::Internal("HOME directory not found".into()))?;
        Ok(home_dir.join(".rss-reader").join("tag_config.json"))
    }

    /// Load from disk; missing or unreadable files fall back to defaults.
    /// An out-of-range persisted threshold also falls back to the default so a
    /// hand-edited file cannot silently disable or over-merge tags.
    pub fn load() -> Self {
        let loaded: Self = Self::config_path()
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if loaded.validate().is_err() {
            return Self {
                enabled: loaded.enabled,
                ..Self::default()
            };
        }
        loaded
    }

    pub fn save(&self) -> Result<()> {
        self.validate()?;
        let path = Self::config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AppError::Internal(format!("Failed to create config dir: {}", e)))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| AppError::Internal(format!("Failed to serialize tag config: {}", e)))?;
        std::fs::write(&path, json)
            .map_err(|e| AppError::Internal(format!("Failed to write tag config: {}", e)))?;
        Ok(())
    }
}

/// Process-wide local embedder shared by tag matching and tag clustering so
/// the ONNX model is loaded at most once.
pub fn shared_tag_embedder() -> OnnxEmbeddingFunction {
    static EMBEDDER: OnceLock<OnnxEmbeddingFunction> = OnceLock::new();
    EMBEDDER.get_or_init(OnnxEmbeddingFunction::new).clone()
}

/// Snaps generated tag names onto the existing catalog using local embeddings.
pub struct TagMatcher {
    embedder: Arc<dyn EmbeddingFunction>,
    config: RwLock<TagMatchConfig>,
    /// Name → embedding. Tag names are short and embeddings depend only on
    /// the string itself, so the cache never needs invalidation.
    cache: Mutex<HashMap<String, Vec<f32>>>,
}

impl TagMatcher {
    pub fn new(embedder: Arc<dyn EmbeddingFunction>, config: TagMatchConfig) -> Self {
        Self {
            embedder,
            config: RwLock::new(config),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Matcher backed by the shared local ONNX model and the persisted config.
    pub fn local() -> Self {
        Self::new(Arc::new(shared_tag_embedder()), TagMatchConfig::load())
    }

    pub async fn config(&self) -> TagMatchConfig {
        self.config.read().await.clone()
    }

    /// Persist and apply new settings. Takes effect for the next classification.
    pub async fn set_config(&self, config: TagMatchConfig) -> Result<()> {
        config.save()?;
        *self.config.write().await = config;
        Ok(())
    }

    /// Embed names with the local model, reusing cached vectors.
    pub async fn embed(&self, names: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut cache = self.cache.lock().await;
        let missing: Vec<&str> = names
            .iter()
            .filter(|name| !cache.contains_key(name.as_str()))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            let vectors = self
                .embedder
                .embed(&missing)
                .await
                .map_err(|error| AppError::OperationFailed(format!("Embed tags: {}", error)))?;
            if vectors.len() != missing.len() {
                return Err(AppError::OperationFailed(
                    "Embed tags: embedder returned a different number of vectors".into(),
                ));
            }
            for (name, vector) in missing.into_iter().zip(vectors) {
                cache.insert(name.to_string(), vector);
            }
        }
        Ok(names
            .iter()
            .map(|name| cache.get(name.as_str()).cloned().unwrap_or_default())
            .collect())
    }

    /// Resolve generated names to canonical catalog names.
    ///
    /// Steps, in order: normalize to `snake_case`, drop blocked names, follow
    /// aliases, deduplicate, and — when matching is enabled — embed each
    /// remaining unknown name and rewrite it to the closest catalog tag whose
    /// similarity meets the threshold. Applied matches are stored as aliases.
    ///
    /// Embedding failures (for example the model cannot be downloaded) are
    /// logged and degrade to the exact-resolution result so article
    /// classification never fails because of the matcher.
    pub async fn resolve(
        &self,
        repo: &dyn FeedItemRepository,
        proposed: &[String],
    ) -> Result<Vec<String>> {
        let catalog = repo.find_tag_catalog().await?;
        let blocked: std::collections::HashSet<String> =
            repo.find_blocked_tags().await?.into_iter().collect();
        let mut aliases: HashMap<String, String> = HashMap::new();
        for entry in &catalog {
            for alias in &entry.aliases {
                aliases.insert(alias.clone(), entry.name.clone());
            }
        }

        let mut resolved: Vec<String> = Vec::new();
        for raw in proposed {
            let Some(mut tag) = normalize_tag(raw) else {
                continue;
            };
            if blocked.contains(&tag) {
                continue;
            }
            if let Some(canonical) = aliases.get(&tag) {
                tag = canonical.clone();
            }
            if blocked.contains(&tag) {
                continue;
            }
            if !resolved.contains(&tag) {
                resolved.push(tag);
            }
        }

        let config = self.config().await;
        let catalog_names: Vec<String> = catalog.into_iter().map(|entry| entry.name).collect();
        if !config.enabled || catalog_names.is_empty() {
            return Ok(resolved);
        }
        let unknown: Vec<String> = resolved
            .iter()
            .filter(|tag| !catalog_names.contains(tag))
            .cloned()
            .collect();
        if unknown.is_empty() {
            return Ok(resolved);
        }

        let (catalog_vectors, unknown_vectors) =
            match (self.embed(&catalog_names).await, self.embed(&unknown).await) {
                (Ok(catalog_vectors), Ok(unknown_vectors)) => (catalog_vectors, unknown_vectors),
                (Err(error), _) | (_, Err(error)) => {
                    eprintln!("Tag matching skipped: {}", error);
                    return Ok(resolved);
                }
            };

        let mut replacements: HashMap<String, String> = HashMap::new();
        for (name, vector) in unknown.iter().zip(&unknown_vectors) {
            if let Some(index) = best_match(vector, &catalog_vectors, config.similarity_threshold) {
                let head = catalog_names[index].clone();
                if let Err(error) = repo.add_tag_alias(name, &head).await {
                    eprintln!(
                        "Tag matching: could not store alias {} -> {}: {}",
                        name, head, error
                    );
                }
                replacements.insert(name.clone(), head);
            }
        }
        if replacements.is_empty() {
            return Ok(resolved);
        }

        let mut snapped: Vec<String> = Vec::new();
        for tag in resolved {
            let tag = replacements.get(&tag).cloned().unwrap_or(tag);
            if !snapped.contains(&tag) {
                snapped.push(tag);
            }
        }
        Ok(snapped)
    }
}

/// Index of the most similar catalog vector, if its cosine similarity is at
/// or above `threshold`. Ties resolve to the lowest index, which keeps the
/// result deterministic for a sorted catalog.
pub fn best_match(candidate: &[f32], catalog: &[Vec<f32>], threshold: f32) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (index, vector) in catalog.iter().enumerate() {
        let similarity = cosine_similarity(candidate, vector);
        if similarity < threshold {
            continue;
        }
        match best {
            Some((_, best_similarity)) if best_similarity >= similarity => {}
            _ => best = Some((index, similarity)),
        }
    }
    best.map(|(index, _)| index)
}

pub fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    let (dot, left_norm, right_norm) = left.iter().zip(right).fold(
        (0.0f32, 0.0f32, 0.0f32),
        |(dot, left_norm, right_norm), (left, right)| {
            (
                dot + left * right,
                left_norm + left * left,
                right_norm + right * right,
            )
        },
    );
    let denominator = (left_norm * right_norm).sqrt();
    if denominator == 0.0 {
        0.0
    } else {
        dot / denominator
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_similarity_handles_normalized_and_zero_vectors() {
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < f32::EPSILON);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
    }

    #[test]
    fn best_match_respects_threshold_and_prefers_closest() {
        let catalog = vec![vec![1.0, 0.0], vec![0.6, 0.8], vec![0.0, 1.0]];
        assert_eq!(best_match(&[0.7, 0.71], &catalog, 0.9), Some(1));
        assert_eq!(best_match(&[0.99, 0.1], &catalog, 0.9), Some(0));
        assert_eq!(best_match(&[-1.0, 0.0], &catalog, 0.5), None);
        // Exactly at the threshold counts as a match.
        assert_eq!(best_match(&[1.0, 0.0], &catalog, 1.0), Some(0));
    }

    #[test]
    fn config_validation_bounds_threshold() {
        assert!(TagMatchConfig::default().validate().is_ok());
        let low = TagMatchConfig {
            enabled: true,
            similarity_threshold: 0.2,
        };
        assert!(low.validate().is_err());
        let high = TagMatchConfig {
            enabled: true,
            similarity_threshold: 1.01,
        };
        assert!(high.validate().is_err());
        let nan = TagMatchConfig {
            enabled: true,
            similarity_threshold: f32::NAN,
        };
        assert!(nan.validate().is_err());
    }

    #[test]
    fn config_deserializes_with_defaults_for_missing_fields() {
        let parsed: TagMatchConfig = serde_json::from_str("{\"enabled\": false}").unwrap();
        assert!(!parsed.enabled);
        assert_eq!(parsed.similarity_threshold, DEFAULT_MATCH_THRESHOLD);
    }
}

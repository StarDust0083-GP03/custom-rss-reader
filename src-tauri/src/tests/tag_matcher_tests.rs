//! Tag matcher behaviour against the real SQLite catalog with a fake embedder.
//!
//! The ONNX model is never loaded here: the embedder maps known names to fixed
//! vectors so similarity outcomes are deterministic and the tests stay fast
//! and offline.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chromadb::embeddings::EmbeddingFunction;

use super::helpers::TestEnv;
use crate::services::tag_matcher::{TagMatchConfig, TagMatcher};

/// Deterministic embedder: known names get fixed 2-D vectors, unknown names
/// get an orthogonal "far away" vector. Counts how many names were embedded.
struct FakeEmbedder {
    vectors: HashMap<&'static str, Vec<f32>>,
    calls: AtomicUsize,
}

impl FakeEmbedder {
    fn new() -> Arc<Self> {
        let mut vectors = HashMap::new();
        vectors.insert("machine_learning", vec![1.0, 0.0]);
        // ~0.95 cosine with machine_learning
        vectors.insert("deep_learning", vec![0.95, 0.312]);
        // ~0.80 cosine with machine_learning
        vectors.insert("neural_networks", vec![0.8, 0.6]);
        vectors.insert("cooking", vec![0.0, 1.0]);
        Arc::new(Self {
            vectors,
            calls: AtomicUsize::new(0),
        })
    }

    fn embedded_names(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl EmbeddingFunction for FakeEmbedder {
    async fn embed(&self, docs: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(docs.len(), Ordering::SeqCst);
        Ok(docs
            .iter()
            .map(|doc| {
                self.vectors
                    .get(*doc)
                    .cloned()
                    .unwrap_or_else(|| vec![-1.0, 0.0])
            })
            .collect())
    }
}

struct FailingEmbedder;

#[async_trait]
impl EmbeddingFunction for FailingEmbedder {
    async fn embed(&self, _docs: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::bail!("model download failed")
    }
}

fn matcher(embedder: Arc<dyn EmbeddingFunction>, threshold: f32, enabled: bool) -> TagMatcher {
    TagMatcher::new(
        embedder,
        TagMatchConfig {
            enabled,
            similarity_threshold: threshold,
        },
    )
}

fn strings(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

#[tokio::test]
async fn resolve_snaps_similar_names_and_persists_alias() {
    let env = TestEnv::new().await;
    env.feed_repo.create_tag("machine_learning").await.unwrap();
    let embedder = FakeEmbedder::new();
    let matcher = matcher(embedder.clone(), 0.85, true);

    let resolved = matcher
        .resolve(
            env.feed_repo.as_ref(),
            &strings(&["Deep Learning", "neural_networks", "cooking"]),
        )
        .await
        .unwrap();

    // deep_learning (0.95) snaps; neural_networks (0.80) and cooking stay new.
    assert_eq!(
        resolved,
        vec!["machine_learning", "neural_networks", "cooking"]
    );

    // The applied match is persisted as an alias of the head.
    let catalog = env.feed_repo.find_tag_catalog().await.unwrap();
    let head = catalog
        .iter()
        .find(|entry| entry.name == "machine_learning")
        .unwrap();
    assert_eq!(head.aliases, vec!["deep_learning"]);
    // Unmatched names are NOT inserted by the matcher; save_tags owns inserts.
    assert_eq!(catalog.len(), 1);

    // The next occurrence resolves through the alias without embedding it.
    let before = embedder.embedded_names();
    let again = matcher
        .resolve(env.feed_repo.as_ref(), &strings(&["deep_learning"]))
        .await
        .unwrap();
    assert_eq!(again, vec!["machine_learning"]);
    assert_eq!(embedder.embedded_names(), before);
}

#[tokio::test]
async fn resolve_deduplicates_after_snapping_and_saves_canonically() {
    let env = TestEnv::new().await;
    env.feed_repo.create_tag("machine_learning").await.unwrap();
    let sub_id = env
        .repo
        .create(super::helpers::new_sub("https://example.com/feed.xml"))
        .await
        .unwrap()
        .id;
    let item = env
        .feed_repo
        .create(crate::models::NewFeedItem {
            subscription_id: sub_id,
            guid: Some("matcher-1".into()),
            title: "Matcher item".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let matcher = matcher(FakeEmbedder::new(), 0.85, true);

    let resolved = matcher
        .resolve(
            env.feed_repo.as_ref(),
            &strings(&["machine_learning", "deep_learning", "cooking"]),
        )
        .await
        .unwrap();
    assert_eq!(resolved, vec!["machine_learning", "cooking"]);

    let saved = env
        .feed_repo
        .save_tags(item.id, &serde_json::to_string(&resolved).unwrap(), "tech")
        .await
        .unwrap();
    let tags: Vec<String> = serde_json::from_str(&saved.tags.unwrap()).unwrap();
    assert_eq!(tags, vec!["machine_learning", "cooking"]);
}

#[tokio::test]
async fn threshold_and_enabled_flag_control_matching() {
    let env = TestEnv::new().await;
    env.feed_repo.create_tag("machine_learning").await.unwrap();

    // A lower threshold also captures neural_networks (0.80).
    let loose = matcher(FakeEmbedder::new(), 0.75, true);
    let resolved = loose
        .resolve(env.feed_repo.as_ref(), &strings(&["neural_networks"]))
        .await
        .unwrap();
    assert_eq!(resolved, vec!["machine_learning"]);

    // Disabled matching keeps unknown names untouched even when similar.
    let env = TestEnv::new().await;
    env.feed_repo.create_tag("machine_learning").await.unwrap();
    let embedder = FakeEmbedder::new();
    let disabled = matcher(embedder.clone(), 0.5, false);
    let resolved = disabled
        .resolve(env.feed_repo.as_ref(), &strings(&["deep_learning"]))
        .await
        .unwrap();
    assert_eq!(resolved, vec!["deep_learning"]);
    assert_eq!(embedder.embedded_names(), 0);
}

#[tokio::test]
async fn resolve_never_maps_onto_blocked_names_and_survives_embedder_errors() {
    let env = TestEnv::new().await;
    env.feed_repo.create_tag("machine_learning").await.unwrap();
    env.feed_repo.create_tag("cooking").await.unwrap();
    env.feed_repo.delete_tag("cooking").await.unwrap();

    let matcher = matcher(FakeEmbedder::new(), 0.85, true);
    let resolved = matcher
        .resolve(
            env.feed_repo.as_ref(),
            &strings(&["cooking", "deep_learning"]),
        )
        .await
        .unwrap();
    // Blocked names are dropped before matching; deep_learning still snaps.
    assert_eq!(resolved, vec!["machine_learning"]);

    // Embedding failure degrades to exact resolution instead of failing.
    let failing = self::matcher(Arc::new(FailingEmbedder), 0.85, true);
    let resolved = failing
        .resolve(env.feed_repo.as_ref(), &strings(&["quantum_computing"]))
        .await
        .unwrap();
    assert_eq!(resolved, vec!["quantum_computing"]);
}

#[tokio::test]
async fn add_tag_alias_rejects_shadowing_active_or_blocked_names() {
    let env = TestEnv::new().await;
    env.feed_repo.create_tag("machine_learning").await.unwrap();
    env.feed_repo.create_tag("databases").await.unwrap();
    env.feed_repo.create_tag("spam").await.unwrap();
    env.feed_repo.delete_tag("spam").await.unwrap();

    assert!(env
        .feed_repo
        .add_tag_alias("databases", "machine_learning")
        .await
        .is_err());
    assert!(env
        .feed_repo
        .add_tag_alias("spam", "machine_learning")
        .await
        .is_err());
    assert!(env
        .feed_repo
        .add_tag_alias("ml", "does_not_exist")
        .await
        .is_err());

    env.feed_repo
        .add_tag_alias("ml", "machine_learning")
        .await
        .unwrap();
    // Idempotent.
    env.feed_repo
        .add_tag_alias("ml", "machine_learning")
        .await
        .unwrap();
    let catalog = env.feed_repo.find_tag_catalog().await.unwrap();
    let head = catalog
        .iter()
        .find(|entry| entry.name == "machine_learning")
        .unwrap();
    assert_eq!(head.aliases, vec!["ml"]);
}

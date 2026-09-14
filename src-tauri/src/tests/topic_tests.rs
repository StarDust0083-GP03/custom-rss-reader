//! The topic navigation layer and the community map's source data.
//!
//! Raw classifier output remains available for reversible vocabulary changes,
//! but every user-facing Topic and Community read uses canonical display tags.

use std::collections::HashSet;

use crate::models::NewFeedItem;
use crate::models::NewSubscription;
use crate::repositories::{TopicAssignment, TopicCategory};

use super::helpers::TestEnv;

async fn seed_item_with_tags(env: &TestEnv, title: &str, raw: &[&str]) -> i64 {
    let sub_id = env
        .service
        .add_subscription(NewSubscription {
            url: format!("https://example.com/{title}"),
            title: Some("Topic Sub".into()),
            ..Default::default()
        })
        .await
        .expect("Failed to seed subscription")
        .id;
    let item = env
        .feed_service
        .create_item(NewFeedItem {
            subscription_id: sub_id,
            title: title.into(),
            ..Default::default()
        })
        .await
        .expect("Failed to create feed item");
    let raw_json = serde_json::to_string(raw).unwrap();
    env.feed_repo
        .save_tags(item.id, &raw_json)
        .await
        .expect("Failed to save tags");
    item.id
}

#[tokio::test]
async fn raw_names_survive_the_display_cap() {
    let env = TestEnv::new().await;
    let id = seed_item_with_tags(
        &env,
        "cap",
        &["alpha", "beta", "gamma", "delta", "epsilon"],
    )
    .await;

    let item = env.feed_repo.find_by_id(id).await.unwrap();
    let displayed: Vec<String> = serde_json::from_str(item.tags.as_deref().unwrap()).unwrap();
    // `raw_tags` is storage-only (the UI never renders it), so read it from the
    // column the reversibility promise depends on.
    let raw_json: Option<String> = sqlx::query_scalar("SELECT raw_tags FROM feed_items WHERE id = $1")
        .bind(id)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    let raw: Vec<String> = serde_json::from_str(raw_json.as_deref().unwrap()).unwrap();

    // At most three names are displayed, but the classifier's whole answer is
    // kept: dropping the rest here is what made an adoption change lossy.
    assert_eq!(displayed.len(), 3, "display cap still applies: {displayed:?}");
    assert_eq!(
        raw,
        vec!["alpha", "beta", "gamma", "delta", "epsilon"],
        "every proposed name must be recorded"
    );
}

#[tokio::test]
async fn overview_counts_canonical_names_after_a_mapping_rewrites_the_display() {
    let env = TestEnv::new().await;
    env.feed_repo.create_tag("machine_learning").await.unwrap();
    let id = seed_item_with_tags(&env, "raw", &["deep_learning"]).await;

    // Fold the article's own name onto a synonym: the display column now says
    // machine_learning, while the raw column still says deep_learning.
    env.feed_repo
        .merge_tags("machine_learning", &["deep_learning".to_string()])
        .await
        .unwrap();
    let item = env.feed_repo.find_by_id(id).await.unwrap();
    assert_eq!(item.tags.as_deref(), Some(r#"["machine_learning"]"#));
    let usage = env.feed_repo.find_tag_usage(None).await.unwrap();
    assert_eq!(usage.get("machine_learning").copied(), Some(1));
    assert!(!usage.contains_key("deep_learning"), "raw aliases must not enter the map: {usage:?}");
}

#[tokio::test]
async fn catalog_and_overview_usage_share_the_canonical_name_space() {
    let env = TestEnv::new().await;
    env.feed_repo.create_tag("machine_learning").await.unwrap();
    seed_item_with_tags(&env, "raw-name", &["deep_learning"]).await;

    env.feed_repo
        .merge_tags("machine_learning", &["deep_learning".to_string()])
        .await
        .unwrap();

    let catalog = env.feed_repo.find_tag_catalog().await.unwrap();
    assert_eq!(
        catalog
            .iter()
            .find(|entry| entry.name == "machine_learning")
            .map(|entry| entry.usage_count),
        Some(1),
        "catalog usage follows the display column"
    );
    let usage = env.feed_repo.find_tag_usage(None).await.unwrap();
    assert_eq!(usage.get("machine_learning").copied(), Some(1));
    assert!(!usage.contains_key("deep_learning"));
}

#[tokio::test]
async fn cooccurrence_and_coverage_describe_the_scope_honestly() {
    let env = TestEnv::new().await;
    seed_item_with_tags(&env, "a", &["docker", "containerization"]).await;
    seed_item_with_tags(&env, "b", &["docker"]).await;
    let untagged = seed_item_with_tags(&env, "c", &[]).await;

    let edges = env.feed_repo.find_tag_cooccurrence(None).await.unwrap();
    let pair = edges
        .iter()
        .find(|(left, right, _)| left == "containerization" && right == "docker")
        .expect("the shared article must produce one edge");
    assert_eq!(pair.2, 1, "one article shares these two names");

    let coverage = env.feed_repo.tag_overview_coverage(None).await.unwrap();
    assert_eq!(coverage.total_items, 3);
    assert_eq!(coverage.tagged_items, 2);
    assert_eq!(coverage.unreadable_items, 0);

    // A territory counts articles, so an article carrying two of its names is
    // counted once rather than once per name. The overview builds this union
    // from the one canonical `(article_id, tag)` query.
    let tag_rows = env.feed_repo.find_tag_items(None).await.unwrap();
    let both: HashSet<i64> = tag_rows
        .into_iter()
        .filter(|(_, tag)| tag == "docker" || tag == "containerization")
        .map(|(item_id, _)| item_id)
        .collect();
    assert_eq!(both.len(), 2);

    // An article with no readable tags must be reported, not silently counted
    // as covered.
    sqlx::query("UPDATE feed_items SET tags = '{not json' WHERE id = $1")
        .bind(untagged)
        .execute(&env.pool)
        .await
        .unwrap();
    let coverage = env.feed_repo.tag_overview_coverage(None).await.unwrap();
    assert_eq!(coverage.unreadable_items, 1);
    assert_eq!(coverage.tagged_items, 2);
}

#[tokio::test]
async fn topic_catalog_is_seeded_and_assignments_replace_wholesale() {
    let env = TestEnv::new().await;

    let seeded = env.feed_repo.find_topic_categories().await.unwrap();
    assert_eq!(seeded.len(), 40, "the shipped catalog is the starting point");
    assert_eq!(seeded[0].id, 1);
    assert!(!seeded[0].label.trim().is_empty());

    let mut categories = seeded.clone();
    categories[0].label = "Models and training".to_string();
    let assignments = vec![TopicAssignment {
        tag_name: "docker".to_string(),
        category_id: Some(12),
        state: "assigned".to_string(),
        source: "manual".to_string(),
    }];
    env.feed_repo
        .replace_topic_state(&categories, &assignments)
        .await
        .unwrap();

    let stored = env.feed_repo.find_topic_categories().await.unwrap();
    assert_eq!(stored[0].label, "Models and training");
    assert_eq!(stored.len(), 40, "topics are never dropped by a save");

    // Saving again without the assignment removes it: the workspace owns the
    // whole set, so a removed row means a removed decision.
    env.feed_repo
        .replace_topic_state(&categories, &[])
        .await
        .unwrap();
    assert!(env
        .feed_repo
        .find_topic_assignments()
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn assignment_states_cannot_disagree_with_the_topic_column() {
    let env = TestEnv::new().await;

    // The database refuses a half-decided row even if a future caller forgets
    // to validate: "context_only" with a topic, and "assigned" without one.
    let half = sqlx::query(
        "INSERT INTO tag_topic_assignments (tag_name, category_id, state, source)
         VALUES ($1, $2, $3, 'manual')",
    )
    .bind("rust")
    .bind(5_i64)
    .bind("context_only")
    .execute(&env.pool)
    .await;
    assert!(half.is_err(), "context_only must not carry a topic");

    let empty = sqlx::query(
        "INSERT INTO tag_topic_assignments (tag_name, category_id, state, source)
         VALUES ($1, NULL, $2, 'manual')",
    )
    .bind("rust")
    .bind("assigned")
    .execute(&env.pool)
    .await;
    assert!(empty.is_err(), "assigned must carry a topic");

    let unknown_state = sqlx::query(
        "INSERT INTO tag_topic_assignments (tag_name, category_id, state, source)
         VALUES ($1, NULL, 'undecided', 'manual')",
    )
    .bind("rust")
    .execute(&env.pool)
    .await;
    assert!(
        unknown_state.is_err(),
        "'undecided' is a response-only value; the table stores decided states"
    );
}

#[tokio::test]
async fn topic_catalog_refuses_ids_beyond_the_navigation_ceiling() {
    let env = TestEnv::new().await;
    let overflow: Result<_, _> = sqlx::query(
        "INSERT INTO topic_categories (id, label, definition, sort_order)
         VALUES (50, 'One too many', '', 50)",
    )
    .execute(&env.pool)
    .await;
    assert!(
        overflow.is_err(),
        "the ≤50 navigation promise is enforced in the schema, not by convention"
    );
}

#[tokio::test]
async fn a_new_unknown_word_never_creates_a_topic() {
    let env = TestEnv::new().await;
    let before = env.feed_repo.find_topic_categories().await.unwrap().len();
    seed_item_with_tags(&env, "new", &["quantum_error_correction"]).await;
    let after = env.feed_repo.find_topic_categories().await.unwrap().len();
    assert_eq!(
        before, after,
        "classification may add vocabulary, never a navigation entry"
    );
    // It is known as a word, so the workspace can ask a human about it.
    let catalog = env.feed_repo.find_tag_catalog().await.unwrap();
    assert!(catalog.iter().any(|entry| entry.name == "quantum_error_correction"));
}

/// Helper so the compiler keeps `TopicCategory` in scope for the tests above
/// that only read the seeded rows.
#[allow(dead_code)]
fn category_label(category: &TopicCategory) -> &str {
    &category.label
}

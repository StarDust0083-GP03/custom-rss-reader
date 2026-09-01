//! Live end-to-end test for the ChromaDB semantic features and the AI
//! read-recommendation feature.
//!
//! This test runs against REAL infrastructure — it must never run in CI:
//!   - requires the local ChromaDB server (scripts/setup-chroma.sh)
//!   - requires the user's real `~/.rss-reader/rss_reader.db` (opened
//!     READ-ONLY — the sync only reads the DB)
//!   - requires the user's real AI config; the recommend step makes a live
//!     LLM call (billed against their key)
//!
//! Side effects: writes the real `~/.rss-reader/chroma_sync.json` watermark
//! and indexes items into the real ChromaDB collection — i.e. it performs
//! the same startup sync the app would. That is the point: after this test
//! the environment is "ready" for the app.
//!
//! Run with: cargo test -- --ignored --nocapture chroma_e2e

use std::sync::Arc;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::ai::service::{AiService, LlmAiService};
use crate::ai::AiConfig;
use crate::chroma::service::ChromaService;
use crate::chroma::{ChromaConfig, ChromaHolder};
use crate::repositories::feed_item_repo::SqliteFeedItemRepository;
use crate::repositories::subscription_repo::SqliteSubscriptionRepository;
use crate::repositories::{FeedItemRepository, SubscriptionRepository};

/// How many recent unread items are offered to the LLM (mirrors the
/// `recommend_reads` command).
const RECOMMEND_CANDIDATES: i64 = 10;

#[tokio::test]
#[ignore]
async fn chroma_and_recommend_e2e() {
    // 1. Config — real user config.
    let chroma_config = ChromaConfig::load();
    if !chroma_config.enabled {
        eprintln!("SKIP: ChromaDB disabled in ~/.rss-reader/chroma_config.json");
        return;
    }

    // 2. Open the real DB read-only — the sync path only reads it.
    let home = dirs::home_dir().expect("HOME");
    let db_path = home.join(".rss-reader").join("rss_reader.db");
    assert!(db_path.exists(), "DB not found at {}", db_path.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&db_path)
                .read_only(true),
        )
        .await
        .expect("open read-only DB");

    let feed_repo: Arc<dyn FeedItemRepository> =
        Arc::new(SqliteFeedItemRepository::new(pool.clone()));
    let sub_repo: Arc<dyn SubscriptionRepository> =
        Arc::new(SqliteSubscriptionRepository::new(pool.clone()));

    // 3. Connect to ChromaDB — fail loudly when the server is down.
    let chroma = ChromaService::new(&chroma_config)
        .await
        .expect("ChromaDB server must be running (scripts/setup-chroma.sh)");

    // 4. Incremental sync — indexes everything above the persisted
    //    watermark (first run: the whole library). Reads the DB, writes
    //    only the chroma sync state.
    let report = crate::chroma::sync::incremental_sync(&feed_repo, &chroma)
        .await
        .expect("incremental sync");
    println!(
        "\n[1/4] SYNC  indexed={} deleted={} pages={} in {}ms",
        report.indexed, report.deleted, report.pages, report.duration_ms
    );

    // 5. Semantic search — query with a real article's title.
    let max_id = feed_repo.max_item_id().await.expect("max id");
    let anchor = feed_repo
        .find_by_id(max_id)
        .await
        .expect("anchor item exists");
    let hits = chroma.search(&anchor.title, 5).await.expect("search");
    println!(
        "[2/4] SEARCH  query={:?}",
        &anchor.title[..anchor.title.len().min(50)]
    );
    for h in &hits {
        println!(
            "      hit  score={:.4} id={} title={}",
            h.score,
            h.item_id,
            truncate(&h.title)
        );
    }
    assert!(
        hits.iter().any(|h| h.item_id == anchor.id),
        "the item itself should be the top semantic hit"
    );

    // 6. Related articles — the "Find Similar" path.
    let similar = chroma.find_similar(&anchor, 5).await.expect("find similar");
    println!("[3/4] SIMILAR  anchor={:?}", truncate(&anchor.title));
    for s in &similar {
        println!(
            "      related  score={:.4} id={} title={}",
            s.score,
            s.item_id,
            truncate(&s.title)
        );
    }
    assert!(
        similar.iter().all(|s| s.item_id != anchor.id),
        "anchor item must be excluded from its own similar list"
    );

    // 7. Recommendations — one live LLM call with the real API key.
    let ai_config: AiConfig = {
        let path = home.join(".rss-reader").join("ai_config.json");
        let raw = std::fs::read_to_string(&path)
            .expect("ai_config.json exists (set the API key in the app first)");
        serde_json::from_str(&raw).expect("ai_config.json parses (legacy files are supported)")
    };
    assert!(ai_config.is_valid().is_ok(), "AI config must be complete");

    let unread = feed_repo
        .get_unread(None, RECOMMEND_CANDIDATES, 0)
        .await
        .expect("unread summaries");
    let subs = sub_repo.find_all().await.expect("subscriptions");
    let candidates: Vec<crate::ai::RecommendCandidate> = unread
        .iter()
        .map(|s| {
            let source = subs
                .iter()
                .find(|sub| sub.id == s.subscription_id)
                .map(|sub| sub.title.clone().unwrap_or_else(|| sub.url.clone()))
                .unwrap_or_else(|| "Unknown".into());
            let mut context = format!("{} — {}", source, s.title);
            if let Some(desc) = s.description.as_deref() {
                let plain = crate::chroma::service::strip_html_tags(desc);
                let snippet: String = plain.chars().take(140).collect();
                if !snippet.trim().is_empty() {
                    context.push_str(&format!(" — {}", snippet.trim()));
                }
            }
            crate::ai::RecommendCandidate {
                item_id: s.id,
                context,
            }
        })
        .collect();
    assert!(!candidates.is_empty(), "need at least one unread item");

    let ai_service = LlmAiService::new(ai_config).expect("AI service");
    let picks = ai_service
        .recommend_reads(&candidates)
        .await
        .expect("live LLM recommend call");
    println!(
        "[4/4] RECOMMEND  {} unread candidates, {} picks",
        candidates.len(),
        picks.len()
    );
    for p in &picks {
        println!("      pick  id={} reason={}", p.item_id, p.reason);
    }

    // The holder path is what commands use — make sure it resolves too.
    let holder = ChromaHolder::default();
    let via_holder = holder.get().await;
    assert!(via_holder.is_some(), "ChromaHolder must resolve a service");
    println!("PASS: chroma e2e (sync/search/similar) + recommend (LLM)");
}

fn truncate(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= 60 {
        s.to_string()
    } else {
        chars.iter().take(60).collect()
    }
}

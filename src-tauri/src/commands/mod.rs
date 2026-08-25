pub mod ai_commands;
pub mod chroma_commands;
pub mod feed_commands;
pub mod item_commands;
pub mod streaming;
pub mod subscription_commands;
pub mod webview;

#[cfg(debug_assertions)]
pub mod debug;

pub use ai_commands::*;
pub use chroma_commands::*;
pub use feed_commands::*;
pub use item_commands::*;
pub use streaming::*;
pub use subscription_commands::*;
pub use webview::*;

#[cfg(debug_assertions)]
pub use debug::*;

use std::sync::Arc;

use crate::ai::activity::AiActivityStore;
use crate::ai::service::SharedAiService;
use crate::feed::FeedFetcher;
use crate::repositories::FeedItemRepository;
use crate::{FeedService, SubscriptionService};

/// Application state managed by Tauri.
///
/// Holds all service instances and shared infrastructure.
/// Services are the canonical way to access business logic;
/// repositories and the pool are exposed for command convenience.
pub struct AppState {
    pub subscription_service: SubscriptionService,
    pub feed_service: FeedService,
    pub feed_repo: Arc<dyn FeedItemRepository>,
    pub fetcher: Arc<FeedFetcher>,
    pub ai_service: SharedAiService,
    pub ai_activity: AiActivityStore,
    /// Lazily-connected ChromaDB service (auto-reconnects).
    pub chroma_service: crate::chroma::ChromaHolder,
}

/**
 * Shared TypeScript types for the RSS Reader frontend.
 *
 * Mirrors the Rust models in src-tauri/src/models/ — keep in sync when a
 * field is added or removed on either side.
 */

export interface Subscription {
  id: number;
  url: string;
  title: string | null;
  website_url: string | null;
  rsshub_url: string | null;
  use_website: boolean;
  auto_classify: boolean;
  opml_attributes: string | null;
  created_at: string;
  updated_at: string;
}

/**
 * Full feed item. Returned by `get_item(id)` and by write commands
 * (`mark_read`, `save_tags`, …). List views use `FeedItemSummary` instead
 * (see `src/api/items.ts`).
 */
export interface FeedItem {
  id: number;
  subscription_id: number;
  guid: string | null;
  title: string;
  link: string | null;
  content: string | null;
  content_md: string | null;
  description: string | null;
  author: string | null;
  published_at: string | null;
  fetched_at: string;
  is_website_content: boolean;
  is_read: boolean;
  is_favorite: boolean;
  is_read_later: boolean;
  is_ignored: boolean;
  /** JSON array string, e.g. `["rust","programming"]`. */
  tags: string | null;
  translated_title: string | null;
  translated_content: string | null;
  translated_at: string | null;
}

/** Lightweight projection of `FeedItem` returned by list commands. */
export interface FeedItemSummary {
  id: number;
  subscription_id: number;
  title: string;
  link: string | null;
  description: string | null;
  author: string | null;
  published_at: string | null;
  fetched_at: string;
  is_website_content: boolean;
  is_read: boolean;
  is_favorite: boolean;
  is_read_later: boolean;
  is_ignored: boolean;
  tags: string | null;
  translated_title: string | null;
  has_translation: boolean;
  /** Subscription (source) title, joined in by list queries. */
  source_title: string | null;
  /** Subscription (source) URL — fallback when the feed has no title. */
  source_url: string | null;
}

export interface AiClassificationResponse {
  tags: string[];
}

export interface TagCatalogEntry {
  name: string;
  usage_count: number;
  aliases: string[];
}

// ---------------------------------------------------------------------------
// Topic navigation and the community map
// ---------------------------------------------------------------------------

export interface TopicCategory {
  id: number;
  label: string;
  definition: string;
  sort_order: number;
}

export interface TopicAssignment {
  tag_name: string;
  category_id: number | null;
  state: "assigned" | "context_only" | "review";
  source: "manual" | "ai";
}

export interface TopicWord {
  name: string;
  usage_count: number;
  category_id: number | null;
  state: "assigned" | "context_only" | "review" | "undecided";
  source: "manual" | "ai" | "none";
}

export interface TopicSuggestion {
  name: string;
  category_id: number | null;
  state: "assigned" | "context_only" | "review";
  reason: string;
}

export interface TopicSuggestionProgress {
  suggestions: TopicSuggestion[];
  remaining: number;
  considered: number;
  skipped: number;
}

export interface TopicWorkspace {
  categories: TopicCategory[];
  words: TopicWord[];
  expected_hash: string;
  undecided: number;
}

export interface TagOverviewNode {
  name: string;
  usage_count: number;
  category_id: number | null;
}

export interface TagOverviewEdge {
  source: string;
  target: string;
  shared_articles: number;
}

export interface TagOverviewCommunity {
  id: string;
  members: string[];
  summary_tags: string[];
  article_count: number;
  children: TagOverviewCommunity[];
}

export interface TagOverviewCoverage {
  total_items: number;
  tagged_items: number;
  unreadable_items: number;
}

export interface TagOverview {
  snapshot_id: string;
  scope_label: string;
  coverage: TagOverviewCoverage;
  nodes: TagOverviewNode[];
  edges: TagOverviewEdge[];
  communities: TagOverviewCommunity[];
  singletons: string[];
  blocked_excluded: number;
  structuring: "cooccurrence" | "semantic";
  warnings: string[];
}

/** Coverage of the LLM-written tag dictionary. */
export interface TagDictionaryStatus {
  tags: number;
  explained: number;
  indexed: number;
}

/** Progress of one explanation batch. */
export interface TagExplanationProgress {
  generated: number;
  remaining: number;
  tags: number;
}

/** Result of embedding the dictionary. */
export interface TagIndexResult {
  indexed: number;
  total: number;
}

/** How Auto-group proposes groups. */
export type GroupingMethod = "embedding" | "community";

/** Settings for snapping AI-generated tag names onto the catalog. */
export interface TagMatchConfig {
  enabled: boolean;
  similarity_threshold: number;
  /** `embedding` compares tag text; `community` uses article co-occurrence. */
  grouping_method: GroupingMethod;
  /** Minimum shared articles for a co-occurrence edge. */
  community_min_weight: number;
}

/** One AI-recommended article (manual "Picks" feature). */
export interface Recommendation {
  item_id: number;
  title: string;
  link: string | null;
  source: string;
  reason: string;
}

export interface SemanticSearchResult {
  item_id: number;
  title: string;
  url: string | null;
  author: string | null;
  /** Distance score (lower = more similar). */
  score: number;
}

export interface ChromaConfigResponse {
  host: string;
  port: number;
  collection_name: string;
  enabled: boolean;
}

export interface ChromaSyncReport {
  indexed: number;
  deleted: number;
  pages: number;
  duration_ms: number;
}

export interface ChromaInitializationResponse {
  config: ChromaConfigResponse;
  sync: ChromaSyncReport;
}

/** Live progress of an in-flight ChromaDB reindex/sync, polled by the UI. */
export interface SyncProgress {
  running: boolean;
  phase: string;
  total: number;
  done: number;
  pages: number;
  elapsed_ms: number;
}

/** Result of a website-Markdown backfill pass (see the QPS contract in Rust). */
export interface MarkdownBackfillReport {
  already_running: boolean;
  fetched: number;
  failed: number;
  queued_reindex: number;
  hosts_skipped: number;
  more_pending: boolean;
  duration_ms: number;
}

export interface AiActivitySnapshot {
  version: number;
  task_id: number | null;
  phase: "idle" | "waiting" | "running";
  kind: string;
  title: string | null;
  current: number | null;
  total: number | null;
  candidate_count: number | null;
  queue_length: number;
  started_at_ms: number | null;
}

export interface AiConfigResponse {
  /** Masked API key, e.g. `sk-****1234`. Empty when no key configured. */
  api_key: string;
  base_url: string;
  model: string;
  max_chars_per_segment: number | null;
}

export interface OpmlImportResult {
  created: Subscription[];
  skipped: { url: string; reason: string }[];
}

export type FeedFilter = "all" | "unread" | "favorites" | "read-later" | "today" | "tag";
export type SearchMode = "text" | "semantic";

/** Queue depth for background enrichment work. */
export interface JobStats {
  queued: number;
  running: number;
  failed: number;
  succeeded: number;
  recent_errors: string[];
  /** Queued jobs per kind (classify, website_markdown, chroma_upsert). */
  queued_by_kind?: Record<string, number>;
  /**
   * Machine-readable reasons why queued work cannot progress, e.g.
   * `ai_not_configured` or `semantic_search_disabled`.
   */
  blocked_reasons?: string[];
}

/** Index health shown in the Semantic DB settings panel. */
export interface IndexStatus {
  enabled: boolean;
  running: boolean;
  /** Stage of a running walk: "" | "deletes" | "upserts" | "walk" | "reconcile". */
  phase: string;
  /** Items with id <= indexed are known to be in the index. */
  indexed: number;
  /** Highest item id in the library. */
  total: number;
  queued_jobs: number;
  pending_upserts: number;
  pending_deletes: number;
  collection_name: string;
  collection_id: string | null;
  done: number;
  scan_total: number;
  elapsed_ms: number;
}

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
  category: string | null;
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
  category: string | null;
  translated_title: string | null;
  has_translation: boolean;
  /** Subscription (source) title, joined in by list queries. */
  source_title: string | null;
  /** Subscription (source) URL — fallback when the feed has no title. */
  source_url: string | null;
}

export interface AiClassificationResponse {
  tags: string[];
  category: string | null;
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

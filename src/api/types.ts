/**
 * TypeScript interfaces mirroring the backend's domain models.
 *
 * These are the public API contract. When backend models change,
 * update this file and TypeScript will flag every call site
 * that needs attention.
 */

/** A single RSS subscription (feed source). */
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

/** Input for creating a new subscription. */
export interface NewSubscriptionInput {
  url: string;
  title?: string;
  website_url?: string;
  rsshub_url?: string;
  use_website?: boolean;
}

/** Input for updating an existing subscription. */
export interface UpdateSubscriptionInput {
  title?: string;
  website_url?: string;
  use_website?: boolean;
  rsshub_url?: string;
}

/** A single feed item (article). */
export interface FeedItem {
  id: number;
  subscription_id: number;
  guid: string | null;
  title: string;
  link: string | null;
  content: string | null;
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
  translated_content: string | null;
  translated_at: string | null;
}

/** AI classification response. */
export interface AiClassificationResponse {
  tags: string[];
  category: string | null;
}

/** AI configuration as stored by the backend. */
export interface AiConfig {
  api_key: string;
  base_url: string;
  model: string;
}

/** OPML import result. */
export interface ImportResult {
  imported: number;
  skipped: number;
  errors: string[];
}

/** Progress events emitted by the backend during feed refresh. */
export interface FetchProgress {
  current: number;
  total: number;
  title?: string;
  url?: string;
  status?: "processing" | "completed";
}

/** Success event for a single feed during batch refresh. */
export interface FetchSuccess {
  title: string;
  count: number;
}

/** Error event for a single feed during batch refresh. */
export interface FetchError {
  title: string;
  error: string;
}

/** Translation progress emitted by the AI translation pipeline. */
export interface TranslationProgress {
  item_id: number;
  total: number;
  completed: number;
  html_chunk: string;
  is_complete: boolean;
  cached?: boolean;
  has_error?: boolean;
  error_messages?: string[];
  partial_content?: string;
}

/** Translation error event. */
export interface TranslationError {
  item_id: number;
  error: string;
  paragraph_index: number;
}

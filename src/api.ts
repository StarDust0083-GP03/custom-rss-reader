/**
 * Thin typed wrapper around the Tauri `invoke` boundary.
 *
 * Centralises every backend call so:
 * - TypeScript can verify parameter and return types end-to-end
 * - Error messages are normalised before reaching the UI
 * - We have a single place to add retries, telemetry, or mocking
 */

import { invoke } from "@tauri-apps/api/core";
import type {
  FeedItem,
  FeedItemSummary,
  Subscription,
  AiClassificationResponse,
  Recommendation,
  SemanticSearchResult,
  ChromaConfigResponse,
  ChromaInitializationResponse,
  AiConfigResponse,
  AiActivitySnapshot,
  OpmlImportResult,
  MarkdownBackfillReport,
} from "./types";

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

export class AppApiError extends Error {
  readonly source: string;
  constructor(message: string, source: string) {
    super(message);
    this.source = source;
    this.name = "AppApiError";
  }
}

async function call<T>(cmd: string, args?: Record<string, unknown>, source?: string): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    const msg = typeof e === "string" ? e : (e instanceof Error ? e.message : String(e));
    throw new AppApiError(msg, source ?? cmd);
  }
}

// ---------------------------------------------------------------------------
// Subscriptions
// ---------------------------------------------------------------------------

export const subscriptions = {
  list: () => call<Subscription[]>("list_subscriptions", undefined, "subscriptions.list"),
  get: (id: number) =>
    call<Subscription>("get_subscription", { id }, "subscriptions.get"),
  add: (input: {
    url: string;
    title?: string | null;
    website_url?: string | null;
    rsshub_url?: string | null;
    use_website?: boolean;
  }) =>
    call<Subscription>(
      "add_subscription",
      {
        url: input.url,
        title: input.title ?? null,
        websiteUrl: input.website_url ?? null,
        rsshubUrl: input.rsshub_url ?? null,
        useWebsite: input.use_website ?? false,
      },
      "subscriptions.add",
    ),
  update: (id: number, input: {
    title?: string | null;
    websiteUrl?: string | null;
    useWebsite?: boolean;
    rsshubUrl?: string | null;
  }) =>
    call<Subscription>("update_subscription", { id, ...input }, "subscriptions.update"),
  remove: (id: number) =>
    call<void>("remove_subscription", { id }, "subscriptions.remove"),
  toggleUseWebsite: (id: number) =>
    call<Subscription>("toggle_use_website", { id }, "subscriptions.toggleUseWebsite"),
  toggleAutoClassify: (id: number) =>
    call<Subscription>("toggle_auto_classify", { id }, "subscriptions.toggleAutoClassify"),
};

// ---------------------------------------------------------------------------
// Feed items
// ---------------------------------------------------------------------------

export const items = {
  list: (opts: {
    subscriptionId?: number | null;
    limit?: number;
    offset?: number;
  } = {}) =>
    call<FeedItemSummary[]>(
      "get_items",
      {
        subscriptionId: opts.subscriptionId ?? null,
        limit: opts.limit ?? 50,
        offset: opts.offset ?? 0,
      },
      "items.list",
    ),
  get: (id: number) => call<FeedItem>("get_item", { id }, "items.get"),
  resetContentMd: (id: number) => call<FeedItem>("reset_item_content_md", { id }, "items.resetContentMd"),
  search: (query: string, limit = 50) =>
    call<FeedItemSummary[]>("search_items", { query, limit }, "items.search"),
  byTag: (tag: string, subscriptionId?: number | null) =>
    call<FeedItemSummary[]>(
      "get_items_by_tag",
      { tag, subscriptionId: subscriptionId ?? null, limit: 50, offset: 0 },
      "items.byTag",
    ),
  bySubscription: (subscriptionId: number) =>
    call<FeedItemSummary[]>(
      "get_items_by_subscription",
      { subscriptionId, limit: 50, offset: 0 },
      "items.bySubscription",
    ),
  unread: (subscriptionId?: number | null) =>
    call<FeedItemSummary[]>(
      "get_unread",
      { subscriptionId: subscriptionId ?? null, limit: 50, offset: 0 },
      "items.unread",
    ),
  today: (subscriptionId?: number | null, unreadOnly = false) =>
    call<FeedItemSummary[]>(
      "get_today_items",
      { subscriptionId: subscriptionId ?? null, unreadOnly, limit: 50, offset: 0 },
      "items.today",
    ),
  favorites: (subscriptionId?: number | null) =>
    call<FeedItemSummary[]>(
      "get_favorites",
      { subscriptionId: subscriptionId ?? null, limit: 50, offset: 0 },
      "items.favorites",
    ),
  readLater: (subscriptionId?: number | null) =>
    call<FeedItemSummary[]>(
      "get_read_later",
      { subscriptionId: subscriptionId ?? null, limit: 50, offset: 0 },
      "items.readLater",
    ),
  tags: (subscriptionId?: number | null) =>
    call<string[]>("get_all_tags", { subscriptionId: subscriptionId ?? null }, "items.tags"),
  markRead: (id: number, isRead: boolean) =>
    call<FeedItem>("mark_item_read", { itemId: id, isRead }, "items.markRead"),
  markAllRead: (subscriptionId?: number | null) =>
    call<void>("mark_all_read", { subscriptionId: subscriptionId ?? null }, "items.markAllRead"),
  toggleFavorite: (id: number) => call<boolean>("toggle_favorite", { itemId: id }, "items.toggleFavorite"),
  toggleReadLater: (id: number) =>
    call<boolean>("toggle_read_later", { itemId: id }, "items.toggleReadLater"),
  saveTags: (id: number, tags: string[], category: string | null) =>
    call<FeedItem>(
      "save_item_tags",
      { itemId: id, tags, category },
      "items.saveTags",
    ),
};

// ---------------------------------------------------------------------------
// Feed fetching
// ---------------------------------------------------------------------------

export const feeds = {
  fetch: (subscriptionId: number) =>
    call<FeedItem[]>("fetch_feed", { subscriptionId }, "feeds.fetch"),
  fetchAll: () =>
    call<{
      total_subscriptions: number;
      success_count: number;
      total_items: number;
      new_items: number;
      errors: string[];
    }>("fetch_all_feeds", undefined, "feeds.fetchAll"),
  refresh: (ids: number[]) =>
    call<[number, [FeedItem[] | null, string | null]][]>(
      "refresh_subscriptions",
      { subscriptionIds: ids },
      "feeds.refresh",
    ),
  // Returns the website's Markdown (already html2md-converted by the
  // backend). Kept under the name `fetchWebsiteContent` for caller
  // compatibility — the underlying command is `fetch_website_markdown`.
  // Both display paths (host DOM text + iframe webview) consume this
  // Markdown; neither ever sees raw article HTML.
  fetchWebsiteContent: (url: string, itemId?: number) =>
    call<string>(
      "fetch_website_markdown",
      { url, itemId: itemId ?? null },
      "feeds.fetchWebsiteContent",
    ),
};

// ---------------------------------------------------------------------------
// OPML
// ---------------------------------------------------------------------------

export const opml = {
  import: (filePath: string) =>
    call<OpmlImportResult>("import_opml", { filePath }, "opml.import"),
  export: (filePath: string) =>
    call<void>("export_opml", { filePath }, "opml.export"),
};

// ---------------------------------------------------------------------------
// AI
// ---------------------------------------------------------------------------

export const ai = {
  getConfig: () => call<AiConfigResponse>("get_ai_config", undefined, "ai.getConfig"),
  getActivity: () => call<AiActivitySnapshot>("get_ai_activity", undefined, "ai.getActivity"),
  setConfig: (config: {
    /** Omit/blank to keep the existing key shown as a mask in the UI. */
    apiKey?: string;
    baseUrl?: string;
    model?: string;
    skipTest?: boolean;
    maxCharsPerSegment?: number | null;
  }) =>
    call<void>(
      "set_ai_config",
      {
        apiKey: config.apiKey ?? null,
        baseUrl: config.baseUrl ?? null,
        model: config.model ?? null,
        skipTest: config.skipTest ?? false,
        maxCharsPerSegment: config.maxCharsPerSegment ?? null,
      },
      "ai.setConfig",
    ),
  classify: (input: {
    title: string;
    description?: string | null;
    contentSnippet?: string | null;
    rssTitle?: string | null;
    existingTags?: string[] | null;
  }) =>
    call<AiClassificationResponse>(
      "classify_item",
      {
        title: input.title,
        description: input.description ?? null,
        contentSnippet: input.contentSnippet ?? null,
        rssTitle: input.rssTitle ?? null,
        existingTags: input.existingTags ?? null,
      },
      "ai.classify",
    ),
  /** Manually triggered: let the LLM pick the most worthwhile reads. */
  recommendReads: () =>
    call<Recommendation[]>("recommend_reads", undefined, "ai.recommendReads"),
  translateContent: (content: string, sourceLang: string, targetLang: string) =>
    call<string>(
      "translate_content_bilingual",
      { content, sourceLang, targetLang },
      "ai.translateContent",
    ),
  translateItem: (itemId: number) =>
    call<string>("translate_item_bilingual", { itemId }, "ai.translateItem"),
  translateItemStreaming: (itemId: number) =>
    call<string>(
      "translate_item_bilingual_streaming",
      { itemId },
      "ai.translateItemStreaming",
    ),
  translateHtmlStreaming: (itemId: number, content: string) =>
    call<string>(
      "translate_html_content_streaming",
      { itemId, content },
      "ai.translateHtmlStreaming",
    ),
};

// ---------------------------------------------------------------------------
// ChromaDB
// ---------------------------------------------------------------------------

export const chroma = {
  getConfig: () => call<ChromaConfigResponse>("get_chroma_config", undefined, "chroma.getConfig"),
  setConfig: (config: {
    host?: string;
    port?: number;
    collectionName?: string;
    enabled?: boolean;
  }) =>
    call<void>(
      "set_chroma_config",
      {
        host: config.host ?? null,
        port: config.port ?? null,
        collectionName: config.collectionName ?? null,
        enabled: config.enabled ?? null,
      },
      "chroma.setConfig",
    ),
  enableAndIndex: (config: { host: string; port: number; collectionName: string }) =>
    call<ChromaInitializationResponse>(
      "enable_chroma_and_index",
      config,
      "chroma.enableAndIndex",
    ),
  search: (query: string, limit = 10) =>
    call<SemanticSearchResult[]>("semantic_search", { query, limit }, "chroma.search"),
  findSimilar: (itemId: number, limit = 10) =>
    call<FeedItemSummary[]>("find_similar_items", { itemId, limit }, "chroma.findSimilar"),
  reindex: () => call<string>("reindex_chromadb", undefined, "chroma.reindex"),
  healthCheck: () => call<boolean>("chroma_health_check", undefined, "chroma.healthCheck"),
  backfillMarkdown: () =>
    call<MarkdownBackfillReport>("chroma_backfill_markdown", undefined, "chroma.backfillMarkdown"),
};

// ---------------------------------------------------------------------------
// Misc
// ---------------------------------------------------------------------------

export const misc = {
  openUrl: (url: string) =>
    call<void>("open_url_in_browser", { url }, "misc.openUrl").catch((e) => {
      console.error("openUrl failed:", e);
    }),
};

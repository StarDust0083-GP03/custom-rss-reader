/**
 * User actions: item flags (read/favorite/later), subscription CRUD,
 * refresh, search, OPML import/export, item selection + the "quick-abandon"
 * ignore timer, and the add-feed modal.
 */

import { invoke } from "@tauri-apps/api/core";
import { open, save, ask } from "@tauri-apps/plugin-dialog";
import { items as itemsApi, feeds as feedsApi, chroma as chromaApi, opml as opmlApi, subscriptions as subscriptionsApi, jobs as jobsApi } from "../api";
import type { FeedItem, FeedItemSummary, Subscription } from "../types";
import { state } from "../state";
import {
  renderItems,
  renderSubscriptions,
  renderItemDetail,
  loadItems,
  selectSubscription,
  updateToggleButtonStates,
  iframeManager,
} from "../ui/render";
import { setLoadingWithStatus, clearLoadingStatus, resetCounts, incrementError } from "../ui/status";
import { beginLibraryQuery, isCurrentLibraryQuery } from "../ui/library-query";
import { success as toastSuccess, error as toastError, info as toastInfo } from "../toast";
import { ensureMarkdownBackfill } from "./chroma";

const S = state;

// Refresh concurrency guard — the previous debounce let concurrent
// refresh_all_feeds calls through because the timer was only set AFTER the
// first call completed.
let refreshInProgress = false;

// Monotonic counter for `selectItem` so a slow full-item fetch for an old
// selection doesn't paint over a fresh one.
let selectItemSeq = 0;

// ---------------------------------------------------------------------------
// Item selection + ignore timer
// ---------------------------------------------------------------------------

/**
 * Reveal the reader pane. At ≤1024px CSS hides `.detail-panel` and only
 * `.visible` reopens it — selecting an article previously loaded it into a
 * pane that stayed at width 0.
 */
function showDetailPane() {
  document.querySelector(".detail-panel")?.classList.add("visible");
}

/** Return to the list at narrow widths (the detail pane is an overlay there). */
export function closeDetailPane() {
  document.querySelector(".detail-panel")?.classList.remove("visible");
  const card = S.selectedItem
    ? document.querySelector<HTMLElement>(`.item-card[data-id="${S.selectedItem.id}"]`)
    : null;
  card?.focus({ preventScroll: false });
}

export async function selectItem(item: FeedItemSummary) {
  const seq = ++selectItemSeq;
  // Cancel any in-flight webview load for a different article
  iframeManager.cancel();

  // Resolve to the full FeedItem; the summary may be a partial projection
  // (e.g., from `get_items`) and detail rendering needs `content`/`content_md`.
  let fullItem: FeedItem;
  try {
    fullItem = await itemsApi.get(item.id);
  } catch (e) {
    if (seq !== selectItemSeq) return;
    toastError("Failed to load item");
    return;
  }
  if (seq !== selectItemSeq) return;
  S.selectedItem = fullItem;

  const subId = fullItem.subscription_id;

  // Render mode is a transient, per-view preference that is DECOUPLED from
  // the subscription's `use_website` fetch setting (issue #8). Webview
  // always defaults to opening as rendered markdown; the in-memory map only
  // remembers the mode the user explicitly chose for this subscription
  // during the current session and is NEVER persisted to the backend.
  S.useWebView = S.webviewPerSubscription.get(subId) ?? false;

  updateToggleButtonStates();

  renderItems(true);
  renderItemDetail(fullItem);
  showDetailPane();

  if (!fullItem.is_read) {
    await markAsRead(fullItem.id, true);
  }
}

// "Ignored" is no longer derived from a timer. A one-second quiet period used
// to mark the article the user had just opened as ignored, which mutated
// library state the user never asked to change. Read/favorite/later remain
// explicit actions; engagement-derived ranking needs its own product rule and
// tests before it comes back.

// ---------------------------------------------------------------------------
// Item flags
// ---------------------------------------------------------------------------

// 标记已读/未读
export async function markAsRead(itemId: number, isRead: boolean) {
  try {
    await itemsApi.markRead(itemId, isRead);
  } catch (error) {
    console.error("Failed to mark as read:", error);
    return;
  }

  const item = S.currentItems.find(i => i.id === itemId);
  if (item) item.is_read = isRead;

  if (S.selectedItem?.id === itemId) {
    S.selectedItem.is_read = isRead;
    const markReadBtn = document.getElementById("mark-read-btn");
    if (markReadBtn) {
      markReadBtn.classList.toggle("active", isRead);
    }
  }

  // A read item must disappear immediately from an unread-only result. A
  // targeted class toggle leaves it visible and makes the filter lie.
  const unreadOnly = S.currentFilter === "unread"
    || (S.currentFilter === "today" && S.unreadFilterEnabled);
  if (unreadOnly && isRead) {
    await loadItems();
    return;
  }

  const card = document.querySelector(`.item-card[data-id="${itemId}"]`);
  if (card) card.classList.toggle("unread", !isRead);
}

// 切换收藏
export async function toggleFavorite(itemId: number) {
  try {
    const isFavorite = await invoke<boolean>("toggle_favorite", { itemId });
    applyItemFlag(itemId, "is_favorite", isFavorite);
    toastSuccess(isFavorite ? "Added to favorites" : "Removed from favorites");
  } catch (error) {
    console.error("Failed to toggle favorite:", error);
    toastError("Failed to toggle favorite");
  }
}

// 切换稍后读
export async function toggleReadLater(itemId: number) {
  try {
    const isReadLater = await invoke<boolean>("toggle_read_later", { itemId });
    applyItemFlag(itemId, "is_read_later", isReadLater);
    toastSuccess(isReadLater ? "Added to Read Later" : "Removed from Read Later");
  } catch (error) {
    console.error("Failed to toggle read later:", error);
    toastError("Failed to toggle read later");
  }
}

/**
 * Write a boolean flag to every copy of the item and re-evaluate whether it
 * still belongs to the active list.
 *
 * Flipping the flag only painted the card and the detail button, so an item
 * un-favorited while the Favorites filter was active stayed in the list (and
 * vice versa) until the next reload.
 */
function applyItemFlag(
  itemId: number,
  field: "is_favorite" | "is_read_later",
  value: boolean,
) {
  const item = S.currentItems.find(i => i.id === itemId);
  if (item) item[field] = value;
  if (S.selectedItem?.id === itemId) S.selectedItem[field] = value;

  if (!matchesActiveFilter(item ?? S.selectedItem, field, value)) {
    S.currentItems = S.currentItems.filter(i => i.id !== itemId);
  }

  renderItems(true);
  if (S.selectedItem?.id === itemId) {
    const button = document.getElementById(field === "is_favorite" ? "favorite-btn" : "read-later-btn");
    button?.classList.toggle("active", value);
  }
}

/** Does an item still belong in the list the user is currently looking at? */
function matchesActiveFilter(
  item: FeedItemSummary | FeedItem | null,
  field: "is_favorite" | "is_read_later",
  value: boolean,
): boolean {
  if (!item) return true;
  const flagFilters: Partial<Record<typeof S.currentFilter, typeof field>> = {
    favorites: "is_favorite",
    "read-later": "is_read_later",
  };
  if (flagFilters[S.currentFilter] !== field) return true;
  return value;
}

// 批量标记已读
export async function markAllAsRead() {
  try {
    await invoke("mark_all_read", { subscriptionId: S.currentSubscriptionId });
    const unreadOnly = S.currentFilter === "unread"
      || (S.currentFilter === "today" && S.unreadFilterEnabled);
    if (unreadOnly) {
      await loadItems();
    } else {
      S.currentItems.forEach(item => item.is_read = true);
      if (S.selectedItem && (!S.currentSubscriptionId || S.selectedItem.subscription_id === S.currentSubscriptionId)) {
        S.selectedItem.is_read = true;
      }
      renderItems();
    }
    toastSuccess("All items marked as read");
  } catch (error) {
    console.error("Failed to mark all as read:", error);
    toastError("Failed to mark all as read");
  }
}

// ---------------------------------------------------------------------------
// Subscriptions
// ---------------------------------------------------------------------------

// 添加订阅
export async function addSubscription(data: {
  url: string;
  title?: string;
  website_url?: string;
  rsshub_url?: string;
  use_website?: boolean;
}) {
  setLoadingWithStatus(data.url, "Adding subscription...");
  try {
    // Go through the typed adapter: it maps the form's snake_case fields to
    // the camelCase keys the Tauri command expects (websiteUrl, rsshubUrl,
    // useWebsite). Passing `data` straight to `invoke` silently dropped the
    // website URL and the RSSHub mirror.
    await subscriptionsApi.add(data);
    await loadSubscriptions();
    closeAddFeedModal();
    clearLoadingStatus(true, "Subscription added");
    toastSuccess("Subscription added successfully");
  } catch (error) {
    console.error("Failed to add subscription:", error);
    clearLoadingStatus(false, "Add failed");
    // Surface the backend's reason (duplicate URL, invalid URL, ...) —
    // a bare "failed" leaves the user guessing why "sometimes" it rejects.
    toastError(`Failed to add subscription: ${error}`);
  }
}

// 删除订阅
export async function deleteSubscription(id: number) {
  // 使用 Tauri 的原生对话框
  const confirmed = await ask("Are you sure you want to delete this subscription?", {
    title: "Confirm Delete",
    kind: "warning"
  });

  if (!confirmed) return;

  setLoadingWithStatus("", "Deleting subscription...");
  try {
    await invoke("remove_subscription", { id });
    await loadSubscriptions();
    if (S.currentSubscriptionId === id) {
      selectSubscription(null);
    }
    clearLoadingStatus(true, "Subscription deleted");
    toastSuccess("Subscription deleted");
  } catch (error) {
    console.error("Failed to delete subscription:", error);
    clearLoadingStatus(false, "Delete failed");
    toastError("Failed to delete subscription");
  }
}

// Toggle the subscription's persistent webview (use_website) setting.
// This is DISTINCT from the transient render-mode toggle in the detail
// view (issue #8): it changes whether content is fetched/cached from the
// website instead of RSS, and it persists to the backend. The detail-view
// Web View/Markdown button only switches how the current article is shown
// and never writes to this setting.
export async function toggleUseWebsite(id: number) {
  try {
    const updated = await invoke<Subscription>("toggle_use_website", { id });
    const index = S.subscriptions.findIndex(s => s.id === id);
    if (index !== -1) {
      S.subscriptions[index] = updated;
    }
    renderSubscriptions();
    toastSuccess(updated.use_website
      ? "Website content enabled for this subscription"
      : "Website content disabled for this subscription");
  } catch (error) {
    console.error("Failed to toggle use_website:", error);
    toastError("Failed to update subscription");
  }
}

// Toggle auto-classify for subscription
export async function toggleAutoClassify(id: number) {
  try {
    const updated = await invoke<Subscription>("toggle_auto_classify", { id });
    // Update local subscription list
    const index = S.subscriptions.findIndex(s => s.id === id);
    if (index !== -1) {
      S.subscriptions[index] = updated;
    }
    renderSubscriptions();
    toastSuccess(updated.auto_classify ? "Auto-classify enabled" : "Auto-classify disabled");
  } catch (error) {
    console.error("Failed to toggle auto-classify:", error);
    toastError("Failed to toggle auto-classify");
  }
}

// 加载订阅列表
export async function loadSubscriptions() {
  setLoadingWithStatus("", "Loading subscriptions...");
  try {
    S.subscriptions = await invoke<Subscription[]>("list_subscriptions");
    renderSubscriptions();
    clearLoadingStatus(true, "Ready");
  } catch (error) {
    console.error("Failed to load subscriptions:", error);
    clearLoadingStatus(false, "Load failed");
    toastError("Failed to load subscriptions");
  }
}

// 刷新所有订阅
export async function refreshAllFeeds() {
  // Concurrency guard — the previous "debounce" only ran AFTER the first
  // call completed, so a second click during the in-flight refresh still
  // went through. Reject re-entrant clicks here.
  if (refreshInProgress) {
    toastInfo("Refresh already running");
    return;
  }
  refreshInProgress = true;
  resetCounts();
  setLoadingWithStatus("", "Starting refresh...");

  try {
    const result = await feedsApi.fetchAll();
    await loadItems();
    // Enrichment (classification, website caching, indexing) is queued, not
    // awaited. Say so, and surface failures instead of letting them look like
    // a fully successful refresh.
    const stats = await jobsApi.stats().catch(() => null);
    const queued = stats ? stats.queued + stats.running : 0;
    const failed = stats?.failed ?? 0;
    const blocked = blockedReasonLabel(stats?.blocked_reasons ?? []);
    clearLoadingStatus(
      true,
      `Refresh complete: ${result.new_items} new item${result.new_items === 1 ? "" : "s"}` +
        (queued > 0 ? ` · ${queued} background task${queued === 1 ? "" : "s"} queued` : "") +
        (blocked ? ` · ${blocked}` : "") +
        (failed > 0 ? ` · ${failed} failed` : ""),
    );
    if (failed > 0) {
      toastInfo(`${failed} background task${failed === 1 ? "" : "s"} failed. Use ⋯ → Retry background work.`);
    } else if (blocked) {
      // Queued work waiting on configuration is not a failure, but it needs an
      // action from the reader — say which one.
      toastInfo(`Background work is waiting: ${blocked}.`);
    }
  } catch (error) {
    console.error("Failed to refresh feeds:", error);
    incrementError(`Failed to refresh`);
    clearLoadingStatus(false, "Refresh failed");
    toastError("Failed to refresh feeds");
  } finally {
    refreshInProgress = false;
  }
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

// 搜索
export async function searchItems(query: string) {
  const seq = beginLibraryQuery();
  const trimmedQuery = query.trim();
  if (!trimmedQuery) {
    await loadItems();
    return;
  }
  const searchT0 = performance.now();
  console.log(`[search] start mode=${S.searchMode} query="${query}"`);

  if (S.searchMode === "semantic" && S.chromaEnabled) {
    // Loading the semantic search view is the trigger to catch up on
    // website Markdown for history-imported articles (they get re-indexed
    // in the background). Fire-and-forget: this search isn't delayed.
    void ensureMarkdownBackfill();
    setLoadingWithStatus("", `Semantic search: "${query}"`);
    try {
      const results = await chromaApi.search(trimmedQuery, 50);
      if (!isCurrentLibraryQuery(seq)) return;
      // Real summaries from SQLite, in vector-ranking order: no synthesized
      // subscription id, read state, or source name.
      S.currentItems = results;
      renderItems();
      clearLoadingStatus(true, `Found ${results.length} semantic results`);
      console.log(
        `[search] done semantic in ${Math.round(performance.now() - searchT0)}ms hits=${results.length} query="${trimmedQuery}"`,
      );
    } catch (error) {
      if (!isCurrentLibraryQuery(seq)) return;
      console.error(
        `[search] FAILED semantic after ${Math.round(performance.now() - searchT0)}ms:`,
        error,
      );
      clearLoadingStatus(false, "Semantic search failed");
      toastError("Semantic search failed. Is ChromaDB running?");
    }
    return;
  }

  setLoadingWithStatus("", `Searching: "${query}"`);
  try {
    const items = await itemsApi.search(trimmedQuery, 100);
    if (!isCurrentLibraryQuery(seq)) return;
    S.currentItems = items;
    renderItems();
    clearLoadingStatus(true, `Found ${items.length} items`);
    console.log(
      `[search] done text in ${Math.round(performance.now() - searchT0)}ms hits=${items.length} query="${trimmedQuery}"`,
    );
  } catch (error) {
    if (!isCurrentLibraryQuery(seq)) return;
    console.error(`[search] FAILED text after ${Math.round(performance.now() - searchT0)}ms:`, error);
    clearLoadingStatus(false, "Search failed");
    toastError("Failed to search items");
  }
}

// ---------------------------------------------------------------------------
// OPML
// ---------------------------------------------------------------------------

// 导入 OPML
export async function importOpml() {
  try {
    const selected = await open({
      multiple: false,
      filters: [{ name: "OPML", extensions: ["opml", "xml"] }],
    });

    if (!selected) return;

    const filePath = typeof selected === "string" ? selected : (selected as { path?: string }).path ?? selected;
    setLoadingWithStatus(filePath, "Importing OPML...");
    const result = await opmlApi.import(filePath);

    clearLoadingStatus(true, "Import complete");
    toastSuccess(`Imported ${result.created.length} subscriptions. Skipped ${result.skipped.length}.`);
    if (result.skipped.length > 0) {
      console.warn("Import skipped:", result.skipped);
    }
    await loadSubscriptions();
  } catch (error) {
    console.error("Failed to import OPML:", error);
    clearLoadingStatus(false, "Import failed");
    toastError(`Failed to import OPML: ${error}`);
  }
}

// 导出 OPML
export async function exportOpml() {
  try {
    const filePath = await save({
      defaultPath: "subscriptions.opml",
      filters: [{ name: "OPML", extensions: ["opml"] }],
    });

    if (!filePath) return;

    setLoadingWithStatus(filePath, "Exporting OPML...");
    await invoke("export_opml", { filePath });
    clearLoadingStatus(true, "Export complete");
    toastSuccess("OPML exported successfully");
  } catch (error) {
    console.error("Failed to export OPML:", error);
    clearLoadingStatus(false, "Export failed");
    toastError(`Failed to export OPML: ${error}`);
  }
}

/** Turn the backend's machine-readable blockers into something readable. */
function blockedReasonLabel(reasons: string[]): string {
  const labels: Record<string, string> = {
    ai_not_configured: "AI is not configured",
    semantic_search_disabled: "semantic search is off",
  };
  return reasons.map((reason) => labels[reason] ?? reason).join(", ");
}

/** Put failed enrichment jobs back in the queue. */
export async function retryFailedJobs() {
  try {
    const retried = await jobsApi.retryFailed();
    const expired = await jobsApi.requeueExpired();
    if (retried + expired === 0) {
      toastInfo("No failed background work to retry");
      return;
    }
    toastSuccess(`Retrying ${retried + expired} background task${retried + expired === 1 ? "" : "s"}`);
  } catch (error) {
    console.error("Failed to retry background jobs:", error);
    toastError("Failed to retry background work");
  }
}

// ---------------------------------------------------------------------------
// Add-feed modal
// ---------------------------------------------------------------------------

export function openAddFeedModal() {
  const modal = document.getElementById("add-feed-modal");
  if (modal) modal.classList.add("visible");
}

export function closeAddFeedModal() {
  const modal = document.getElementById("add-feed-modal");
  if (modal) {
    modal.classList.remove("visible");
    const form = document.getElementById("add-feed-form") as HTMLFormElement;
    if (form) form.reset();
  }
}

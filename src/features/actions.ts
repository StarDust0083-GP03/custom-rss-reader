/**
 * User actions: item flags (read/favorite/later), subscription CRUD,
 * refresh, search, OPML import/export, item selection + the "quick-abandon"
 * ignore timer, and the add-feed modal.
 */

import { invoke } from "@tauri-apps/api/core";
import { open, save, ask } from "@tauri-apps/plugin-dialog";
import { items as itemsApi, feeds as feedsApi, chroma as chromaApi, opml as opmlApi } from "../api";
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
  invalidateLoadItems,
} from "../ui/render";
import { setLoadingWithStatus, clearLoadingStatus, resetCounts, incrementError } from "../ui/status";
import { success as toastSuccess, error as toastError, info as toastInfo } from "../toast";

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

  if (!fullItem.is_read) {
    markAsRead(fullItem.id, true);
  }

  setupIgnoreTimer(fullItem);
}

// Set up timer to detect if user quickly abandons the article
function setupIgnoreTimer(item: FeedItem) {
  // Clear any existing timer
  if (ignoreTimer !== null) {
    clearTimeout(ignoreTimer);
    ignoreTimer = null;
  }

  // Don't set up timer for already ignored items
  if (item.is_ignored) {
    return;
  }

  S.lastSelectedAt = Date.now();

  ignoreTimer = setTimeout(async () => {
    // Only mark as ignored if:
    // 1. The same item is still selected
    // 2. The item has not been marked as read (is_read should be true by now if user actually read it)
    // 3. Less than 1 second elapsed between selection and timer fire
    if (S.selectedItem?.id === item.id && !item.is_ignored) {
      const elapsed = Date.now() - S.lastSelectedAt;
      if (elapsed < 1000) {
        try {
          await invoke<boolean>("toggle_ignored", { itemId: item.id });
          item.is_ignored = true;
          renderItems(true);
          console.log(`[Ignore] Article "${item.title}" marked as ignored (read for ${elapsed}ms)`);
        } catch (error) {
          console.error('[Ignore] Failed to toggle ignored:', error);
        }
      }
    }
    ignoreTimer = null;
  }, 1000);
}

let ignoreTimer: ReturnType<typeof setTimeout> | null = null;

// Cancel ignore timer when user takes an action (scroll, translate, etc.)
export function cancelIgnoreTimer() {
  if (ignoreTimer !== null) {
    clearTimeout(ignoreTimer);
    ignoreTimer = null;
    console.log('[Ignore] Timer cancelled due to user action');
  }
}

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
  // Update local state
  const item = S.currentItems.find(i => i.id === itemId);
  if (item) item.is_read = isRead;

  // Targeted DOM update by data-id — no full re-render
  const card = document.querySelector(
    `.item-card[data-id="${itemId}"]`,
  );
  if (card) card.classList.toggle("unread", !isRead);

  if (S.selectedItem?.id === itemId) {
    S.selectedItem.is_read = isRead;
    const markReadBtn = document.getElementById("mark-read-btn");
    if (markReadBtn) {
      markReadBtn.classList.toggle("active", isRead);
    }
  }
}

// 切换收藏
export async function toggleFavorite(itemId: number) {
  try {
    const isFavorite = await invoke<boolean>("toggle_favorite", { itemId });
    // 更新本地状态
    const item = S.currentItems.find(i => i.id === itemId);
    if (item) {
      item.is_favorite = isFavorite;
      renderItems();
      if (S.selectedItem?.id === itemId) {
        S.selectedItem.is_favorite = isFavorite;
        const favoriteBtn = document.getElementById("favorite-btn");
        if (favoriteBtn) {
          favoriteBtn.classList.toggle("active", isFavorite);
        }
      }
      toastSuccess(isFavorite ? "Added to favorites" : "Removed from favorites");
    }
  } catch (error) {
    console.error("Failed to toggle favorite:", error);
    toastError("Failed to toggle favorite");
  }
}

// 切换稍后读
export async function toggleReadLater(itemId: number) {
  try {
    const isReadLater = await invoke<boolean>("toggle_read_later", { itemId });
    // 更新本地状态
    const item = S.currentItems.find(i => i.id === itemId);
    if (item) {
      item.is_read_later = isReadLater;
      renderItems();
      if (S.selectedItem?.id === itemId) {
        S.selectedItem.is_read_later = isReadLater;
        const readLaterBtn = document.getElementById("read-later-btn");
        if (readLaterBtn) {
          readLaterBtn.classList.toggle("active", isReadLater);
        }
      }
      toastSuccess(isReadLater ? "Added to Read Later" : "Removed from Read Later");
    }
  } catch (error) {
    console.error("Failed to toggle read later:", error);
    toastError("Failed to toggle read later");
  }
}

// 批量标记已读
export async function markAllAsRead() {
  try {
    await invoke("mark_all_read", { subscriptionId: S.currentSubscriptionId });
    S.currentItems.forEach(item => item.is_read = true);
    renderItems();
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
    await invoke("add_subscription", data);
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
    clearLoadingStatus(true, `Refresh complete: ${result.new_items} new items`);
    await loadItems();
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
  if (!query.trim()) {
    loadItems();
    return;
  }
  const searchT0 = performance.now();
  console.log(`[search] start mode=${S.searchMode} query="${query}"`);

  if (S.searchMode === "semantic" && S.chromaEnabled) {
    setLoadingWithStatus("", `Semantic search: "${query}"`);
    try {
      const results = await chromaApi.search(query, 50);
      // Drop any in-flight normal list load before installing search hits —
      // same staleness race find-similar guards against.
      invalidateLoadItems();
      // Synthesize a FeedItemSummary from each semantic hit. Fields the hit
      // doesn't carry (subscription_id, flags) are zeroed; navigation still
      // works because `selectItem` re-fetches the full item by its real id.
      S.currentItems = results.map(r => ({
        id: r.item_id,
        subscription_id: 0,
        title: r.title,
        link: r.url,
        description: null,
        author: r.author,
        published_at: null,
        fetched_at: "",
        is_website_content: false,
        is_read: false,
        is_favorite: false,
        is_read_later: false,
        is_ignored: false,
        tags: null,
        category: null,
        translated_title: null,
        has_translation: false,
        source_title: null,
        source_url: null,
      } as unknown as FeedItemSummary));
      renderItems();
      clearLoadingStatus(true, `Found ${results.length} semantic results`);
      console.log(
        `[search] done semantic in ${Math.round(performance.now() - searchT0)}ms hits=${results.length} query="${query}"`,
      );
    } catch (error) {
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
    const items = await itemsApi.search(query, 100);
    S.currentItems = items;
    renderItems();
    clearLoadingStatus(true, `Found ${items.length} items`);
    console.log(
      `[search] done text in ${Math.round(performance.now() - searchT0)}ms hits=${items.length} query="${query}"`,
    );
  } catch (error) {
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

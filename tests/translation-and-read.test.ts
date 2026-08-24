import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import type { FeedItem, FeedItemSummary } from "../src/types";
import { state } from "../src/state";

const renderMocks = vi.hoisted(() => ({
  renderItems: vi.fn(),
  renderItemDetail: vi.fn(),
  loadItems: vi.fn(() => Promise.resolve()),
}));

vi.mock("../src/ui/render", () => ({
  renderItems: renderMocks.renderItems,
  renderItemDetail: renderMocks.renderItemDetail,
  loadItems: renderMocks.loadItems,
  renderSubscriptions: vi.fn(),
  selectSubscription: vi.fn(),
  updateToggleButtonStates: vi.fn(),
  iframeManager: { cancel: vi.fn() },
}));

const { renderItems, renderItemDetail, loadItems } = renderMocks;

import {
  cancelTranslation,
  handleTranslateAction,
  toggleCachedTranslation,
} from "../src/features/ai";
import { markAsRead } from "../src/features/actions";
import { resetFiltersForSubscription } from "../src/ui/filter-state";

function item(overrides: Partial<FeedItem> = {}): FeedItem {
  return {
    id: 7,
    subscription_id: 3,
    guid: null,
    title: "A test article",
    link: null,
    content: "A sufficiently long article body.",
    content_md: null,
    description: null,
    author: null,
    published_at: null,
    fetched_at: "2026-01-01T00:00:00Z",
    is_website_content: false,
    is_read: true,
    is_favorite: false,
    is_read_later: false,
    is_ignored: false,
    tags: null,
    category: null,
    translated_title: null,
    translated_content: null,
    translated_at: null,
    ...overrides,
  };
}

function summary(overrides: Partial<FeedItemSummary> = {}): FeedItemSummary {
  return {
    id: 7,
    subscription_id: 3,
    title: "A test article",
    link: null,
    description: null,
    author: null,
    published_at: null,
    fetched_at: "2026-01-01T00:00:00Z",
    is_website_content: false,
    is_read: false,
    is_favorite: false,
    is_read_later: false,
    is_ignored: false,
    tags: null,
    category: null,
    translated_title: null,
    has_translation: false,
    source_title: "Test source",
    source_url: "https://example.com/feed",
    ...overrides,
  };
}

describe("translation and read-state regressions", () => {
  beforeEach(() => {
    state.currentFilter = "all";
    state.currentTagFilter = null;
    state.currentSubscriptionId = null;
    state.unreadFilterEnabled = false;
    state.currentItems = [];
    state.selectedItem = null;
    state.translationStateByItemId.clear();
    renderItems.mockClear();
    renderItemDetail.mockClear();
    loadItems.mockClear();

    mockIPC((cmd, args) => {
      if (cmd === "mark_item_read") {
        return { ...item(), ...(args as { isRead?: boolean }), is_read: (args as { isRead?: boolean }).isRead ?? false };
      }
      if (cmd === "translate_item_bilingual_streaming" || cmd === "translate_html_content_streaming") {
        return "translation-result";
      }
      return null;
    }, { shouldMockEvents: true });
  });

  afterEach(() => {
    state.translationStateByItemId.clear();
    clearMocks();
  });

  it("uses the official Tauri IPC mock and sends force=true for retranslation", async () => {
    const selected = item();
    state.selectedItem = selected;
    state.currentItems = [summary({ is_read: true })];

    const calls: Array<{ cmd: string; args: unknown }> = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === "mark_item_read") return selected;
      if (cmd === "translate_item_bilingual_streaming") return "ok";
      return null;
    }, { shouldMockEvents: true });

    await handleTranslateAction(selected, true);

    expect(calls).toContainEqual({
      cmd: "mark_item_read",
      args: { itemId: selected.id, isRead: false },
    });
    expect(calls).toContainEqual({
      cmd: "translate_item_bilingual_streaming",
      args: { itemId: selected.id, force: true },
    });
    expect(selected.translated_content).toBeNull();
  });

  it("removes the translating state immediately on cancellation", () => {
    const selected = item({ is_read: false });
    const controller = new AbortController();
    state.translationStateByItemId.set(selected.id, {
      useTranslation: true,
      inProgressContent: "partial",
      abortController: controller,
      hasError: false,
      errorMessage: null,
    });

    expect(cancelTranslation(selected)).toBe(true);
    expect(controller.signal.aborted).toBe(true);
    expect(state.translationStateByItemId.has(selected.id)).toBe(false);
    expect(renderItems).toHaveBeenCalledWith(true);
  });

  it("reloads an unread-only list after an item becomes read", async () => {
    state.currentFilter = "unread";
    state.currentItems = [summary({ is_read: false })];
    state.selectedItem = item({ is_read: false });

    await markAsRead(7, true);

    expect(loadItems).toHaveBeenCalledTimes(1);
    expect(state.currentItems[0]?.is_read).toBe(true);
  });

  it("treats blank translation content as a miss, not a cache hit", () => {
    const selected = item({ translated_content: "   " });
    expect(toggleCachedTranslation(selected)).toBe(false);
  });

  it("clears unread and tag filters when changing subscription context", () => {
    state.currentFilter = "unread";
    state.currentTagFilter = "rust";
    state.unreadFilterEnabled = true;

    resetFiltersForSubscription();

    expect(state.currentFilter).toBe("all");
    expect(state.currentTagFilter).toBeNull();
    expect(state.unreadFilterEnabled).toBe(false);
  });
});

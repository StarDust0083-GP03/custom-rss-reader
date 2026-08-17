/**
 * App bootstrap: event wiring and the two remaining view-level behaviours
 * (image zoom). All rendering lives in ui/, all actions in features/.
 */

import { invoke } from "@tauri-apps/api/core";
import type { Subscription } from "./types";
import { state } from "./state";
import { misc } from "./api";
import {
  renderItems,
  renderItemDetail,
  loadItems,
  renderSubscriptions,
} from "./ui/render";
import { setFilter, showTagSelector, updateFilterTabs } from "./ui/filters";
import {
  loadSubscriptions,
  refreshAllFeeds,
  searchItems,
  importOpml,
  exportOpml,
  openAddFeedModal,
  closeAddFeedModal,
  addSubscription,
  markAllAsRead,
  markAsRead,
  toggleFavorite,
  toggleReadLater,
  cancelIgnoreTimer,
} from "./features/actions";
import {
  translateItem,
  classifyItem,
  openAiSettingsModal,
  closeAiSettingsModal,
  openRecommendations,
  closeRecommendModal,
} from "./features/ai";
import {
  loadChromaConfig,
  findSimilarArticles,
  saveChromaConfig,
  openChromaSettingsModal,
  closeChromaSettingsModal,
  reindexChroma,
  chromaHealthCheck,
  toggleSearchMode,
} from "./features/chroma";
import { success as toastSuccess, error as toastError } from "./toast";

const S = state;

// 初始化
async function init() {
  await loadSubscriptions();
  await loadItems();

  // 拦截所有链接点击，在系统浏览器中打开。
  // 仅放行 http(s):，过滤 placeholder '#' 链接和 target=_self 的内部链接。
  document.addEventListener("click", (e) => {
    const target = e.target as HTMLElement | null;
    if (!target) return;
    const link = target.closest("a");
    if (!link) return;
    const href = link.getAttribute("href") || "";
    if (!href || href === "#" || link.target === "_self") return;
    if (!/^https?:\/\//i.test(href)) return;
    e.preventDefault();
    misc.openUrl(href);
  });

  // 事件监听
  document.getElementById("add-feed-btn")?.addEventListener("click", openAddFeedModal);
  document.getElementById("import-opml-btn")?.addEventListener("click", importOpml);
  document.getElementById("export-opml-btn")?.addEventListener("click", exportOpml);

  // 刷新所有订阅 - 防抖由 refreshAllFeeds 内部 in-progress 标志处理
  document.getElementById("refresh-all-btn")?.addEventListener("click", () => {
    refreshAllFeeds();
  });

  // 筛选标签
  document.querySelectorAll(".filter-tab").forEach(tab => {
    tab.addEventListener("click", () => {
      const filter = tab.getAttribute("data-filter") as typeof S.currentFilter;
      if (filter === "tag") {
        // For tag filter, show the in-DOM tag picker anchored to the tab
        showTagSelector(tab as HTMLElement);
      } else if (filter === "unread" && S.currentFilter === "today") {
        // Special case: clicking Unread while in Today mode toggles "Today + Unread"
        S.unreadFilterEnabled = !S.unreadFilterEnabled;
        updateFilterTabs();
        loadItems();
      } else {
        setFilter(filter);
      }
    });
  });

  // 标记所有已读
  document.getElementById("mark-all-read-btn")?.addEventListener("click", markAllAsRead);

  // AI 推荐阅读(手动触发)
  document.getElementById("recommend-btn")?.addEventListener("click", openRecommendations);
  const recommendModal = document.getElementById("recommend-modal");
  recommendModal?.querySelector(".close-modal")?.addEventListener("click", closeRecommendModal);
  recommendModal?.addEventListener("click", (e) => {
    if (e.target === e.currentTarget) {
      closeRecommendModal();
    }
  });

  // 搜索
  let searchTimeout: number;
  document.getElementById("search-input")?.addEventListener("input", (e) => {
    window.clearTimeout(searchTimeout);
    searchTimeout = window.setTimeout(() => {
      searchItems((e.target as HTMLInputElement).value);
    }, 300);
  });

  // 添加订阅表单
  document.getElementById("add-feed-form")?.addEventListener("submit", (e) => {
    e.preventDefault();
    const url = (document.getElementById("feed-url") as HTMLInputElement).value;
    const title = (document.getElementById("feed-title") as HTMLInputElement).value;
    const website_url = (document.getElementById("website-url") as HTMLInputElement).value;
    const rsshub_url = (document.getElementById("rsshub-url") as HTMLInputElement).value;
    const use_website = (document.getElementById("use-website") as HTMLInputElement).checked;

    addSubscription({ url, title: title || undefined, website_url: website_url || undefined, rsshub_url: rsshub_url || undefined, use_website });
  });

  document.querySelector(".cancel-btn")?.addEventListener("click", closeAddFeedModal);
  document.querySelector(".close-modal")?.addEventListener("click", closeAddFeedModal);

  // AI settings modal close buttons
  const aiModal = document.getElementById("ai-settings-modal");
  aiModal?.querySelector(".close-modal")?.addEventListener("click", closeAiSettingsModal);
  aiModal?.querySelector(".cancel-btn")?.addEventListener("click", closeAiSettingsModal);

  // ChromaDB settings
  document.getElementById("chroma-settings-btn")?.addEventListener("click", openChromaSettingsModal);
  const chromaModal = document.getElementById("chroma-settings-modal");
  chromaModal?.querySelector(".close-modal")?.addEventListener("click", closeChromaSettingsModal);
  chromaModal?.querySelector(".cancel-btn")?.addEventListener("click", closeChromaSettingsModal);
  document.getElementById("chroma-settings-form")?.addEventListener("submit", async (e) => {
    e.preventDefault();
    const host = (document.getElementById("chroma-host") as HTMLInputElement).value;
    const port = parseInt((document.getElementById("chroma-port") as HTMLInputElement).value) || 8000;
    const collection_name = (document.getElementById("chroma-collection") as HTMLInputElement).value;
    const enabled = (document.getElementById("chroma-enabled") as HTMLInputElement).checked;
    closeChromaSettingsModal();
    await saveChromaConfig({ host, port, collection_name, enabled });
  });
  document.getElementById("reindex-chroma-btn")?.addEventListener("click", reindexChroma);
  document.getElementById("chroma-health-btn")?.addEventListener("click", chromaHealthCheck);
  // Initialize search mode button
  loadChromaConfig();
  // Search mode toggle
  document.getElementById("search-mode-btn")?.addEventListener("click", toggleSearchMode);

  // 详情操作按钮
  document.getElementById("toggle-webview-btn")?.addEventListener("click", async () => {
    const selectedItem = S.selectedItem;
    if (selectedItem) {
      const subId = selectedItem.subscription_id;
      const subscription = S.subscriptions.find(s => s.id === subId);
      const currentUseWebsite = S.webviewPerSubscription.get(subId) ?? subscription?.use_website ?? false;

      // 先立即切换前端状态
      S.useWebView = !currentUseWebsite;
      S.webviewPerSubscription.set(subId, S.useWebView);

      // 更新按钮状态并重新渲染详情
      const btn = document.getElementById("toggle-webview-btn") as HTMLButtonElement;
      btn.textContent = S.useWebView ? "Markdown" : "Web View";
      renderItemDetail(selectedItem);

      // 然后在后台更新后端状态
      try {
        const updated = await invoke<Subscription>("toggle_use_website", { id: subId });
        // 使用后端返回的新状态来确保一致性
        S.useWebView = updated.use_website;
        S.webviewPerSubscription.set(subId, updated.use_website);
        // 更新本地订阅源列表
        const index = S.subscriptions.findIndex(s => s.id === subId);
        if (index !== -1) {
          S.subscriptions[index] = updated;
        }
        renderSubscriptions();
        // 如果后端返回的状态与本地不一致，需要重新渲染
        if (S.selectedItem) renderItemDetail(S.selectedItem);
      } catch (error) {
        console.error("Failed to update subscription:", error);
      }
    }
  });

  document.getElementById("mark-read-btn")?.addEventListener("click", () => {
    if (S.selectedItem) {
      markAsRead(S.selectedItem.id, !S.selectedItem.is_read);
    }
  });

  document.getElementById("favorite-btn")?.addEventListener("click", () => {
    if (S.selectedItem) {
      toggleFavorite(S.selectedItem.id);
    }
  });

  document.getElementById("read-later-btn")?.addEventListener("click", () => {
    if (S.selectedItem) {
      toggleReadLater(S.selectedItem.id);
    }
  });

  // AI 功能按钮
  document.getElementById("translate-btn")?.addEventListener("click", async () => {
    if (!S.selectedItem) return;

    // 获取当前文章的翻译状态
    const translationState = S.translationStateByItemId.get(S.selectedItem.id);
    const isTranslating = !!(translationState && translationState.abortController);
    const hasCache = S.selectedItem.translated_content !== null;

    // 1. 如果正在翻译，点击取消
    if (isTranslating) {
      translationState!.abortController!.abort();
      S.translationStateByItemId.delete(S.selectedItem.id);
      renderItemDetail(S.selectedItem);
      toastSuccess("Translation cancelled");
      return;
    }

    // 2. 如果有缓存，切换显示模式
    if (hasCache) {
      if (!translationState) {
        S.translationStateByItemId.set(S.selectedItem.id, {
          useTranslation: true,
          inProgressContent: null,
          abortController: null,
          hasError: false,
          errorMessage: null,
        });
        toastSuccess("Showing translation");
      } else {
        translationState.useTranslation = !translationState.useTranslation;
        toastSuccess(translationState.useTranslation ? "Showing translation" : "Showing original");
      }
      renderItemDetail(S.selectedItem);
      return;
    }

    // 3. 开始新的翻译
    // 标记为未读
    if (S.selectedItem.is_read) {
      S.selectedItem.is_read = false;
      await invoke("mark_item_read", { itemId: S.selectedItem.id, isRead: false });
      renderItems();
    }

    // 检查是否在 webview 模式下，如果是则先将网页内容保存为 content_md
    const useWebViewForItem = S.webviewPerSubscription.get(S.selectedItem.subscription_id) ?? false;

    if (useWebViewForItem && S.selectedItem.link && !S.selectedItem.content_md) {
      // 只在 content_md 尚未缓存时获取网页内容（避免重复请求）
      try {
        console.log('[Translate] Fetching website markdown');
        await invoke<string>("fetch_website_markdown", { url: S.selectedItem.link, itemId: S.selectedItem.id });
      } catch (error) {
        console.error('[Translate] Failed to fetch website content:', error);
      }
    }

    // 开始翻译 — 使用 translate_item_bilingual_streaming 从数据库读取
    // WebView 模式：优先使用 content_md；RSS 模式：优先使用 content
    await translateItem(S.selectedItem, undefined);
  });

  document.getElementById("classify-btn")?.addEventListener("click", async () => {
    if (!S.selectedItem) return;
    await classifyItem(S.selectedItem);
  });

  document.getElementById("similar-btn")?.addEventListener("click", findSimilarArticles);

  document.getElementById("ai-settings-btn")?.addEventListener("click", openAiSettingsModal);

  // 测试 AI 连接按钮 - 测试连接并保存
  document.getElementById("test-ai-btn")?.addEventListener("click", async () => {
    const apiKey = (document.getElementById("ai-api-key") as HTMLInputElement).value;
    const baseUrl = (document.getElementById("ai-base-url") as HTMLInputElement).value;
    const model = (document.getElementById("ai-model") as HTMLInputElement).value;
    const maxCharsPerSegment = parseInt((document.getElementById("ai-max-chars") as HTMLInputElement).value) || undefined;

    if (!apiKey) {
      toastError("Please enter an API key first");
      return;
    }

    const btn = document.getElementById("test-ai-btn") as HTMLButtonElement;
    const originalText = btn.textContent;
    btn.textContent = "Testing...";
    btn.disabled = true;

    try {
      // 测试连接（会同时保存配置）
      await invoke("set_ai_config", { apiKey, baseUrl: baseUrl || undefined, model: model || undefined, maxCharsPerSegment, skipTest: false });
      toastSuccess("API connection successful! Configuration saved.");
    } catch (error) {
      toastError(`Connection test failed: ${error}`);
    } finally {
      btn.textContent = originalText;
      btn.disabled = false;
    }
  });

  // AI 设置表单 - 直接保存（跳过测试）
  document.getElementById("ai-settings-form")?.addEventListener("submit", async (e) => {
    e.preventDefault();
    const apiKey = (document.getElementById("ai-api-key") as HTMLInputElement).value;
    const baseUrl = (document.getElementById("ai-base-url") as HTMLInputElement).value;
    const model = (document.getElementById("ai-model") as HTMLInputElement).value;
    const maxCharsPerSegment = parseInt((document.getElementById("ai-max-chars") as HTMLInputElement).value) || undefined;

    if (!apiKey) {
      toastError("Please enter an API key");
      return;
    }

    try {
      // 直接保存，跳过连接测试
      await invoke("set_ai_config", { apiKey, baseUrl: baseUrl || undefined, model: model || undefined, maxCharsPerSegment, skipTest: true });
      closeAiSettingsModal();
      toastSuccess("AI configuration saved (not tested)");
    } catch (error) {
      toastError(`Failed to save AI configuration: ${error}`);
    }
  });

  // Modal 点击外部关闭
  document.getElementById("add-feed-modal")?.addEventListener("click", (e) => {
    if (e.target === e.currentTarget) {
      closeAddFeedModal();
    }
  });

  document.getElementById("ai-settings-modal")?.addEventListener("click", (e) => {
    if (e.target === e.currentTarget) {
      closeAiSettingsModal();
    }
  });

  document.getElementById("chroma-settings-modal")?.addEventListener("click", (e) => {
    if (e.target === e.currentTarget) {
      closeChromaSettingsModal();
    }
  });

  // Cancel ignore timer for any user interaction (indicates engagement)
  cancelIgnoreTimer();

  // 初始化图片缩放功能
  initImageZoom();
  addZoomHintToImages();
}

// ==================== 图片缩放功能 ====================

/**
 * 初始化图片缩放功能
 */
function initImageZoom(): void {
  // 使用事件委托处理图片点击
  document.getElementById('detail-content')?.addEventListener('click', (e) => {
    // Cancel ignore timer on user interaction
    cancelIgnoreTimer();
    const target = e.target as HTMLElement;

    // 检查是否点击了图片
    if (target.tagName === 'IMG') {
      const img = target as HTMLImageElement;

      // 切换缩放状态
      if (img.classList.contains('zoomed')) {
        // 缩小图片
        img.classList.remove('zoomed');
        img.style.maxWidth = '400px';
        img.style.maxHeight = '300px';
      } else {
        // 放大图片
        img.classList.add('zoomed');
        img.style.maxWidth = '100%';
        img.style.maxHeight = 'none';
      }
    }
  });

  // 添加键盘支持：ESC 键退出缩放
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
      const zoomedImages = document.querySelectorAll('.detail-body img.zoomed');
      zoomedImages.forEach(img => {
        img.classList.remove('zoomed');
        (img as HTMLImageElement).style.maxWidth = '400px';
        (img as HTMLImageElement).style.maxHeight = '300px';
      });
    }
  });
}

/**
 * 为图片容器添加缩放提示
 */
function addZoomHintToImages(): void {
  const detailBody = document.querySelector('.detail-body');
  if (!detailBody) return;

  detailBody.querySelectorAll('img').forEach(img => {
    // 添加 title 提示用户可以点击缩放
    if (!img.title) {
      img.title = '点击图片查看大图，按 ESC 键退出';
    }

    // 添加 data 属性标记
    img.dataset.zoomable = 'true';
  });
}

window.addEventListener("DOMContentLoaded", init);

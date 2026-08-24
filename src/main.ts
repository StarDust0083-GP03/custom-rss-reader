/**
 * App bootstrap: event wiring and the two remaining view-level behaviours
 * (image zoom). All rendering lives in ui/, all actions in features/.
 */

import { invoke } from "@tauri-apps/api/core";
import { state } from "./state";
import { items as itemsApi, misc } from "./api";
import {
  renderItemDetail,
  loadItems,
  updateToggleButtonStates,
} from "./ui/render";
import { setFilter, showTagSelector, updateFilterTabs } from "./ui/filters";
import { attachMenu, closeMenu } from "./ui/menu";
import { initColumnLayout } from "./ui/layout";
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
  toggleUseWebsite,
  cancelIgnoreTimer,
} from "./features/actions";
import {
  handleTranslateAction,
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
import { initAiActivity } from "./ui/ai-activity";

const S = state;

// 初始化
async function init() {
  void initAiActivity();
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

  // 列宽恢复 + 折叠/拖动
  initColumnLayout(document.querySelector(".app-container") as HTMLElement);

  // 主题恢复 + 色板切换
  const currentTheme = localStorage.getItem("rss.theme") || "paper";
  applyTheme(currentTheme);
  document.querySelectorAll<HTMLButtonElement>(".theme-swatch").forEach((swatch) => {
    if (swatch.dataset.theme === currentTheme) swatch.classList.add("active");
    swatch.addEventListener("click", () => {
      const theme = swatch.dataset.theme || "paper";
      applyTheme(theme);
      localStorage.setItem("rss.theme", theme);
      document.querySelectorAll<HTMLButtonElement>(".theme-swatch").forEach(s =>
        s.classList.toggle("active", s === swatch));
      closeMenu();
      toastSuccess(`Theme: ${theme}`);
    });
  });

  // 事件监听
  document.getElementById("add-feed-btn")?.addEventListener("click", openAddFeedModal);

  // 溢出菜单(⋯)——把不常用的操作收进去
  const menuActionHandlers: Record<string, () => void> = {
    "import-opml": () => importOpml(),
    "export-opml": () => exportOpml(),
    "mark-all-read": () => markAllAsRead(),
    "chroma-settings": () => openChromaSettingsModal(),
    "classify": () => { if (S.selectedItem) classifyItem(S.selectedItem); },
    "similar": () => findSimilarArticles(),
    "ai-settings": () => openAiSettingsModal(),
  };
  const attach = (btnId: string, menuId: string) => {
    const btn = document.getElementById(btnId);
    const menu = document.getElementById(menuId);
    if (btn && menu) attachMenu({ button: btn, menu, onAction: (a) => menuActionHandlers[a]?.() });
  };
  attach("sidebar-menu-btn", "sidebar-menu");
  attach("items-menu-btn", "items-menu");
  attach("detail-menu-btn", "detail-menu");

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

  // 详情操作按钮.
  //
  // - Web View button: toggles the SUBSCRIPTION's persistent webview mode
  //   (use_website) — whether content is fetched from the website vs RSS
  //   text. This is a persistent, backend setting; the active state mirrors
  //   the subscription's use_website value.
  // - Markdown button: within webview mode, switches the transient render
  //   between Markdown and the live Web page (in-memory, per session only).
  //   In text mode (use_website off) it is disabled — only text is shown.
  document.getElementById("webview-btn")?.addEventListener("click", async () => {
    const item = S.selectedItem;
    if (!item) return;
    const subId = item.subscription_id;
    const wasOn = S.subscriptions.find(s => s.id === subId)?.use_website ?? false;

    await toggleUseWebsite(subId);
    const sub = S.subscriptions.find(s => s.id === subId);
    updateToggleButtonStates();
    if (!sub) return;

    const turningOn = !wasOn && sub.use_website;
    const turningOff = wasOn && !sub.use_website;

    try {
      if (turningOn && item.link) {
        // Enable webview mode: lazily fetch the website's content so the
        // Markdown view reflects the actual article page rather than the
        // RSS snippet. Mirrors the translate path — fetch_website_markdown
        // persists the website markdown into content_md (and flips the
        // is_website_content flag).
        const md = await invoke<string>("fetch_website_markdown", {
          url: item.link,
          itemId: item.id,
        });
        item.content_md = md;
        item.is_website_content = true;
      } else if (turningOff) {
        // Disable webview mode: revert the markdown back to the RSS
        // content so we don't keep showing the previously cached website
        // page. The backend re-derives content_md from the raw RSS
        // `content` and clears is_website_content.
        const updated = await itemsApi.resetContentMd(item.id);
        item.content_md = updated.content_md;
        item.is_website_content = updated.is_website_content;
      }
    } catch (error) {
      console.error("Failed to update content source:", error);
    }

    renderItemDetail(item);
  });

  document.getElementById("markdown-btn")?.addEventListener("click", () => {
    const item = S.selectedItem;
    if (!item) return;
    const subId = item.subscription_id;
    const subscription = S.subscriptions.find(s => s.id === subId);
    if (!subscription?.use_website) return; // text mode: nothing to toggle
    const next = !(S.webviewPerSubscription.get(subId) ?? false);
    S.useWebView = next;
    S.webviewPerSubscription.set(subId, next);
    updateToggleButtonStates();
    renderItemDetail(item);
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

  // AI 功能按钮。普通点击负责开始、取消或切换缓存；右键和长按强制重译。
  const translateBtn = document.getElementById("translate-btn");
  if (translateBtn) {
    let longPressTimer: number | null = null;
    let longPressTriggered = false;
    let suppressNextClick = false;

    const clearLongPress = () => {
      if (longPressTimer !== null) {
        window.clearTimeout(longPressTimer);
        longPressTimer = null;
      }
    };

    translateBtn.addEventListener("pointerdown", (event) => {
      if (event.button !== 0) return;
      clearLongPress();
      longPressTriggered = false;
      longPressTimer = window.setTimeout(() => {
        longPressTimer = null;
        longPressTriggered = true;
        suppressNextClick = true;
        if (S.selectedItem) void handleTranslateAction(S.selectedItem, true);
      }, 600);
    });
    translateBtn.addEventListener("pointerup", () => {
      if (longPressTriggered) suppressNextClick = true;
      clearLongPress();
    });
    translateBtn.addEventListener("pointercancel", clearLongPress);
    translateBtn.addEventListener("pointerleave", clearLongPress);
    translateBtn.addEventListener("contextmenu", (event) => {
      event.preventDefault();
      clearLongPress();
      if (S.selectedItem) void handleTranslateAction(S.selectedItem, true);
    });
    translateBtn.addEventListener("click", () => {
      if (suppressNextClick) {
        suppressNextClick = false;
        return;
      }
      if (S.selectedItem) void handleTranslateAction(S.selectedItem);
    });
  }

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

/** Apply a color theme by setting html[data-theme]. */
function applyTheme(theme: string): void {
  document.documentElement.dataset.theme = theme;
}
